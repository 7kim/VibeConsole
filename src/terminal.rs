use crate::{
    Control, Detector, Event, Result,
    mappings::{self, Mapping},
    midi,
};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Key {
    Up,
    Down,
    Tab,
    Left,
    Right,
    Enter,
    Space,
    Escape,
    Other,
    Character(u8),
    Backspace,
    InvalidInput,
}

fn csi_key(sequence: &[u8]) -> Key {
    let (code, modifier) = match sequence {
        [code @ (b'A' | b'B' | b'C' | b'D')] => (*code, 1),
        [
            b'1',
            b';',
            modifier @ b'2'..=b'8',
            code @ (b'A' | b'B' | b'C' | b'D'),
        ] => (*code, *modifier - b'0'),
        _ => {
            let Some((&final_byte, params)) = sequence.split_last() else {
                return Key::Other;
            };
            let Ok(params) = std::str::from_utf8(params) else {
                return Key::Other;
            };
            let mut fields = params.split(';');
            let (Some(code), Some(modifier)) = (fields.next(), fields.next()) else {
                return Key::Other;
            };
            let Ok(code) = code.parse::<u8>() else {
                return Key::Other;
            };
            let Ok(modifier) = modifier.parse::<u8>() else {
                return Key::Other;
            };
            if !(2..=8).contains(&modifier) {
                return Key::Other;
            }
            let code = match final_byte {
                b'u' if fields.next().is_none() => code,
                b'~' if code == 27 => match fields.next().and_then(|s| s.parse::<u8>().ok()) {
                    Some(code) if fields.next().is_none() => code,
                    _ => return Key::Other,
                },
                _ => return Key::Other,
            };
            return match code {
                13 => Key::Enter,
                27 => Key::Escape,
                32 => Key::Space,
                _ => Key::Other,
            };
        }
    };
    if !(1..=8).contains(&modifier) {
        return Key::Other;
    }
    match code {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        _ => Key::Other,
    }
}

trait Ui {
    fn draw(&mut self, text: &str) -> Result<()>;
    fn key(&mut self) -> Result<Option<Key>>;
    fn edit_file(&mut self, _path: &Path) -> Result<()> {
        Err("editor unavailable".into())
    }
    fn height(&self) -> usize {
        24
    }
    fn width(&self) -> usize {
        80
    }
    fn context(&self) -> u8 {
        0
    }
    fn program(&self) -> Option<(u8, crate::feedback::Payload)> {
        None
    }
    fn label(&mut self, control: Control, _current: &[Mapping]) -> Result<Option<String>> {
        Ok(Some(control.label()))
    }
    fn applications(&self) -> Result<Vec<(String, String)>> {
        crate::actions::catalog()
    }
    fn learn(&mut self) -> Result<Option<Control>>;
    fn learn_knob(&mut self) -> Result<Option<(Control, mappings::Input, String)>> {
        Err("knob learning unavailable".into())
    }
    fn learn_joystick(&mut self) -> Result<Option<(Control, mappings::Input, String)>> {
        Err("joystick learning unavailable".into())
    }
    fn learn_piano(&mut self) -> Result<Option<Control>> {
        Err("piano learning unavailable".into())
    }
    fn record(&mut self) -> Result<Vec<String>> {
        Err("recording unavailable; use Select keys".into())
    }
}

struct Terminal {
    context: u8,
    context_verified: bool,
    follow_programs: bool,
    follow_retry: Instant,
    program_names: Vec<String>,
    program_snapshot: Option<crate::feedback::Payload>,
    original: Option<libc::termios>,
    stop: Arc<AtomicBool>,
    signals: Vec<signal_hook::SigId>,
    input: Option<midi::Input>,
    detector: Detector,
    motion: crate::motion::Motion,
    capture: Option<Vec<[u8; 3]>>,
    output: Option<crate::keyboard::Output>,
    actions: crate::actions::Dispatcher,
    flow_pending: bool,
    /// Status-feedback pads: what the last press did, and the derived running state.
    phases: BTreeMap<Control, Phase>,
    probed: BTreeMap<Control, Option<bool>>,
    /// A background status probe in flight: tmux/gdbus/proc never block the session loop.
    probe: Option<std::sync::mpsc::Receiver<BTreeMap<Control, Option<bool>>>>,
    /// Return to running once the Flow capture is confirmed and the command list is shown.
    flow_resume: bool,
    /// (agent, tmux window, state), refreshed every `STATUS_EVERY`.
    agent_states: Vec<(String, String, &'static str)>,
    /// Agents' status-light settings, in `agent_states` order; None while Status lights is Off.
    light_agents: Option<Vec<mappings::Agent>>,
    /// Start of the status lights' once-a-second octave alternation.
    lights_since: Instant,
    status_at: Instant,
    flow_failed: bool,
    start_flow_enabled: bool,
    flow_missing: bool,
    controller: Option<crate::feedback::Controller>,
    resolver: Option<crate::feedback::Resolver>,
    idle: crate::feedback::Settings,
    path: PathBuf,
    run_mappings: Vec<Mapping>,
    wanted_run: bool,
    suspended: bool,
    retry: Option<Instant>,
    connecting: bool,
    reconnected: bool,
    feedback_allowed: bool,
    error: String,
    notice: String,
    last_press: Option<Control>,
    last_event: String,
    input_flash: Option<Instant>,
    note_flashes: BTreeMap<Control, Instant>,
    cc_values: BTreeMap<(u8, u8), u8>,
    bend: Option<(u8, u16)>,
    at_menu: bool,
    menu: usize,
    rendered: String,
    rendered_size: (usize, usize),
}

impl Terminal {
    fn name(&self, control: Control) -> String {
        let program = self.program();
        match self.run_mappings.iter().find(|m| m.control == control) {
            Some(mapping) => mapping.name(program.as_ref()),
            None => mappings::control_name(control, None, self.context, program.as_ref()),
        }
    }

    /// Tiled layout for `text` in the details pane (SPEC-UI layout).
    fn screen(&self, text: &str) -> crate::screen::Screen {
        let size = self.size();
        let state = self
            .output
            .as_ref()
            .map(|o| o.keyboard.status(|control| self.name(control)))
            .unwrap_or_else(|| format!("PAUSED | {} saved assignments", self.run_mappings.len()))
            + &agent_line(&self.agent_states);
        let state = if self.wanted_run {
            let label = if !self.context_verified {
                "RUNNING (detecting program; actions inactive)"
            } else if self.suspended {
                "RUNNING (menu open; actions suspended)"
            } else if self.retry.is_some() || self.connecting {
                "RUNNING (waiting for MIDI/readiness)"
            } else {
                "RUNNING"
            };
            state.replacen("PAUSED", label, 1)
        } else {
            state
        };
        let feedback = if !self.feedback_allowed
            && (self.idle.enabled() || self.run_mappings.iter().any(|m| m.feedback.enabled()))
        {
            "Saved feedback off: select and verify hardware with /prog-select."
        } else {
            ""
        };
        let connection = self
            .input
            .as_ref()
            .map(|input| format!("connected {}", input.port))
            .unwrap_or_else(|| "closed/disconnected".into());
        let mut state_lines = state.lines();
        let input_on = self.input.is_some() && self.input_light(Instant::now());
        let midi_status = if self.connecting {
            "preparing fresh snapshot"
        } else if self.retry.is_some() {
            "waiting/retrying"
        } else {
            &connection
        };
        let connection_line = if size.0 < 75 {
            format!(
                "{} | MIDI {} | Input {}",
                if self.flow_missing {
                    "Flow not installed"
                } else {
                    "VibeConsole"
                },
                crate::screen::bulb(self.input.is_some()),
                crate::screen::bulb(input_on)
            )
        } else {
            format!(
                "{} | MIDI {} {midi_status} | Input {}",
                if self.flow_missing {
                    "VibeConsole: Wispr Flow not installed"
                } else {
                    "VibeConsole"
                },
                crate::screen::bulb(self.input.is_some()),
                crate::screen::bulb(input_on)
            )
        };
        let program = if self.program_names.len() == 8 {
            self.program_names[self.context as usize].clone()
        } else {
            mappings::program_label(self.context)
        };
        let program = if self.context_verified {
            format!("{program} (hardware/RAM verified)")
        } else {
            format!("{program} (select hardware to verify)")
        };
        let header = format!(
            "{connection_line}\n{program} | {}",
            state_lines.next().unwrap_or_default()
        );
        let mut live = state_lines.map(String::from).collect::<Vec<_>>();
        let notice = if !self.error.is_empty() {
            format!("Error: {}", self.error.replace('\n', " "))
        } else if !self.notice.is_empty() {
            self.notice.replace('\n', " ")
        } else if !feedback.is_empty() {
            feedback.into()
        } else {
            self.controller
                .as_ref()
                .map(|c| format!("Feedback: {}", c.status))
                .unwrap_or_default()
        };
        let hints = if size.0 < 75 {
            "↑↓ · Enter · Space · Esc · Ctrl+C: quit"
        } else {
            "Up/Down: move | Enter: confirm | Space: select | Esc: back | Ctrl+C: quit"
        };
        let mut screen = crate::screen::Screen {
            header,
            commands: Vec::new(),
            anchor: self.menu,
            focus_commands: self.at_menu,
            details: text.into(),
            notice,
            live: Vec::new(),
            hints: hints.into(),
        };
        for (index, (name, command)) in command_rows(self.wanted_run).iter().enumerate() {
            screen.commands.push(match command {
                None => format!("── {name}"),
                Some(_) if index == self.menu && self.at_menu => format!("> {name}"),
                Some(_) if index == self.menu => format!("• {name}"),
                Some(_) => format!("  {name}"),
            });
        }
        let (width, height) = screen.live_size(size.0, size.1);
        live.extend(self.status_lines());
        live.extend(self.toggle_preview(width, height.saturating_sub(live.len())));
        screen.live = live;
        screen
    }

    fn reset_input_lights(&mut self) {
        self.detector = Detector::default();
        self.input_flash = None;
        self.note_flashes.clear();
        self.cc_values.clear();
        self.bend = None;
    }

    fn note_light(&self, control: Control, now: Instant) -> bool {
        self.detector.down.contains(&control)
            || self
                .note_flashes
                .get(&control)
                .is_some_and(|deadline| now < *deadline)
    }

    fn input_light(&self, now: Instant) -> bool {
        !self.detector.down.is_empty() || self.input_flash.is_some_and(|deadline| now < deadline)
    }

    fn observe_light(&mut self, event: Option<&Event>, now: Instant) {
        self.input_flash = Some(now + Duration::from_millis(200));
        if let Some(Event::Press(control)) = event {
            if control.message == crate::Message::Note {
                self.note_flashes
                    .insert(*control, now + Duration::from_millis(200));
            }
        }
    }

    /// `[●] Toggle N: <physical name> (<shortcut>)` per toggle mapping, in saved order (SPEC-UI story 5).
    fn toggle_preview(&self, width: usize, height: usize) -> Vec<String> {
        let toggles = self
            .run_mappings
            .iter()
            .filter(|mapping| mapping.behavior == mappings::Behavior::Toggle)
            .collect::<Vec<_>>();
        if toggles.is_empty() {
            return Vec::new();
        }
        let mut lines = Vec::new();
        let mut count = 0;
        for (index, mapping) in toggles.iter().enumerate() {
            let on = self
                .output
                .as_ref()
                .is_some_and(|output| output.keyboard.active(mapping.control));
            let line = crate::screen::toggle_line(
                index + 1,
                &self.name(mapping.control),
                &mapping.action_label(),
                on,
            );
            let rows = crate::screen::wrap(&line, width);
            if lines.len() + rows.len() >= height {
                break;
            }
            lines.extend(rows);
            count += 1;
        }
        // ponytail: the pane shows what fits; /list shows every mapping/state in large setups.
        let mut rows = vec![format!("Toggles {count}/{} · /list", toggles.len())];
        rows.extend(lines);
        rows
    }

    /// Bank A | Bank B panes over piano, knobs and joystick; one Unidentified pane when the
    /// program is unverified (SPEC-UI story 4). `live` gates bulbs on an open MIDI input.
    fn detection_view(&self, (columns, rows): (usize, usize), live: bool) -> String {
        use crate::screen::{bulb, fit, grid, group};
        let now = Instant::now();
        let width = self.screen("").details_size(columns, rows).0;
        let lit = |control| fit(bulb(live && self.note_light(control, now)), 6);
        let raw = |c: Control| format!("{} Note ch{} #{}", lit(c), c.channel, c.id);
        let value = |v: Option<String>| v.unwrap_or_else(|| "–".into());
        let mut rows = Vec::new();
        let Some((_, payload)) = self.program() else {
            let mut items = self
                .note_flashes
                .keys()
                .map(|c| raw(*c))
                .collect::<Vec<_>>();
            items.extend(
                self.cc_values
                    .iter()
                    .map(|((channel, id), v)| format!("CC ch{channel} #{id}: {v}")),
            );
            items.extend(
                self.bend
                    .map(|(channel, v)| format!("Bend ch{channel}: {v}")),
            );
            rows.extend(group(
                "Unidentified",
                &grid(&items, width.saturating_sub(4)),
                width,
            ));
            rows.push(fit(&self.last_event, width).trim_end().into());
            return rows.join("\n");
        };
        let pads = mappings::pad_controls(&payload);
        // As on the MPK Mini MK3: Pads 5–8 above Pads 1–4, each a name / bulb / note cell,
        // boxed where 10-column boxes fit (stacked, 160+) and a plain grid otherwise.
        let bank = |title, first: usize, width: usize| {
            let cell = width.saturating_sub(4) / 4;
            let boxed = cell >= 10;
            let pad = " ".repeat(cell.saturating_sub(10) / 2); // centres each box in its cell
            let edge =
                |left, right| fit(&format!("{pad}{left}{}{right}", "─".repeat(8)), cell).repeat(4);
            let mut lines = Vec::new();
            for row in [4, 0] {
                let cells = |text: &dyn Fn(usize) -> String| {
                    (row..row + 4)
                        .map(|n| {
                            let text = text(n);
                            fit(
                                &if boxed {
                                    format!("{pad}│ {} │", fit(&text, 6))
                                } else {
                                    text
                                },
                                cell,
                            )
                        })
                        .collect::<String>()
                };
                if boxed {
                    lines.push(edge('┌', '┐'));
                } else if row == 0 {
                    lines.push(String::new());
                }
                lines.push(cells(&|n| format!("Pad {}", n + 1)));
                lines.push(cells(&|n| match pads[first + n] {
                    Some(c) => lit(c),
                    None => bulb(false).into(),
                }));
                lines.push(cells(&|n| match pads[first + n] {
                    Some(c) => format!("#{}", c.id),
                    None => "none".into(),
                }));
                if boxed {
                    lines.push(edge('└', '┘'));
                }
            }
            group(title, &lines, width)
        };
        if columns >= 100 {
            let half = width / 2;
            let right = bank("Bank B", 8, width - half);
            for (a, b) in bank("Bank A", 0, half).iter().zip(&right) {
                rows.push(format!("{a}{b}"));
            }
        } else {
            rows.extend(bank("Bank A", 0, width));
            rows.extend(bank("Bank B", 8, width));
        }
        let knobs = (0..8).map(|k| payload[0x55 + 20 * k]).collect::<Vec<_>>();
        let mut items = knobs
            .iter()
            .enumerate()
            .map(|(k, id)| {
                let v = self.cc_values.iter().find(|((_, cc), _)| cc == id);
                format!(
                    "Knob {} · CC {id}: {}",
                    k + 1,
                    value(v.map(|(_, v)| v.to_string()))
                )
            })
            .collect::<Vec<_>>();
        items.push(format!(
            "Joystick X: {}",
            value(self.bend.map(|(_, v)| v.to_string()))
        ));
        items.extend(
            self.cc_values
                .iter()
                .filter(|((_, id), _)| !knobs.contains(id))
                .map(|((channel, id), v)| format!("CC ch{channel} #{id}: {v}")),
        );
        // Notes off the pad table, e.g. piano keys; mapped ones keep their saved physical name.
        items.extend(
            self.note_flashes
                .keys()
                .filter(|c| !pads.contains(&Some(**c)))
                .map(|c| {
                    if self.run_mappings.iter().any(|m| m.control == *c) {
                        format!("{} {}", lit(*c), self.name(*c))
                    } else {
                        raw(*c)
                    }
                }),
        );
        rows.extend(group(
            "Piano · Knobs · Joystick",
            &grid(&items, width.saturating_sub(4)),
            width,
        ));
        rows.push(fit(&self.last_event, width).trim_end().into());
        rows.join("\n")
    }

    fn detect(&mut self) -> Result<()> {
        self.suspend()?;
        if self.input.is_none() {
            self.input = Some(midi::Input::open()?);
            self.reset_input_lights();
        }
        loop {
            self.tick()?;
            let view = self.detection_view(self.size(), self.input.is_some());
            self.draw(&view)?;
            if self.input.is_none() {
                return Err(self.error.clone().into());
            }
            if let Some(Key::Enter | Key::Escape) = self.key()? {
                return Ok(());
            }
        }
    }

    fn size(&self) -> (usize, usize) {
        // SAFETY: ioctl writes the borrowed winsize structure.
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut size) };
        (
            if size.ws_col > 0 {
                size.ws_col as usize
            } else {
                80
            },
            if size.ws_row > 0 {
                size.ws_row as usize
            } else {
                24
            },
        )
    }
    fn open(path: &Path) -> Result<Self> {
        Self::open_mode(path, false)
    }
    fn open_mode(path: &Path, headless: bool) -> Result<Self> {
        // SAFETY: tcgetattr initializes termios on success; descriptors are borrowed.
        let original = if headless {
            None
        } else {
            Some(unsafe {
                let mut termios = std::mem::zeroed();
                if libc::tcgetattr(0, &mut termios) != 0 {
                    return Err("interactive mode needs a terminal on stdin".into());
                }
                termios
            })
        };
        let mut terminal = Self {
            context: 0,
            context_verified: true,
            follow_programs: false,
            follow_retry: Instant::now(),
            program_names: (1..=8)
                .map(|n| format!("Program {n} — name unavailable"))
                .collect(),
            program_snapshot: None,
            original,
            stop: Arc::new(AtomicBool::new(false)),
            signals: Vec::new(),
            input: None,
            detector: Detector::default(),
            motion: crate::motion::Motion::default(),
            capture: None,
            output: None,
            actions: crate::actions::Dispatcher::new(),
            flow_pending: false,
            flow_resume: false,
            phases: BTreeMap::new(),
            probed: BTreeMap::new(),
            probe: None,
            agent_states: Vec::new(),
            light_agents: None,
            lights_since: Instant::now(),
            status_at: Instant::now(),
            flow_failed: false,
            start_flow_enabled: true,
            flow_missing: false,
            controller: None,
            resolver: None,
            idle: crate::feedback::Settings::default(),
            path: path.to_path_buf(),
            run_mappings: Vec::new(),
            wanted_run: false,
            suspended: false,
            retry: None,
            connecting: false,
            reconnected: false,
            feedback_allowed: false,
            error: String::new(),
            notice: String::new(),
            last_press: None,
            last_event: String::new(),
            input_flash: None,
            note_flashes: BTreeMap::new(),
            cc_values: BTreeMap::new(),
            bend: None,
            at_menu: false,
            menu: 1,
            rendered: String::new(),
            rendered_size: (0, 0),
        };
        for signal in [
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGHUP,
            signal_hook::consts::SIGQUIT,
        ] {
            terminal.signals.push(signal_hook::flag::register(
                signal,
                Arc::clone(&terminal.stop),
            )?);
        }
        let Some(original) = original else {
            return Ok(terminal);
        };
        let mut raw = original;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL);
        raw.c_oflag &= !libc::OPOST;
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: valid termios and stdin descriptor; Drop restores the original.
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        write!(io::stdout(), "\x1b[?1049h\x1b[?25l")?;
        io::stdout().flush()?;
        Ok(terminal)
    }

    /// Re-derive running state for status pads; never stored across presses (SPEC). The
    /// probes run on a thread, one at a time; results land on a later tick.
    fn probe_status(&mut self) {
        if let Some(probe) = &self.probe {
            match probe.try_recv() {
                Ok(probed) => self.probed = probed,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
            }
            self.probe = None;
        }
        let pads = self
            .run_mappings
            .iter()
            .filter(|m| m.status)
            .map(|m| (m.control, m.action.clone()))
            .collect::<Vec<_>>();
        if pads.is_empty() {
            return;
        }
        // Sequences are answered here: the dispatcher knows what it has queued.
        let sequences = pads
            .iter()
            .filter(|(_, action)| matches!(action, crate::actions::Action::Sequence(_)))
            .map(|(pad, action)| (*pad, Some(self.actions.sequence_running(action))))
            .collect::<BTreeMap<_, _>>();
        let agents = self
            .agent_states
            .iter()
            .map(|(name, window, _)| (name.clone(), window.clone()))
            .collect::<Vec<_>>();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut probed = sequences;
            for (pad, action) in pads {
                probed
                    .entry(pad)
                    .or_insert_with(|| crate::actions::running(&action, &agents));
            }
            let _ = sender.send(probed);
        });
        self.probe = Some(receiver);
    }
    /// `Status <pad>: <word>` per status pad, for the live pane.
    fn status_lines(&self) -> Vec<String> {
        self.run_mappings
            .iter()
            .filter(|m| m.status)
            .map(|m| {
                let probed = self.probed.get(&m.control).copied().flatten();
                let word = status_word(self.phases.get(&m.control).copied(), probed);
                format!("Status {}: {word}", self.name(m.control))
            })
            .collect()
    }
    fn refresh_agent_states(&mut self) {
        let Ok(config) = mappings::load_config(&self.path) else {
            return;
        };
        self.light_agents = config.status_lights.then(|| config.agents.clone());
        self.agent_states = config
            .agents
            .into_iter()
            .map(|a| {
                let state = crate::status::read(&a.name);
                (a.name, a.window, state)
            })
            .collect();
        // A busy agent whose window is gone can never report again: back to idle.
        // ponytail: one blocking tmux call per refresh while an agent is busy (a few ms);
        // move it into the background probe if it ever stalls the loop.
        if self.agent_states.iter().any(|(_, _, s)| *s != "idle") {
            let gone = crate::status::vanished(&self.agent_states, crate::actions::window_alive)
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            for (name, _, state) in &mut self.agent_states {
                if gone.contains(name) {
                    match crate::status::write(name, "idle") {
                        Ok(()) => *state = "idle",
                        Err(error) => self.error = format!("Agent status not cleared: {error}"),
                    }
                }
            }
        }
        // Only ask tmux when something could be cleared.
        if self.agent_states.iter().any(|(_, _, s)| *s == "finished") {
            let attended =
                crate::status::attended(&self.agent_states, crate::status::tmux_active())
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
            self.clear_finished(&attended);
        }
    }
    /// Attending to an agent (its tmux window, or a mapping sent to it) clears its finished state.
    fn clear_finished(&mut self, agents: &[String]) {
        for (name, _, state) in &mut self.agent_states {
            if *state == "finished" && agents.contains(name) {
                match crate::status::write(name, "idle") {
                    Ok(()) => *state = "idle",
                    Err(error) => self.error = format!("Agent status not cleared: {error}"),
                }
            }
        }
    }
    /// Ticket 14: what the status lights should show now. None gives the controller back to the
    /// program's own feedback: Status lights Off, every agent idle, output not running on a
    /// verified program, a piano program (octave/arpeggiator change the notes), or the login
    /// service (`interactive` false).
    fn wanted_lights(&self, interactive: bool) -> Option<crate::feedback::Settings> {
        let agents = self.light_agents.as_ref()?;
        if !interactive
            || !self.wanted_run
            || self.suspended
            || !self.context_verified
            || !self.feedback_allowed
            || self
                .run_mappings
                .iter()
                .any(|m| m.input == mappings::Input::Piano)
        {
            return None;
        }
        let states = self
            .agent_states
            .iter()
            .zip(agents)
            .filter(|((name, _, _), agent)| *name == agent.name)
            .map(|((_, _, state), agent)| (*state, agent))
            .collect::<Vec<_>>();
        crate::status::lights(&states, self.lights_since.elapsed().as_secs())
    }
    fn status_lights(&mut self) -> Result<()> {
        let wanted = self.wanted_lights(self.original.is_some());
        match &mut self.controller {
            // No writes before the controller's snapshot is validated.
            Some(controller) if controller.baseline.is_some() => controller.light(wanted),
            _ => Ok(()),
        }
    }
    fn dialed(&mut self, dialed: crate::motion::Dialed) {
        match dialed {
            crate::motion::Dialed::Send(action, zone, count) => {
                self.notice = format!("Choice {}/{count}: {}", zone + 1, action.description());
                if let Err(error) = self.actions.submit(action, None) {
                    self.error = format!("Choice not sent: {error}");
                }
            }
            crate::motion::Dialed::Write(path, text) => {
                if let Err(error) = crate::actions::write_atomic(Path::new(&path), &text) {
                    self.error = format!("Value not written to {path}: {error}");
                } else {
                    self.notice = format!("Value {} → {path}", text.trim_end());
                }
            }
        }
    }
    fn tap(&mut self, keys: &[evdev::KeyCode]) -> Result<()> {
        self.output
            .as_mut()
            .ok_or("virtual keyboard unavailable")?
            .tap(keys)
    }
    fn cancel_actions(&mut self) {
        self.actions.cancel();
        if self.flow_pending {
            self.error = "Flow work cancelled; output remains paused. If Flow was already quit, reopen it manually after VibeConsole's keyboard is ready, test physical Shift, then Run.".into();
        }
        self.flow_pending = false;
        self.flow_resume = false;
        // Cancelled work never answers: let the derived state show instead of a stale "sent".
        self.phases.retain(|_, phase| *phase != Phase::Sent);
    }
    fn pause(&mut self) -> Result<()> {
        self.wanted_run = false;
        self.suspend()
    }
    fn reset_confirmed(&mut self, current: &mut Vec<Mapping>) -> Result<PathBuf> {
        self.pause()?;
        if let Some(mut controller) = self.controller.take() {
            self.resolver = None;
            controller.finish()?;
            self.input = None;
            self.reconnected = true;
        }
        let backup = reset_file(&self.path)?;
        self.output = None;
        self.run_mappings.clear();
        current.clear();
        self.idle = crate::feedback::Settings::default();
        self.start_flow_enabled = true;
        self.retry = None;
        self.connecting = false;
        self.error.clear();
        self.notice = format!("Settings reset. Backup: {}", backup.display());
        Ok(backup)
    }
    fn suspend(&mut self) -> Result<()> {
        self.suspended = true;
        self.motion = crate::motion::Motion::default();
        if let Some(output) = &mut self.output {
            if let Err(error) = output.pause() {
                self.output = None;
                self.cancel_actions();
                return Err(error);
            }
        }
        self.cancel_actions();
        let settings = if let Some(resolver) = &mut self.resolver {
            resolver.clear()?
        } else {
            None
        };
        if let (Some(controller), Some(settings)) = (&mut self.controller, settings) {
            controller.set(settings)?;
        }
        // Status lights end with the output: the program's feedback goes back on the controller.
        if let Some(controller) = &mut self.controller {
            controller.light(None)?;
        }
        Ok(())
    }

    fn release_all(&mut self) -> Result<()> {
        self.cancel_actions();
        self.motion = crate::motion::Motion::new(&self.run_mappings)?;
        if let Some(output) = &mut self.output {
            output.clear()?;
        }
        if let Some(resolver) = &mut self.resolver {
            if let Some(settings) = resolver.clear()? {
                if let Some(controller) = &mut self.controller {
                    controller.set(settings)?;
                }
            }
        }
        Ok(())
    }

    /// Start or repair Wispr Flow against VibeConsole's current keyboard.
    fn start_flow(&mut self) -> Result<()> {
        if self.output.is_none() {
            self.output = Some(crate::keyboard::Output::new(&self.run_mappings)?);
        }
        let node = self.output.as_ref().map(|o| o.node.clone());
        self.begin_flow(node)
    }
    /// Keys are released and output stays paused until capture is confirmed; a confirmed
    /// capture returns to running if the session was running (see `tick`).
    fn begin_flow(&mut self, node: Option<PathBuf>) -> Result<()> {
        let resume = self.wanted_run;
        self.pause()?;
        self.retry = None;
        self.connecting = false;
        self.actions.submit(crate::actions::Action::Flow, node)?;
        self.flow_pending = true;
        self.flow_resume = resume;
        self.flow_failed = false;
        self.error =
            "Flow operation pending; output stays paused. Esc/pause cancels pending work.".into();
        Ok(())
    }
    fn startup_flow(&mut self) -> Result<()> {
        if !self.start_flow_enabled {
            return Ok(());
        }
        if !crate::actions::flow_installed()? {
            self.flow_missing = true;
            self.notice = "Wispr Flow not installed; automatic startup skipped.".into();
            return Ok(());
        }
        self.start_flow()?;
        while self.flow_pending && !self.stop.load(Ordering::Relaxed) {
            self.tick()?;
            std::thread::sleep(Duration::from_millis(20));
        }
        if self.stop.load(Ordering::Relaxed) {
            return Err("session stopped during Flow startup".into());
        }
        if self.flow_failed {
            return Err(self.error.clone().into());
        }
        self.error.clear();
        self.notice =
            "Flow helper opened the current keyboard; dictation remains unverified.".into();
        Ok(())
    }
    fn transition(&mut self, pad: Control, active: bool) -> Result<()> {
        if let Some(action) = self
            .run_mappings
            .iter()
            .find(|m| m.control == pad)
            .map(|m| m.action.clone())
        {
            if action != crate::actions::Action::Shortcut {
                if active {
                    let agents = action
                        .agents()
                        .into_iter()
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    self.clear_finished(&agents);
                    let status = self
                        .run_mappings
                        .iter()
                        .any(|m| m.control == pad && m.status);
                    let submitted = if action.flow() {
                        self.start_flow()
                    } else {
                        self.actions.submit(action, None)
                    };
                    let knob = self.run_mappings.iter().any(|m| {
                        m.control == pad
                            && matches!(
                                m.input,
                                mappings::Input::Knob { .. } | mappings::Input::RelativeKnob { .. }
                            )
                    });
                    // A fast turn sends steps faster than the worker runs them; drop the
                    // extra steps instead of pausing. A rejected pad press still pauses.
                    if knob
                        && submitted
                            .as_ref()
                            .is_err_and(|e| e.to_string() == crate::actions::QUEUE_FULL)
                    {
                        self.notice =
                            "Knob turned faster than its action runs; extra steps skipped".into();
                        return Ok(());
                    }
                    if status {
                        let phase = if submitted.is_ok() {
                            Phase::Sent
                        } else {
                            Phase::Failed
                        };
                        self.phases.insert(pad, phase);
                    }
                    return submitted;
                }
                return Ok(());
            }
        }
        if let Some(resolver) = &mut self.resolver {
            if let Some(settings) = resolver.transition(pad, active)? {
                if let Some(controller) = &mut self.controller {
                    controller.set(settings)?;
                }
            }
        }
        Ok(())
    }

    fn controller(&mut self) -> Result<()> {
        if self.input.is_none() {
            self.input = Some(midi::Input::open()?);
        }
        if self.controller.is_none() {
            self.controller = Some(if self.follow_programs {
                crate::feedback::Controller::start_follow(
                    self.input.as_mut().unwrap(),
                    self.program_snapshot
                        .map(|snapshot| (self.context, snapshot)),
                )?
            } else {
                crate::feedback::Controller::start_verified(
                    self.input.as_mut().unwrap(),
                    self.program_snapshot,
                )?
            });
        }
        Ok(())
    }

    fn program_connection(&mut self) -> Result<()> {
        self.suspend()?;
        self.retry = None;
        self.connecting = false;
        self.resolver = None;
        if let Some(mut controller) = self.controller.take() {
            let restored = controller.finish();
            self.input = None;
            if let Err(error) = restored {
                self.context_verified = false;
                self.feedback_allowed = false;
                self.program_snapshot = None;
                return Err(error);
            }
        }
        if self.input.is_none() {
            self.input = Some(midi::Input::open()?);
        }
        self.reconnected = true;
        self.reset_input_lights();
        Ok(())
    }
    fn selected_idle(&mut self) -> Result<()> {
        if !self.feedback_enabled() {
            return Ok(());
        }
        self.controller()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            self.tick()?;
            if !self.context_verified {
                return Err(self.error.clone().into());
            }
            if let Some(baseline) = self.controller.as_ref().and_then(|c| c.baseline) {
                let mut resolver =
                    crate::feedback::Resolver::new(&self.run_mappings, baseline, self.idle)?;
                if let Some(settings) = resolver.clear()? {
                    self.controller
                        .as_mut()
                        .ok_or("Feedback unavailable")?
                        .set(settings)?;
                }
                self.resolver = Some(resolver);
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("Feedback baseline timed out; select program again".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn rename_program(&mut self) -> Result<()> {
        self.program_connection()?;
        let names = crate::feedback::names(self.input.as_mut().ok_or("MIDI disconnected")?)?;
        self.program_names = names.clone();
        let Some(context) = choose_at(
            self,
            "Rename stored program (current RAM stays unchanged)",
            &names,
            self.context as usize,
        )?
        else {
            return Ok(());
        };
        let Some(name) = program_name_prompt(self, &names[context])? else {
            return Ok(());
        };
        if choose(
            self,
            &format!(
                "Rename {} to {name:?}?\nOnly the stored name changes. Reselect via /prog-select to refresh current RAM.",
                names[context]
            ),
            &options(&["Cancel", "Rename"]),
        )? == Some(1)
        {
            crate::feedback::rename(
                self.input.as_mut().ok_or("MIDI disconnected")?,
                context as u8 + 1,
                &name,
            )?;
            self.program_names[context] = format!("Program {} — {name}", context + 1);
            self.error = "Stored name readback verified; current RAM unchanged; power-cycle persistence unmeasured.".into();
        }
        Ok(())
    }

    fn feedback_trial(&mut self) -> Result<()> {
        use crate::feedback::Settings;
        self.suspend()?;
        self.retry = None;
        self.connecting = false;
        self.resolver = None;
        if choose(
            self,
            "Program 1 hardware check. Changes musical settings, not independent LEDs.\nSelect the trial's Program 1/current note-mode setup on the controller.\nProgram queries cannot establish physical program selection.",
            &options(&["Cancel", "Use this intended Program 1 setup"]),
        )? != Some(1)
        {
            return Ok(());
        }
        self.error.clear();
        self.controller()?;
        let result = (|| {
            loop {
                self.draw(
                    "Reading original settings. No write until snapshot is validated. Esc cancels.",
                )?;
                if self.controller.as_ref().and_then(|c| c.baseline).is_some() {
                    break;
                }
                if self.controller.is_none() {
                    return Err(self.error.clone().into());
                }
                if self.key()? == Some(Key::Escape) {
                    return Ok(());
                }
            }
            let mut trial = Settings::default();
            let mut cursor = 0;
            loop {
                let baseline = self
                    .controller
                    .as_ref()
                    .ok_or_else(|| self.error.clone())?
                    .baseline
                    .unwrap();
                let Some(choice) = choose_at(
                    self,
                    &format!(
                        "Original: {baseline}\nUpdates are bounded and read back. Finish/Esc restores owned fields."
                    ),
                    &options(&[
                        "Set absolute octave",
                        "Set tempo (enables arpeggiator)",
                        "Set arpeggiator",
                        "Alternate octave (V5 status-light gate probe)",
                        "Finish and restore",
                    ]),
                    cursor,
                )?
                else {
                    break;
                };
                cursor = choice;
                let settings = match choice {
                    0 => {
                        let values = (-4..=4)
                            .map(|v| format!("Absolute octave {v:+}"))
                            .collect::<Vec<_>>();
                        let Some(index) = choose_at(
                            self,
                            "Octave (-4..+4)",
                            &values,
                            (trial.octave.unwrap_or(baseline.octave) + 4) as usize,
                        )?
                        else {
                            continue;
                        };
                        Settings {
                            octave: Some(index as i8 - 4),
                            ..trial
                        }
                    }
                    1 => {
                        let Some(bpm) = number(
                            self,
                            "Tempo BPM (1..300), arpeggiator On",
                            trial.tempo.unwrap_or(baseline.tempo) as i32,
                            1,
                            300,
                        )?
                        else {
                            continue;
                        };
                        Settings {
                            tempo: Some(bpm as u16),
                            arp: Some(true),
                            ..trial
                        }
                    }
                    2 => {
                        let Some(index) = choose_at(
                            self,
                            "Arpeggiator",
                            &options(&["Off", "On"]),
                            usize::from(trial.arp.unwrap_or(baseline.arp)),
                        )?
                        else {
                            continue;
                        };
                        Settings {
                            arp: Some(index == 1),
                            ..trial
                        }
                    }
                    3 => {
                        let Some(step) = number(self, "Alternate octave 0 ↔ this value", 1, -4, 4)?
                        else {
                            continue;
                        };
                        let Some(ms) = number(
                            self,
                            "Interval between changes (ms); runs 10 s",
                            1000,
                            100,
                            3000,
                        )?
                        else {
                            continue;
                        };
                        // Same bounded octave writes as above; Finish/Esc restores as usual.
                        let started = Instant::now();
                        let mut next = started;
                        let mut requests = 0;
                        while started.elapsed() < Duration::from_secs(10) {
                            if Instant::now() >= next {
                                requests += 1;
                                let octave = if requests % 2 == 1 { step as i8 } else { 0 };
                                trial = Settings {
                                    octave: Some(octave),
                                    ..trial
                                };
                                self.controller
                                    .as_mut()
                                    .ok_or_else(|| self.error.clone())?
                                    .set(trial)?;
                                next += Duration::from_millis(ms as u64);
                            }
                            let status = self
                                .controller
                                .as_ref()
                                .map_or(String::new(), |c| c.status.clone());
                            self.draw(&format!(
                                "Alternating octave 0 ↔ {step:+} every {ms} ms: {requests} requests in {:.1} s.\nCount the octave light changes; note any skipped or late ones. Esc stops.\nStatus: {status}",
                                started.elapsed().as_secs_f32()
                            ))?;
                            if self.key()? == Some(Key::Escape) {
                                break;
                            }
                        }
                        continue;
                    }
                    _ => break,
                };
                trial = settings;
                self.controller
                    .as_mut()
                    .ok_or_else(|| self.error.clone())?
                    .set(settings)?;
                choose(
                    self,
                    "Update requested; status shows transmission/read-back separately.\nObserve the indicators. Persistence across unplug/reboot remains unmeasured.",
                    &options(&["Continue"]),
                )?;
            }
            Ok(())
        })();
        let restored = if let Some(mut controller) = self.controller.take() {
            controller.finish()
        } else {
            Err(self.error.clone().into())
        };
        self.input = None;
        self.reset_input_lights();
        self.reconnected = self.output.is_some();
        match restored {
            Ok(message) => {
                if self.stop.load(Ordering::Relaxed) {
                    eprintln!("{message}");
                }
                self.error = message;
            }
            Err(error) => {
                self.error = error.to_string();
                return Err(error);
            }
        }
        result
    }

    fn abandon_program(&mut self) -> Result<()> {
        self.resolver = None;
        self.suspend()?; // release keys before joining any controller work
        self.context_verified = false;
        self.feedback_allowed = false;
        self.program_snapshot = None;
        if let Some(mut controller) = self.controller.take() {
            controller.abandon()?;
        }
        self.input = None;
        self.reset_input_lights();
        self.reconnected = true;
        self.connecting = false;
        self.retry = None;
        self.follow_retry = Instant::now();
        Ok(())
    }

    fn follow_controller(&mut self) -> Result<()> {
        if !self.follow_programs {
            return Ok(());
        }
        if self.at_menu && self.controller.is_none() && Instant::now() >= self.follow_retry {
            if let Err(error) = self.controller() {
                self.follow_retry = Instant::now() + Duration::from_secs(1);
                self.notice = format!("Waiting to detect controller program: {error}");
                return Ok(());
            }
        }
        let Some(controller) = &mut self.controller else {
            return Ok(());
        };
        controller.poll()?;
        let Some(observation) = controller.program.take() else {
            return Ok(());
        };
        if !self.at_menu
            && !self.context_verified
            && matches!(
                observation,
                crate::feedback::ProgramObservation::Detected { .. }
            )
        {
            // Initial detection can finish while a guidance dialog is open; adopt on return to Commands.
            controller.program = Some(observation);
            return Ok(());
        }
        self.observe_program(observation)
    }

    fn observe_program(&mut self, observation: crate::feedback::ProgramObservation) -> Result<()> {
        match observation {
            crate::feedback::ProgramObservation::Changed => {
                self.abandon_program()?;
                self.notice = "Controller settings changed; detecting current program. Old holds/toggles cleared.".into();
                if !self.at_menu {
                    return Err(
                        "Controller changed during operation; return to commands for detection"
                            .into(),
                    );
                }
            }
            crate::feedback::ProgramObservation::Detected {
                context,
                payload,
                names,
            } => {
                self.program_names = names;
                if self.context_verified
                    && context == Some(self.context)
                    && self.program_snapshot == Some(payload)
                {
                    return Ok(()); // worker restarted after an explicit selection/menu
                }
                if !self.at_menu {
                    self.abandon_program()?;
                    return Err("Controller changed during operation; configuration interrupted; return to commands for detection".into());
                }
                self.resolver = None;
                self.suspend()?;
                let Some(context) = context else {
                    self.context_verified = false;
                    self.feedback_allowed = false;
                    self.program_snapshot = None;
                    self.notice = if self.original.is_none() {
                        "Current RAM has no unique stored-program match; service output stays inactive."
                    } else {
                        "Current RAM has no unique stored-program match. Select /prog-select; output stays inactive."
                    }.into();
                    return Ok(());
                };
                self.context = context;
                self.program_snapshot = Some(payload);
                self.context_verified = true;
                self.feedback_allowed = true;
                self.error.clear();
                let config = mappings::load_config(&self.path)?;
                self.idle = config.idle_for(context);
                self.run_mappings = config
                    .mappings
                    .into_iter()
                    .filter(|m| m.context == context)
                    .collect();
                // Piano safety confirmation still needs an explicit Run; never prompt recursively from draw/tick.
                let piano = self
                    .run_mappings
                    .iter()
                    .any(|m| m.input == mappings::Input::Piano);
                let service_piano = piano && self.original.is_none();
                if piano && !service_piano {
                    self.wanted_run = false;
                }
                self.suspended = !self.wanted_run || service_piano;
                if self.output.is_none()
                    && self.wanted_run
                    && !self.suspended
                    && !self.run_mappings.is_empty()
                {
                    self.output = Some(crate::keyboard::Output::new(&self.run_mappings)?);
                }
                if let Some(output) = &mut self.output {
                    output.keyboard.replace(&self.run_mappings)?;
                }
                // Drop queued input collected before identifying this preset. Never replay it into new mappings.
                if let Some(input) = &self.input {
                    for _ in 0..1024 {
                        if input.next()?.is_none() {
                            break;
                        }
                    }
                    if input.next()?.is_some() {
                        return Err(
                            "Continuous MIDI during detection; stop controls and retry".into()
                        );
                    }
                }
                self.reset_input_lights();
                self.reconnected = true;
                self.connecting = true;
                self.prepare_connection()?;
                self.notice = format!(
                    "Detected Program {} from current hardware.{}",
                    context + 1,
                    if self
                        .run_mappings
                        .iter()
                        .any(|m| m.input == mappings::Input::Piano)
                    {
                        if self.original.is_none() {
                            " Piano mappings inactive: interactive Run confirmation required."
                        } else {
                            " Piano mappings: confirm arpeggiator Off and intended octave through Run."
                        }
                    } else {
                        " Pads fire on their next press."
                    }
                );
            }
        }
        Ok(())
    }

    fn tick(&mut self) -> Result<()> {
        if self.stop.load(Ordering::Relaxed) {
            self.pause()?;
            return Err("session stopped; terminal restored".into());
        }
        if let Err(error) = self.follow_controller() {
            self.abandon_program()?;
            self.pause()?;
            self.follow_retry = Instant::now() + Duration::from_secs(1);
            self.error = error.to_string();
            if !self.at_menu {
                return Err(error);
            }
        }
        if let Err(error) = self.retry_connection() {
            self.pause()?;
            self.retry = None;
            self.connecting = false;
            self.error = error.to_string();
        }
        if self.status_at.elapsed() >= STATUS_EVERY {
            self.status_at = Instant::now();
            self.refresh_agent_states();
            self.probe_status();
        }
        if let Err(error) = self.status_lights() {
            self.pause()?;
            self.error = format!("Status lights failed: {error}; paused");
        }
        // Prompt pastes and sequence shortcuts: the worker waits for each answer.
        for (keys, reply) in self.actions.tap_requests() {
            let _ = reply.send(self.tap(&keys).map_err(|e| e.to_string()));
        }
        while let Some((action, ok, notice)) = self.actions.notice() {
            for mapping in self
                .run_mappings
                .iter()
                .filter(|m| m.status && m.action == action)
            {
                let phase = if ok { Phase::Done } else { Phase::Failed };
                self.phases.insert(mapping.control, phase);
            }
            let notice = notice
                .chars()
                .filter(|c| !c.is_control())
                .take(2000)
                .collect();
            if action.flow() {
                self.flow_pending = false;
                self.flow_failed = !ok;
                self.flow_resume &= ok;
                if ok {
                    self.error.clear();
                    self.notice = notice;
                    continue;
                }
            }
            self.error = notice;
        }
        // Resume only on the command list: a menu that suspended output must find it paused.
        if self.flow_resume && !self.flow_pending && self.at_menu {
            self.flow_resume = false;
            let notice = std::mem::take(&mut self.notice);
            // Piano mappings need an explicit Run confirmation; never prompt from tick.
            if self
                .run_mappings
                .iter()
                .any(|m| m.input == mappings::Input::Piano)
            {
                self.notice = format!("{notice} Piano mappings: select Run to confirm.");
            } else if self.original.is_none() {
                // Service: resume as program detection does; run_mode's interactive-only
                // guards would keep every program but Program 1 paused.
                self.wanted_run = true;
                self.suspended = false;
                self.connecting = true;
                match self.prepare_connection() {
                    Ok(()) => self.notice = notice,
                    Err(error) => {
                        self.pause()?;
                        self.connecting = false;
                        self.error = format!("Flow ready, but output stays paused: {error}");
                    }
                }
            } else if let Err(error) = self.run_mode(&self.run_mappings.clone(), false) {
                self.error = format!("Flow ready, but output stays paused: {error}");
            } else {
                self.notice = notice;
            }
        }
        // Keep draining while menus are open so held pads stay tracked during configuration.
        for _ in 0..1024 {
            let Some(input) = &self.input else {
                break;
            };
            let message = match input.next() {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(error) => {
                    let reconfirm = true;
                    let resume = self.wanted_run;
                    let suspended = self.suspended || reconfirm;
                    if reconfirm {
                        self.context_verified = false;
                        self.feedback_allowed = false;
                        self.program_snapshot = None;
                    }
                    let cleanup = self.suspend();
                    self.suspended = suspended;
                    self.error = format!(
                        "{error}{}; hardware restoration unavailable on disconnected device",
                        cleanup
                            .err()
                            .map(|e| format!("; key release failed: {e}"))
                            .unwrap_or_default()
                    );
                    if let Some(mut controller) = self.controller.take() {
                        match controller.finish() {
                            Ok(text) => self.error.push_str(&format!("; {text}")),
                            Err(error) => self.error.push_str(&format!("; {error}")),
                        }
                    }
                    self.resolver = None;
                    self.input = None;
                    self.reset_input_lights();
                    self.last_press = None;
                    if self.output.is_some() {
                        self.wanted_run = resume;
                        self.reconnected = true;
                        self.connecting = false;
                        self.retry = if self.follow_programs {
                            None
                        } else {
                            Some(Instant::now() + Duration::from_secs(1))
                        };
                    }
                    self.follow_retry = Instant::now() + Duration::from_secs(1);
                    break;
                }
            };
            if let Some(capture) = &mut self.capture {
                if capture.len() >= 4096 {
                    return Err("input capture limit reached; retry shorter movement".into());
                }
                capture.push(message);
            }
            self.last_event = format!(
                "MIDI {:02X} {:02X} {:02X} | type {:02X}, channel {}",
                message[0],
                message[1],
                message[2],
                message[0] & 0xf0,
                (message[0] & 15) + 1
            );
            self.observe_light(None, Instant::now());
            let channel = (message[0] & 15) + 1;
            match message[0] & 0xf0 {
                0xb0 => {
                    self.cc_values.insert((channel, message[1]), message[2]);
                }
                0xe0 => {
                    self.bend = Some((channel, u16::from(message[1]) | u16::from(message[2]) << 7))
                }
                _ => {}
            }
            if message[0] & 0xf0 == 0xc0 && self.follow_programs {
                self.abandon_program()?;
                break;
            }
            if message[0] & 0xf0 == 0xc0 {
                self.resolver = None; // no idle update under an unverified preset transition
                self.suspend()?;
                self.context_verified = false;
                self.feedback_allowed = false;
                self.program_snapshot = None;
                self.retry = None;
                self.connecting = false;
                self.feedback_allowed = false;
                if let Some(mut controller) = self.controller.take() {
                    let restored = controller.abandon(); // never restore an old preset after an external switch
                    self.input = None;
                    self.reconnected = true;
                    self.error = restored.unwrap_or_else(|e| e.to_string());
                }
                self.error = "Program Change observed: preset transition unverified; select matching /prog-select to continue; no release inferred.".into();
            }
            for dialed in self.motion.dial(message, Instant::now()) {
                self.dialed(dialed);
            }
            let mut events = match self.motion.observe(message) {
                Ok(events) => events,
                Err(error) => {
                    self.pause()?;
                    self.error = error.to_string();
                    Vec::new()
                }
            };
            if let Some(event) = self.detector.observe(message) {
                events.push(event);
            }
            for event in events {
                self.observe_light(Some(&event), Instant::now());
                self.last_event = crate::describe_event(&event, |control| self.name(control));
                if let Event::Press(pad) = &event {
                    self.last_press = Some(*pad);
                }
                if let Some(output) = &mut self.output {
                    match output.observe(event) {
                        Ok(Some((pad, active))) => {
                            if let Err(error) = self.transition(pad, active) {
                                self.pause()?;
                                self.error = format!("Action/feedback rejected: {error}; paused");
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            self.error = format!("Keyboard output failed: {error}");
                            self.output = None;
                            self.pause()?;
                            break;
                        }
                    }
                }
            }
        }
        for dialed in self.motion.settle(Instant::now()) {
            self.dialed(dialed);
        }
        let fired = self.output.as_mut().map(|output| output.fire_due());
        match fired {
            Some(Ok(fired)) => {
                for (pad, active) in fired {
                    if let Err(error) = self.transition(pad, active) {
                        self.pause()?;
                        self.error = format!("Action/feedback rejected: {error}; paused");
                    }
                }
            }
            Some(Err(error)) => {
                self.error = format!("Keyboard output failed: {error}");
                self.output = None;
                self.pause()?;
            }
            None => {}
        }
        if let Some(controller) = &mut self.controller {
            if let Err(error) = controller.poll() {
                self.context_verified = false;
                self.feedback_allowed = false;
                self.program_snapshot = None;
                self.resolver = None;
                self.pause()?;
                self.error = error.to_string();
                if let Some(mut controller) = self.controller.take() {
                    match controller.finish() {
                        Ok(text) => self.error.push_str(&format!("; {text}")),
                        Err(error) => self.error.push_str(&format!("; {error}")),
                    }
                }
                self.input = None;
                self.reconnected = self.output.is_some();
                self.resolver = None;
                self.retry = None;
                self.connecting = false;
                self.reset_input_lights();
            }
        }
        if self.connecting {
            if let Err(error) = self.prepare_connection() {
                self.pause()?;
                self.retry = None;
                self.connecting = false;
                self.error = format!("Connection remains paused: {error}");
            }
        }
        Ok(())
    }

    fn feedback_enabled(&self) -> bool {
        self.context_verified
            && self.feedback_allowed
            && (self.idle.enabled() || self.run_mappings.iter().any(|m| m.feedback.enabled()))
    }
    fn run(&mut self, mappings: &[Mapping]) -> Result<()> {
        self.run_mode(mappings, true)
    }
    fn run_mode(&mut self, mappings: &[Mapping], confirm_feedback: bool) -> Result<()> {
        if self.flow_pending {
            return Err(
                "Flow operation still pending; wait or /pause to cancel before resuming".into(),
            );
        }
        if !self.context_verified {
            return Err(
                "unverified program transition; select /prog-select before resuming".into(),
            );
        }
        let config = mappings::load_config(&self.path)?;
        if config
            .mappings
            .iter()
            .filter(|m| m.context == self.context)
            .cloned()
            .collect::<Vec<_>>()
            != mappings
            || config.idle_for(self.context) != self.idle
        {
            return Err(
                "saved configuration changed outside this session; reload before running".into(),
            );
        }
        self.suspend()?;
        let feedback = self.idle.enabled() || mappings.iter().any(|m| m.feedback.enabled());
        if self.original.is_none()
            && (self.context != 0 || mappings.iter().any(|m| m.input != mappings::Input::Pad))
        {
            return Err("background startup supports the confirmed original pad setup only; configure other contexts/controls interactively".into());
        }
        if mappings.iter().any(|m| m.input == mappings::Input::Piano) {
            if feedback {
                return Err("piano mappings cannot run with octave/arpeggiator/tempo feedback or configured idle feedback; leave these unchanged".into());
            }
            if choose(
                self,
                "Piano mappings use emitted notes, not fixed physical positions.
Arpeggiator must be OFF. Transposing requires pausing and relearning.
Changing musical settings while running is unsupported.",
                &options(&["Cancel", "Confirm arpeggiator OFF and intended octave"]),
            )? != Some(1)
            {
                return Ok(());
            }
        }
        if feedback && !self.feedback_allowed && confirm_feedback {
            return Err(
                "Select /prog-select to verify hardware before enabling saved feedback".into(),
            );
        }
        if !feedback || !self.feedback_allowed {
            self.resolver = None;
            if !self.follow_programs
                && let Some(mut controller) = self.controller.take()
            {
                self.error = controller.finish()?;
                self.input = None;
                self.reset_input_lights();
                self.reconnected = true;
            }
        }
        self.run_mappings = mappings.to_vec();
        self.wanted_run = true;
        self.suspended = false;
        if mappings.is_empty() && !self.feedback_enabled() {
            self.motion = crate::motion::Motion::default();
            self.retry = None;
            self.connecting = false;
            if let Some(output) = &mut self.output {
                output.keyboard.replace(mappings)?;
                output.keyboard.resume();
            }
            return Ok(());
        }
        if self.output.is_none() {
            self.output = Some(crate::keyboard::Output::new(mappings)?);
        }
        self.retry = Some(Instant::now());
        self.tick()
    }

    fn prepare_connection(&mut self) -> Result<()> {
        let config = mappings::load_config(&self.path)?;
        if config
            .mappings
            .iter()
            .filter(|m| m.context == self.context)
            .cloned()
            .collect::<Vec<_>>()
            != self.run_mappings
            || config.idle_for(self.context) != self.idle
        {
            return Err("saved configuration changed/invalid; paused until reloaded".into());
        }
        let feedback = self.feedback_enabled();
        if feedback {
            let Some(baseline) = self.controller.as_ref().and_then(|c| c.baseline) else {
                return Ok(());
            };
            let mut resolver =
                crate::feedback::Resolver::new(&self.run_mappings, baseline, self.idle)?;
            if let Some(settings) = resolver.clear()? {
                self.controller.as_mut().unwrap().set(settings)?;
            }
            self.resolver = Some(resolver);
        } else {
            self.resolver = None;
        }
        let mut reconnected = false;
        if let Some(output) = &mut self.output {
            reconnected = output.keyboard.prepare_connection(
                &self.run_mappings,
                &self.detector.down,
                &mut self.reconnected,
            )?;
            if self.wanted_run && !self.suspended && self.context_verified {
                output.keyboard.resume();
            }
        }
        self.motion = crate::motion::Motion::new(&self.run_mappings)?;
        self.connecting = false;
        self.error = if !self.context_verified {
            "Reconnected but preset/input calibration is unverified; select /prog-select and confirm the matching preset before resuming.".into()
        } else {
            String::new()
        };
        self.notice = if reconnected {
            "MIDI reconnected; pads fire on their next press. A pad held through the reconnect fires once pressed again.".into()
        } else {
            String::new()
        };
        Ok(())
    }

    fn retry_connection(&mut self) -> Result<()> {
        let Some(deadline) = self.retry else {
            return Ok(());
        };
        if Instant::now() < deadline {
            return Ok(());
        }
        let config = mappings::load_config(&self.path)?;
        if config
            .mappings
            .iter()
            .filter(|m| m.context == self.context)
            .cloned()
            .collect::<Vec<_>>()
            != self.run_mappings
            || config.idle_for(self.context) != self.idle
        {
            return Err("saved configuration changed/invalid; paused until reloaded".into());
        }
        if self.input.is_none() {
            match midi::Input::open() {
                Ok(input) => {
                    self.input = Some(input);
                    self.reset_input_lights();
                }
                Err(error) => {
                    if !error.to_string().contains("not connected")
                        && !error.to_string().contains("unavailable; check lsusb")
                    {
                        return Err(error);
                    }
                    self.error = format!("Waiting for MIDI (retry every second): {error}");
                    self.retry = Some(Instant::now() + Duration::from_secs(1));
                    return Ok(());
                }
            }
        }
        self.retry = None;
        let feedback = self.feedback_enabled();
        if feedback {
            self.controller()?;
        }
        self.connecting = true;
        Ok(())
    }

    fn learn_motion(&mut self, knob: bool) -> Result<Option<(Control, mappings::Input, String)>> {
        self.suspend()?;
        let relative = if knob {
            match choose(self, "Knob mode", &options(&["Absolute", "Relative"]))? {
                Some(mode) => mode == 1,
                None => return Ok(None),
            }
        } else {
            false
        };
        if relative && self.program().is_none() {
            return Err("select a verified program before changing knob mode".into());
        }
        if self.input.is_none() {
            self.input = Some(midi::Input::open()?);
        }
        self.tick()?;
        let result = (|| -> Result<Option<(Control, mappings::Input, String)>> {
            loop {
                self.capture = Some(Vec::new());
                loop {
                    self.draw(&format!(
                        "{}\n{}\nEnter finishes observation; Esc cancels. Shortcuts remain paused.",
                        if knob {
                            if relative {
                                "Turn ONE knob both ways to identify it."
                            } else {
                                "Turn ONE knob slowly both ways to its stops."
                            }
                        } else {
                            "Move ONE joystick axis to BOTH endpoints, then let it center."
                        },
                        self.last_event
                    ))?;
                    if self.input.is_none() {
                        return Err(self.error.clone().into());
                    }
                    match self.key()? {
                        Some(Key::Escape) => return Ok(None),
                        Some(Key::Enter) => break,
                        _ => {}
                    }
                }
                let samples = self.capture.take().unwrap();
                if relative {
                    return self.learn_relative_knob(samples);
                }
                // A rejected movement is not an operation failure: nothing changed, so offer a retry.
                match calibrate_motion(self, samples, knob) {
                    Err(error) => {
                        if choose(
                            self,
                            &format!(
                                "Not saved: {error}\nNothing changed. Move the control again?"
                            ),
                            &options(&["Retry", "Cancel"]),
                        )? != Some(0)
                        {
                            return Ok(None);
                        }
                    }
                    learned => return learned,
                }
            }
        })();
        self.capture = None;
        result
    }

    fn learn_relative_knob(
        &mut self,
        samples: Vec<[u8; 3]>,
    ) -> Result<Option<(Control, mappings::Input, String)>> {
        let (context, payload) = self
            .program()
            .ok_or("select a verified program before changing knob mode")?;
        let (channel, cc) = relative_source(&samples)?;
        let knobs = (0..8)
            .filter(|k| payload[0x55 + 20 * k] == cc)
            .collect::<Vec<_>>();
        let [index] = knobs.as_slice() else {
            return Err("captured CC does not identify exactly one knob in this program".into());
        };
        let knob = *index as u8 + 1;
        let old_relative = payload[0x54 + 20 * index] == 1;
        if payload[0x54 + 20 * index] > 1 {
            return Err("unknown knob mode; no controller write attempted".into());
        }
        if choose(
            self,
            &format!(
                "Switch Program {} Knob {knob} (CC {cc}) to Relative?",
                context + 1
            ),
            &options(&["Cancel", "Switch and capture ticks"]),
        )? != Some(1)
        {
            return Ok(None);
        }
        self.program_connection()?;
        let next = crate::feedback::set_knob_mode(
            self.input.as_mut().ok_or("MIDI disconnected")?,
            context,
            knob,
            cc,
            true,
        )
        .map_err(|error| {
            self.context_verified = false;
            self.feedback_allowed = false;
            self.program_snapshot = None;
            error
        })?;
        self.program_snapshot = Some(next);
        while self
            .input
            .as_ref()
            .ok_or("MIDI disconnected")?
            .next()?
            .is_some()
        {}
        self.capture = Some(Vec::new());
        let learned = (|| -> Result<Option<(Control, mappings::Input, String)>> {
            loop {
                self.draw("Turn the same knob slowly and quickly in BOTH directions. Enter confirms capture; Esc restores its former mode.")?;
                match self.key()? {
                    Some(Key::Escape) => return Ok(None),
                    Some(Key::Enter) => break,
                    _ => {}
                }
            }
            let ticks = self.capture.take().unwrap();
            confirm_relative_ticks(&ticks, channel, cc)?;
            let Some(direction) = choose(
                self,
                "Physical direction for this assignment",
                &options(&[
                    "Clockwise / positive ticks",
                    "Counterclockwise / negative ticks",
                ]),
            )?
            else {
                return Ok(None);
            };
            Ok(Some((
                Control {
                    message: crate::Message::Cc,
                    channel,
                    id: cc,
                    direction: if direction == 0 { 1 } else { -1 },
                },
                mappings::Input::RelativeKnob { step: 1 },
                format!(
                    "Knob {knob} {}",
                    if direction == 0 {
                        "increase"
                    } else {
                        "decrease"
                    }
                ),
            )))
        })();
        self.capture = None;
        if !matches!(learned, Ok(Some(_))) && !old_relative {
            let restored = (|| -> Result<_> {
                self.program_connection()?;
                crate::feedback::set_knob_mode(
                    self.input.as_mut().ok_or("MIDI disconnected")?,
                    context,
                    knob,
                    cc,
                    false,
                )
            })();
            match restored {
                Ok(payload) => self.program_snapshot = Some(payload),
                Err(error) => {
                    self.context_verified = false;
                    self.feedback_allowed = false;
                    self.program_snapshot = None;
                    return Err(format!(
                        "relative capture ended; former knob mode restoration unverified: {error}"
                    )
                    .into());
                }
            }
        }
        learned
    }

    fn learn_note(&mut self, piano: bool) -> Result<Option<Control>> {
        self.suspend()?;
        if self.input.is_none() {
            self.input = Some(midi::Input::open()?);
        }
        self.tick()?;
        self.last_press = None;
        let mut learned = None;
        loop {
            self.draw(&format!("{}: press then release one control.\nTurn arpeggiator OFF for piano input; hold/release must be observed.\nCC/Program Change pad mode is unsupported. Esc cancels.\n{}", if piano { "Learn piano key" } else { "Learn pad in either bank" }, self.last_event))?;
            if self.input.is_none() {
                return Err(self.error.clone().into());
            }
            if learned.is_none() {
                learned = self.last_press.take();
            }
            if let Some(control) = learned {
                if !self.detector.down.contains(&control) {
                    if choose(
                        self,
                        &format!(
                            "Observed Note press/release: {} (channel {} note {}).\nConfirm this was a {}. Identical pad/piano messages cannot be separate assignments.",
                            mappings::control_name(
                                control,
                                piano.then_some(mappings::Input::Piano),
                                self.context,
                                self.program().as_ref()
                            ),
                            control.channel,
                            control.id,
                            if piano { "piano key" } else { "pad" }
                        ),
                        &options(&["Cancel", "Use observed control"]),
                    )? == Some(1)
                    {
                        return Ok(Some(control));
                    }
                    return Ok(None);
                }
            }
            if self.key()? == Some(Key::Escape) {
                return Ok(None);
            }
        }
    }

    fn byte(&self, timeout: i32) -> Result<Option<u8>> {
        if self.stop.load(Ordering::Relaxed) {
            return Err("session stopped; terminal restored".into());
        }
        let mut fd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd; no ownership transfer.
        let ready = unsafe { libc::poll(&mut fd, 1, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(error.into());
        }
        if ready == 0 {
            return Ok(None);
        }
        let mut byte = [0];
        // SAFETY: read one byte into valid storage. Stdin's buffered reader would
        // hide escape-sequence bytes from poll, so use the same fd for both.
        let count = unsafe { libc::read(0, byte.as_mut_ptr().cast(), 1) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(error.into());
        }
        if count == 0 {
            return Err("terminal input ended".into());
        }
        Ok(Some(byte[0]))
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Release before restoring terminal settings or waiting for MIDI shutdown.
        self.output = None;
        self.actions.cancel();
        self.controller = None;
        self.input = None;
        // SAFETY: the terminal snapshot came from tcgetattr.
        unsafe {
            if let Some(original) = &self.original {
                libc::tcsetattr(0, libc::TCSANOW, original);
            }
        }
        for signal in self.signals.drain(..) {
            signal_hook::low_level::unregister(signal);
        }
        if self.original.is_some() {
            let _ = write!(io::stdout(), "\x1b[0m\x1b[?25h\x1b[?1049l");
        }
        let _ = io::stdout().flush();
    }
}

impl Ui for Terminal {
    fn edit_file(&mut self, path: &Path) -> Result<()> {
        self.suspend()?;
        let original = self
            .original
            .ok_or("editor needs an interactive terminal")?;
        // SAFETY: restore the saved terminal state only while the editor owns it.
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &original) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        write!(io::stdout(), "\x1b[0m\x1b[?25h\x1b[?1049l")?;
        io::stdout().flush()?;
        let editor = std::env::var_os("VISUAL")
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var_os("EDITOR").filter(|v| !v.is_empty()))
            .unwrap_or_else(|| "nano".into());
        let editor = editor.to_string_lossy();
        let mut parts = editor.split_whitespace();
        let program = parts.next().ok_or("editor command is empty")?;
        let result = std::process::Command::new(program)
            .args(parts)
            .arg(path)
            .status();
        let mut raw = original;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL);
        raw.c_oflag &= !libc::OPOST;
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        write!(io::stdout(), "\x1b[?1049h\x1b[?25l\x1b[2J")?;
        io::stdout().flush()?;
        self.rendered.clear();
        if !result?.success() {
            return Err("editor exited unsuccessfully".into());
        }
        Ok(())
    }
    fn draw(&mut self, text: &str) -> Result<()> {
        self.tick()?;
        let size = self.size();
        let screen = self.screen(text).render(
            size.0,
            size.1,
            std::env::var_os("NO_COLOR").is_none()
                && std::env::var("TERM").as_deref() != Ok("dumb"),
        );
        if screen != self.rendered {
            if self.rendered_size != size {
                print!("\x1b[2J\x1b[H{screen}");
            } else {
                let mut previous = self.rendered.lines();
                for (index, row) in screen.lines().enumerate() {
                    if previous.next() != Some(row) {
                        print!("\x1b[{};1H{row}\x1b[K", index + 1);
                    }
                }
            }
            io::stdout().flush()?;
            self.rendered = screen;
            self.rendered_size = size;
        }
        Ok(())
    }
    // Screens size themselves as if the details pane were a whole terminal:
    // 5 columns of frame and 8 rows of chrome around their text.
    fn height(&self) -> usize {
        let size = self.size();
        self.screen("").details_size(size.0, size.1).1 + 8
    }
    fn width(&self) -> usize {
        let size = self.size();
        self.screen("").details_size(size.0, size.1).0 + 5
    }
    fn key(&mut self) -> Result<Option<Key>> {
        self.tick()?;
        let key = match self.byte(50)? {
            Some(b'\n' | b'\r') => Some(Key::Enter),
            Some(b' ') => Some(Key::Space),
            Some(b'\t') => Some(Key::Tab),
            Some(27) => Some(match self.byte(30)? {
                Some(b'[') => {
                    let mut sequence = Vec::new();
                    for _ in 0..16 {
                        let Some(byte) = self.byte(30)? else { break };
                        sequence.push(byte);
                        if byte.is_ascii_alphabetic() || byte == b'~' {
                            break;
                        }
                    }
                    csi_key(&sequence)
                }
                Some(b'\r' | b'\n') => Key::Enter,
                Some(b' ') => Key::Space,
                Some(27) | None => Key::Escape,
                Some(_) => Key::Escape,
            }),
            Some(8 | 127) => Some(Key::Backspace),
            Some(byte @ 33..=126) => Some(Key::Character(byte)),
            Some(_) => Some(Key::InvalidInput),
            None => None,
        };
        // Never confirm an action whose menu cannot be displayed.
        Ok(
            if (self.size().0 < 40 || self.size().1 < 16) && key != Some(Key::Escape) {
                None
            } else {
                key
            },
        )
    }
    fn record(&mut self) -> Result<Vec<String>> {
        self.suspend()?;
        self.draw("Recording for up to 60 seconds. Release all keys, then press and release ONE chord.\nPhysical shortcuts also reach the desktop. Escape is recordable; Ctrl+C quits (select it via menus).\nCapture closes before Use / Record again / Cancel.")?;
        let mut capture = crate::recording::Capture::open()?;
        let result = (|| {
            loop {
                self.tick()?;
                if let Some(keys) = capture.poll()? {
                    return Ok(keys);
                }
                self.byte(20)?; // discard terminal text; physical events supply identities
            }
        })();
        drop(capture);
        // Discard pass-through bytes on success, cancellation, and capture failures.
        // SAFETY: borrowed terminal descriptor, flush input only.
        unsafe {
            libc::tcflush(0, libc::TCIFLUSH);
        }
        result
    }
    fn context(&self) -> u8 {
        self.context
    }
    fn program(&self) -> Option<(u8, crate::feedback::Payload)> {
        self.program_snapshot
            .filter(|_| self.context_verified)
            .map(|payload| (self.context, payload))
    }
    fn label(&mut self, control: Control, current: &[Mapping]) -> Result<Option<String>> {
        let program = self.program();
        if let Ok(name) = mappings::physical_name(
            control,
            Some(mappings::Input::Pad),
            self.context,
            program.as_ref(),
        ) {
            if !current.iter().any(|mapping| {
                mapping.context == self.context
                    && mapping.control != control
                    && mapping.label == name
            }) {
                return Ok(Some(name));
            }
        }
        let labels = pad_labels(control, current, self.context);
        Ok(choose(
            self,
            "Confirm physical pad label (bank is a label, not live bank state).
Labels assigned to other controls in this program are omitted.",
            &labels,
        )?
        .map(|i| labels[i].clone()))
    }
    fn learn_knob(&mut self) -> Result<Option<(Control, mappings::Input, String)>> {
        self.learn_motion(true)
    }
    fn learn_joystick(&mut self) -> Result<Option<(Control, mappings::Input, String)>> {
        self.learn_motion(false)
    }
    fn learn(&mut self) -> Result<Option<Control>> {
        self.learn_note(false)
    }
    fn learn_piano(&mut self) -> Result<Option<Control>> {
        self.learn_note(true)
    }
}

fn choose(ui: &mut impl Ui, title: &str, options: &[String]) -> Result<Option<usize>> {
    choose_at(ui, title, options, 0)
}

fn refresh_idle_after_menu(command: usize, was_running: bool, verified: bool) -> bool {
    !was_running && verified && matches!(command, 9 | 10 | 14)
}

fn program_name_prompt(ui: &mut impl Ui, label: &str) -> Result<Option<String>> {
    let mut name = String::new();
    loop {
        ui.draw(&format!("{label}\nNew name: {name}\n1–16 printable ASCII characters. Backspace: delete; Enter: review; Esc: cancel."))?;
        match ui.key()? {
            Some(Key::Escape) => return Ok(None),
            Some(Key::Enter) => {
                if name.trim().is_empty() {
                    return Err("Name cannot be blank; nothing written".into());
                }
                return Ok(Some(name));
            }
            Some(Key::Backspace) => {
                name.pop();
            }
            Some(Key::Character(b)) => name.push(char::from(b)),
            Some(Key::Space) => name.push(' '),
            Some(Key::InvalidInput) => {
                return Err(
                    "Unsupported name character; use printable ASCII; nothing written".into(),
                );
            }
            _ => {}
        }
        if name.len() > 16 {
            return Err("Program name exceeds 16 bytes; nothing written".into());
        }
    }
}

fn reset_confirmation(ui: &mut impl Ui) -> Result<bool> {
    let mut answer = String::new();
    loop {
        ui.draw(&format!("Reset all eight programs' mappings and idle feedback?\nType reset and press Enter to confirm; Esc cancels.\n> {answer}"))?;
        match ui.key()? {
            Some(Key::Escape) => return Ok(false),
            Some(Key::Enter) => return Ok(answer == "reset"),
            Some(Key::Backspace) => {
                answer.pop();
            }
            Some(Key::Character(byte)) if answer.len() < 32 => answer.push(char::from(byte)),
            Some(Key::Space) if answer.len() < 32 => answer.push(' '),
            _ => {}
        }
    }
}

fn text_entry(ui: &mut impl Ui, title: &str, initial: &str) -> Result<Option<String>> {
    let mut value = initial.to_owned();
    loop {
        ui.draw(&format!("{title}\n> {value}\nEnter: accept | Esc: cancel"))?;
        match ui.key()? {
            Some(Key::Escape) => return Ok(None),
            Some(Key::Enter) => return Ok(Some(value)),
            Some(Key::Backspace) => {
                value.pop();
            }
            Some(Key::Character(b)) if value.len() < 4096 => value.push(char::from(b)),
            Some(Key::Space) if value.len() < 4096 => value.push(' '),
            _ => {}
        }
    }
}

fn command_action(
    ui: &mut impl Ui,
    saved: Option<&crate::actions::Action>,
) -> Result<Option<crate::actions::Action>> {
    let (line, dir) = match saved {
        Some(crate::actions::Action::Command { line, dir, .. }) => (line.clone(), dir.clone()),
        _ => (
            String::new(),
            std::env::var("HOME").unwrap_or_else(|_| "/".into()),
        ),
    };
    let Some(line) = text_entry(
        ui,
        "Command line (runs with sh -c in a new tmux window of session vibe)",
        &line,
    )?
    else {
        return Ok(None);
    };
    let Some(dir) = text_entry(ui, "Working directory (absolute path)", &dir)? else {
        return Ok(None);
    };
    // The window is renamed after the mapping once its label is known.
    let action = crate::actions::Action::Command {
        window: "command".into(),
        dir,
        line,
    };
    action.validate()?;
    Ok(Some(action))
}

/// 2–16 prompts, one per equal knob zone from the knob's low end.
fn choice_menu(
    ui: &mut impl Ui,
    path: &Path,
    saved: Option<&crate::actions::Action>,
) -> Result<Option<crate::actions::Action>> {
    use crate::actions::Action;
    let mut choices = match saved {
        Some(Action::Choice(choices)) => choices.clone(),
        _ => Vec::new(),
    };
    loop {
        let mut rows = choices
            .iter()
            .enumerate()
            .map(|(i, c)| format!("Zone {}: {}", i + 1, c.description()))
            .collect::<Vec<_>>();
        rows.extend(options(&["Add choice", "Done", "Cancel"]));
        let Some(index) = choose(
            ui,
            "Choice knob: the knob range is split into equal zones; resting 300 ms in a new zone sends its prompt",
            &rows,
        )?
        else {
            return Ok(None);
        };
        match index.checked_sub(choices.len()) {
            Some(0) if choices.len() < 16 => {
                if let Some(prompt) = prompt_action(ui, path, None)? {
                    choices.push(prompt);
                }
            }
            Some(0) => return Err("a choice knob holds at most 16 prompts".into()),
            Some(1) => {
                let action = Action::Choice(choices);
                action.validate()?;
                return Ok(Some(action));
            }
            Some(_) => return Ok(None),
            None => match choose(
                ui,
                &format!("Zone {}: {}", index + 1, choices[index].description()),
                &options(&["Move up", "Move down", "Remove", "Back"]),
            )? {
                Some(0) if index > 0 => choices.swap(index, index - 1),
                Some(1) if index + 1 < choices.len() => choices.swap(index, index + 1),
                Some(2) => {
                    choices.remove(index);
                }
                _ => {}
            },
        }
    }
}

fn value_menu(
    ui: &mut impl Ui,
    saved: Option<&crate::actions::Action>,
) -> Result<Option<crate::actions::Action>> {
    use crate::actions::{Action, fixed};
    let (path, min, max, decimals) = match saved {
        Some(Action::Value {
            path,
            min,
            max,
            decimals,
        }) => (
            path.clone(),
            fixed(*min, *decimals),
            fixed(*max, *decimals),
            *decimals,
        ),
        _ => (String::new(), "0.0".into(), "1.0".into(), 2),
    };
    let Some(path) = text_entry(
        ui,
        "Value file (absolute path; written as `0.42` + newline)",
        &path,
    )?
    else {
        return Ok(None);
    };
    let Some(min) = text_entry(ui, "Minimum (knob low end)", &min)? else {
        return Ok(None);
    };
    let Some(max) = text_entry(ui, "Maximum (knob high end)", &max)? else {
        return Ok(None);
    };
    let Some(decimals) = number(ui, "Decimal places", i32::from(decimals), 0, 6)? else {
        return Ok(None);
    };
    let scale = |text: &str| -> Result<i64> {
        let value: f64 = text.trim().parse()?;
        let scaled = (value * 10f64.powi(decimals)).round();
        if !scaled.is_finite() || scaled.abs() > 1e12 {
            return Err("value out of range".into());
        }
        Ok(scaled as i64)
    };
    let action = Action::Value {
        path,
        min: scale(&min)?,
        max: scale(&max)?,
        decimals: decimals as u8,
    };
    action.validate()?;
    Ok(Some(action))
}

/// Edit an ordered step list: add, move, remove. Returns None when cancelled.
fn sequence_menu(
    ui: &mut impl Ui,
    path: &Path,
    mut steps: Vec<crate::actions::Step>,
) -> Result<Option<Vec<crate::actions::Step>>> {
    use crate::actions::{Action, Step};
    loop {
        let mut rows = steps
            .iter()
            .enumerate()
            .map(|(i, step)| format!("{}. {}", i + 1, step.description()))
            .collect::<Vec<_>>();
        rows.extend(options(&["Add step", "Done", "Cancel"]));
        let Some(index) = choose(
            ui,
            "Sequence steps (run in order; a failing step stops the rest)",
            &rows,
        )?
        else {
            return Ok(None);
        };
        match index.checked_sub(steps.len()) {
            Some(0) if steps.len() < 32 => {
                let Some(kind) = choose(
                    ui,
                    "Step",
                    &options(&[
                        "Ensure agent running",
                        "Prompt",
                        "Command",
                        "Launch application",
                        "Shortcut",
                        "Wait",
                    ]),
                )?
                else {
                    continue;
                };
                let step = match kind {
                    0 => {
                        let names = mappings::load_config(path)?
                            .agents
                            .into_iter()
                            .map(|a| a.name)
                            .collect::<Vec<_>>();
                        choose(ui, "Agent", &names)?.map(|i| Step::Agent(names[i].clone()))
                    }
                    1 => prompt_action(ui, path, None)?.map(Step::Do),
                    2 => command_action(ui, None)?.map(Step::Do),
                    3 => action_menu(ui, 1, mappings::Input::Pad, None, false)?.map(Step::Do),
                    4 => shortcut(ui, &[])?.map(Step::Keys),
                    _ => number(ui, "Wait (milliseconds)", 1000, 100, 60000)?
                        .map(|ms| Step::Wait(ms as u16)),
                };
                if let Some(step) = step {
                    Action::Sequence(vec![step.clone()]).validate()?;
                    steps.push(step);
                }
            }
            Some(0) => return Err("a sequence holds at most 32 steps".into()),
            Some(1) if steps.is_empty() => return Err("add at least one step".into()),
            Some(1) => return Ok(Some(steps)),
            Some(_) => return Ok(None),
            None => match choose(
                ui,
                &format!("Step {}: {}", index + 1, steps[index].description()),
                &options(&["Move up", "Move down", "Remove", "Back"]),
            )? {
                Some(0) if index > 0 => steps.swap(index, index - 1),
                Some(1) if index + 1 < steps.len() => steps.swap(index, index + 1),
                Some(2) => {
                    steps.remove(index);
                }
                _ => {}
            },
        }
    }
}

/// A tmux window name made from a mapping's display name, e.g. "Bank A Pad 1" → "Bank-A-Pad-1".
fn window_name(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(80)
        .collect();
    if name.is_empty() {
        "command".into()
    } else {
        name
    }
}

const STATUS_EVERY: Duration = Duration::from_millis(500);

/// What a status pad's last press did: sent until the worker answers, then done or failed.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Sent,
    Done,
    Failed,
}

/// A failed press stays failed until the next press; otherwise the derived state wins, so a
/// stopped process or killed server never shows a stale "running".
fn status_word(phase: Option<Phase>, running: Option<bool>) -> &'static str {
    match (phase, running) {
        (Some(Phase::Failed), _) => "failed",
        (_, Some(true)) => "running",
        (Some(Phase::Sent), _) => "sent",
        (_, None) => "unavailable",
        _ => "stopped",
    }
}

/// "\nAgents: claude thinking · codex FINISHED", or nothing without agents.
fn agent_line(states: &[(String, String, &'static str)]) -> String {
    if states.is_empty() {
        return String::new();
    }
    let parts = states
        .iter()
        .map(|(name, _, state)| match *state {
            "finished" => format!("{name} FINISHED"),
            "needs-input" => format!("{name} NEEDS INPUT"),
            state => format!("{name} {state}"),
        })
        .collect::<Vec<_>>();
    format!("\nAgents: {}", parts.join(" · "))
}

fn agents_menu(ui: &mut impl Ui, path: &Path) -> Result<()> {
    loop {
        let mut config = mappings::load_config(path)?;
        let mut choices = config
            .agents
            .iter()
            .map(|a| {
                format!(
                    "{} | {} | tmux {} | {}",
                    a.name,
                    a.launch,
                    a.window,
                    crate::status::read(&a.name)
                )
            })
            .collect::<Vec<_>>();
        choices.extend(options(&[
            "Add agent",
            "Claude Code status hooks",
            "Codex status hooks",
        ]));
        choices.push(format!(
            "Status lights: {} (tempo, arpeggiator and octave lights follow agent status)",
            if config.status_lights { "On" } else { "Off" }
        ));
        choices.push("Prompts".into());
        let Some(index) = choose(ui, "Agents | tmux attach -t vibe", &choices)? else {
            return Ok(());
        };
        if index == config.agents.len() + 4 {
            prompts_menu(ui, path)?;
            continue;
        }
        if index == config.agents.len() + 3 {
            config.status_lights = !config.status_lights;
            if let Err(error) = mappings::save_config(path, &config) {
                not_saved(ui, &error)?;
            }
            continue;
        }
        if let Some(which @ (1 | 2)) = index.checked_sub(config.agents.len()) {
            let codex = which == 2;
            let hooks = if codex {
                crate::status::codex_hooks()
            } else {
                crate::status::claude_hooks()
            };
            let text = format!(
                "{}\nInstall merges only the missing entries, after a backup. Hooks report only for agents VibeConsole started in tmux; `vibeconsole` must be on PATH.\n\n{hooks}",
                crate::status::hooks_status(codex)
            );
            match choose(
                ui,
                &text,
                &options(&["Install (merge, with backup)", "Copy with wl-copy", "Back"]),
            )? {
                Some(0) => match crate::status::install_hooks(codex) {
                    Ok(note) => {
                        choose(ui, &note, &options(&["Back"]))?;
                    }
                    Err(error) => not_saved(ui, &error)?,
                },
                Some(1) => crate::actions::copy_text(&hooks)?,
                _ => {}
            }
            continue;
        }
        let existing = config.agents.get(index).cloned();
        let operation = if existing.is_some() {
            choose(ui, "Agent", &options(&["Edit", "Remove", "Back"]))?
        } else {
            Some(0)
        };
        if operation == Some(1) {
            if choose(
                ui,
                "Remove this agent? Its prompt mappings become invalid.",
                &options(&["Cancel", "Remove"]),
            )? == Some(1)
            {
                config.agents.remove(index);
                if let Err(error) = mappings::save_config(path, &config) {
                    not_saved(ui, &error)?;
                }
            }
            continue;
        }
        if operation != Some(0) {
            continue;
        }
        let Some(name) = text_entry(ui, "Agent name", existing.as_ref().map_or("", |a| &a.name))?
        else {
            continue;
        };
        let Some(launch) = text_entry(
            ui,
            "Launch command",
            existing.as_ref().map_or("", |a| &a.launch),
        )?
        else {
            continue;
        };
        let Some(window) = text_entry(
            ui,
            "tmux window name",
            existing.as_ref().map_or("", |a| &a.window),
        )?
        else {
            continue;
        };
        let Some(dir) = text_entry(
            ui,
            "Working directory (absolute path or ${NAME}; empty uses the Settings default)",
            existing
                .as_ref()
                .and_then(|a| a.dir.as_deref())
                .unwrap_or(""),
        )?
        else {
            continue;
        };
        let defaults = mappings::Agent::new(&name, &launch, &window).ok();
        let light = existing.as_ref().or(defaults.as_ref());
        let Some(octave) = number(
            ui,
            "Status light octave (−4..−1 or +1..+4; Claude +1, Codex −1)",
            light.map_or(1, |a| a.octave.into()),
            -4,
            4,
        )?
        else {
            continue;
        };
        let Some(thinking) = number(
            ui,
            "Status light: thinking tempo BPM (1..300; slow blink)",
            light.map_or(50, |a| a.thinking_bpm.into()),
            1,
            300,
        )?
        else {
            continue;
        };
        let Some(input) = number(
            ui,
            "Status light: needs-input tempo BPM (1..300; fast blink)",
            light.map_or(240, |a| a.input_bpm.into()),
            1,
            300,
        )?
        else {
            continue;
        };
        let saved = (|| -> Result<()> {
            let mut agent = mappings::Agent::new(&name, &launch, &window)?;
            agent.octave = octave.try_into()?;
            agent.thinking_bpm = thinking.try_into()?;
            agent.input_bpm = input.try_into()?;
            agent.validate_lights()?;
            if !dir.trim().is_empty() {
                existing_directory(&dir, &config)?;
                agent.dir = Some(dir.clone());
            }
            if existing.is_some() {
                config.agents[index] = agent;
            } else {
                config.agents.push(agent);
            }
            mappings::save_config(path, &config)
        })();
        if let Err(error) = saved {
            not_saved(ui, &error)?;
        }
    }
}

/// A rejected Settings/Agents/Prompts entry: nothing changed, so a note, not a failure that pauses.
fn not_saved(ui: &mut impl Ui, error: &dyn std::fmt::Display) -> Result<()> {
    choose(ui, &format!("Not saved: {error}"), &options(&["Back"]))?;
    Ok(())
}

/// A directory field that must exist now (checked before saving).
fn existing_directory(dir: &str, config: &mappings::Config) -> Result<String> {
    let expanded = mappings::absolute_directory(dir, config)?;
    if !Path::new(&expanded).is_dir() {
        return Err(format!("directory {expanded} is missing").into());
    }
    Ok(expanded)
}

/// Settings (Configure group): default agent directory, Flow at launch, variables.
fn settings_menu(ui: &mut impl Ui, path: &Path) -> Result<()> {
    loop {
        let mut config = mappings::load_config(path)?;
        let rows = vec![
            format!(
                "Default agent directory: {}",
                config
                    .agent_dir
                    .as_deref()
                    .unwrap_or("not set (tmux default)")
            ),
            format!(
                "Start Wispr Flow with VibeConsole: {} (takes effect next launch)",
                if config.start_flow { "On" } else { "Off" }
            ),
            format!(
                "Startup variables ({}): exported into tmux sessions and commands",
                config.env.len()
            ),
            format!(
                "User variables ({}): ${{NAME}} in commands and directories",
                config.vars.len()
            ),
        ];
        let Some(choice) = choose(ui, "Settings", &rows)? else {
            return Ok(());
        };
        let result = match choice {
            0 => {
                let Some(dir) = text_entry(
                    ui,
                    "Default agent directory (absolute path or ${NAME}; empty clears)",
                    config.agent_dir.as_deref().unwrap_or(""),
                )?
                else {
                    continue;
                };
                (|| {
                    config.agent_dir = if dir.trim().is_empty() {
                        None
                    } else {
                        existing_directory(&dir, &config)?;
                        Some(dir)
                    };
                    mappings::save_config(path, &config)
                })()
            }
            1 => {
                config.start_flow = !config.start_flow;
                mappings::save_config(path, &config)
            }
            _ => variables_menu(ui, path, choice == 3),
        };
        if let Err(error) = result {
            not_saved(ui, &error)?;
        }
    }
}

/// Add, edit or remove startup (`user` false) or user variables. Saving validates names,
/// values and every `${NAME}` the configuration uses.
fn variables_menu(ui: &mut impl Ui, path: &Path, user: bool) -> Result<()> {
    loop {
        let mut config = mappings::load_config(path)?;
        let list = if user { &config.vars } else { &config.env };
        let mut rows = list
            .iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>();
        let count = rows.len();
        rows.push("Add variable".into());
        let title = if user {
            "User variables (${NAME} in command lines, command and agent directories)"
        } else {
            "Startup variables (exported into every tmux session and command VibeConsole starts)"
        };
        let Some(index) = choose(ui, title, &rows)? else {
            return Ok(());
        };
        let current = (index < count).then(|| list[index].clone());
        if current.is_some() {
            match choose(ui, &rows[index], &options(&["Edit", "Remove", "Back"]))? {
                Some(0) => {}
                Some(1) => {
                    let list = if user {
                        &mut config.vars
                    } else {
                        &mut config.env
                    };
                    list.remove(index);
                    if let Err(error) = mappings::save_config(path, &config) {
                        not_saved(ui, &error)?;
                    }
                    continue;
                }
                _ => continue,
            }
        }
        let (name, value) = current.unwrap_or_default();
        let Some(name) = text_entry(ui, "Variable name (letters, digits, _)", &name)? else {
            continue;
        };
        let Some(value) = text_entry(ui, &format!("Value of {name} (one line)"), &value)? else {
            continue;
        };
        let list = if user {
            &mut config.vars
        } else {
            &mut config.env
        };
        if index < count {
            list[index] = (name, value);
        } else {
            list.push((name, value));
        }
        if let Err(error) = mappings::save_config(path, &config) {
            not_saved(ui, &error)?;
        }
    }
}

/// Saved prompts beside the configuration, sorted; editor scratch files are hidden.
fn prompt_files(dir: &Path) -> Result<Vec<String>> {
    std::fs::create_dir_all(dir).map_err(stage("file I/O"))?;
    let mut files = std::fs::read_dir(dir)
        .map_err(stage("file I/O"))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| mappings::simple_name(name) && dir.join(name).is_file())
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

/// Edit (or create) prompt `name` on a hidden copy, then rename it into place: a failed or
/// empty edit keeps the old prompt.
fn edit_prompt(ui: &mut impl Ui, dir: &Path, name: &str) -> Result<()> {
    if !mappings::simple_name(name) {
        return Err(
            "prompt names are letters, digits, '-', '_' and '.', not starting with '.'".into(),
        );
    }
    let target = dir.join(name);
    let scratch = dir.join(format!(".{name}.edit"));
    let result = (|| -> Result<()> {
        // A new prompt starts empty; an existing one that cannot be read is never replaced.
        let old = match std::fs::read(&target) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            other => other?,
        };
        std::fs::write(&scratch, old)?;
        ui.edit_file(&scratch)?;
        if std::fs::read(&scratch)?.iter().all(u8::is_ascii_whitespace) {
            return Err("prompt is empty; nothing saved".into());
        }
        std::fs::rename(&scratch, &target)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&scratch);
    result.map_err(stage("file I/O"))
}

/// Agents → Prompts: list, read, create, edit, delete (ticket 13).
fn prompts_menu(ui: &mut impl Ui, path: &Path) -> Result<()> {
    let dir = crate::actions::prompts_dir_for(path)?;
    loop {
        let files = prompt_files(&dir)?;
        let mut rows = files.clone();
        rows.push("New prompt".into());
        let Some(index) = choose(ui, &format!("Prompts in {}", dir.display()), &rows)? else {
            return Ok(());
        };
        let result = if index == files.len() {
            match text_entry(ui, "New prompt name", "")? {
                Some(name) if files.contains(&name) => {
                    Err(format!("prompt {name} already exists").into())
                }
                Some(name) => edit_prompt(ui, &dir, &name),
                None => Ok(()),
            }
        } else {
            let name = &files[index];
            match choose(
                ui,
                &format!("Prompt {name}"),
                &options(&["Read", "Edit", "Delete", "Back"]),
            )? {
                Some(0) => {
                    let text = std::fs::read_to_string(dir.join(name))
                        .unwrap_or_else(|e| format!("(unreadable: {e})"));
                    choose(ui, &format!("{name}\n\n{text}"), &options(&["Back"])).map(drop)
                }
                Some(1) => edit_prompt(ui, &dir, name),
                Some(2) => {
                    let config = mappings::load_config(path)?;
                    let users = config
                        .mappings
                        .iter()
                        .filter(|m| m.action.prompt_files().contains(&name.as_str()))
                        .map(|m| format!("{} {}", mappings::program_label(m.context), m.name(None)))
                        .collect::<Vec<_>>();
                    let warning = if users.is_empty() {
                        "No mapping uses it.".to_string()
                    } else {
                        format!(
                            "Used by: {}. Those pads will report a missing prompt.",
                            users.join(", ")
                        )
                    };
                    if choose(
                        ui,
                        &format!("Delete prompt {name}?\n{warning}"),
                        &options(&["Cancel", "Delete"]),
                    )? == Some(1)
                    {
                        std::fs::remove_file(dir.join(name)).map_err(stage("file I/O"))
                    } else {
                        Ok(())
                    }
                }
                _ => Ok(()),
            }
        };
        if let Err(error) = result {
            not_saved(ui, &error)?;
        }
    }
}

fn prompt_action(
    ui: &mut impl Ui,
    path: &Path,
    saved: Option<&crate::actions::Action>,
) -> Result<Option<crate::actions::Action>> {
    let dir = crate::actions::prompts_dir_for(path)?;
    let mut files = prompt_files(&dir)?;
    files.push("New prompt".into());
    let saved_file = match saved {
        Some(crate::actions::Action::Prompt { file, .. }) => Some(file),
        _ => None,
    };
    let Some(index) = choose_saved(
        ui,
        "Prompt file",
        files.clone(),
        saved_file.and_then(|f| files.iter().position(|s| s == f)),
    )?
    else {
        return Ok(None);
    };
    let file = if index == files.len() - 1 {
        let Some(name) = text_entry(
            ui,
            "New prompt file name (simple name, no extension required)",
            "",
        )?
        else {
            return Ok(None);
        };
        if !mappings::simple_name(&name) {
            return Err("invalid prompt file name".into());
        }
        name
    } else {
        files[index].clone()
    };
    if index == files.len() - 1
        || choose(
            ui,
            "Edit prompt in external editor?",
            &options(&["Use existing", "Edit"]),
        )? == Some(1)
    {
        if let Err(error) = edit_prompt(ui, &dir, &file) {
            not_saved(ui, &error)?;
            return Ok(None); // back to the menu that asked; nothing else is lost
        }
    }
    use crate::actions::Target;
    let mut targets = mappings::load_config(path)
        .map_err(stage("file I/O"))?
        .agents
        .into_iter()
        .map(|a| Target::Agent(a.name))
        .collect::<Vec<_>>();
    targets.push(Target::Focused { shift: false });
    targets.push(Target::Focused { shift: true });
    let saved_target = match saved {
        Some(crate::actions::Action::Prompt { target, .. }) => {
            targets.iter().position(|t| t == target)
        }
        _ => None,
    };
    let Some(target_index) = choose_saved(
        ui,
        "Target: tmux agent, or the focused window (Ctrl+V browser/Claude app, Ctrl+Shift+V terminals; lands wherever focus is)",
        targets.iter().map(Target::description).collect(),
        saved_target,
    )?
    else {
        return Ok(None);
    };
    let Some(mode) = choose(
        ui,
        "Submit prompt?",
        &options(&["Paste only", "Paste and Enter"]),
    )?
    else {
        return Ok(None);
    };
    Ok(Some(crate::actions::Action::Prompt {
        file,
        target: targets.swap_remove(target_index),
        enter: mode == 1,
    }))
}

fn reset_file(path: &Path) -> Result<PathBuf> {
    let mut now = 0;
    // SAFETY: localtime_r writes to the provided tm; strftime writes within the fixed buffer.
    let stamp = unsafe {
        libc::time(&mut now);
        let mut date = std::mem::zeroed::<libc::tm>();
        if libc::localtime_r(&now, &mut date).is_null() {
            return Err("Cannot create reset backup timestamp".into());
        }
        let mut buffer = [0u8; 16];
        if libc::strftime(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            c"%Y%m%d-%H%M%S".as_ptr(),
            &date,
        ) == 0
        {
            return Err("Cannot format reset backup timestamp".into());
        }
        std::ffi::CStr::from_ptr(buffer.as_ptr().cast())
            .to_str()?
            .to_owned()
    };
    let backup = path.with_file_name(format!(
        "{}.bak-{stamp}",
        path.file_name()
            .ok_or("Invalid configuration path")?
            .to_string_lossy()
    ));
    let mut source = std::fs::File::open(path)?;
    let mut target = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&backup)?;
    if let Err(error) = io::copy(&mut source, &mut target).and_then(|_| target.sync_all()) {
        let _ = std::fs::remove_file(&backup);
        return Err(error.into());
    }
    mappings::save_config(path, &mappings::Config::default())?;
    Ok(backup)
}

fn pad_labels(control: Control, current: &[Mapping], context: u8) -> Vec<String> {
    let candidates = std::iter::once(control.label()).chain(
        ['A', 'B']
            .into_iter()
            .flat_map(|bank| (1..=8).map(move |number| format!("Bank {bank} Pad {number}"))),
    );
    let mut labels = Vec::new();
    for label in candidates {
        if !labels.contains(&label)
            && !current.iter().any(|mapping| {
                mapping.context == context && mapping.control != control && mapping.label == label
            })
        {
            labels.push(label);
        }
    }
    labels
}

fn choose_at(
    ui: &mut impl Ui,
    title: &str,
    options: &[String],
    initial: usize,
) -> Result<Option<usize>> {
    if options.is_empty() {
        return Ok(None);
    }
    let mut cursor = initial.min(options.len() - 1);
    loop {
        let visible = ui
            .height()
            .saturating_sub(crate::screen::wrap(title, ui.width().saturating_sub(5)).len() + 10)
            .max(1);
        let first = cursor.saturating_sub(visible - 1);
        let rows = options
            .iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .map(|(i, text)| {
                format!(
                    "{} {}",
                    if i == cursor { ">" } else { " " },
                    crate::screen::fit(text, ui.width().saturating_sub(7)).trim_end()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        ui.draw(&format!("{title}\n\n{rows}"))?;
        match ui.key()? {
            Some(Key::Up) => cursor = (cursor + options.len() - 1) % options.len(),
            Some(Key::Down) => cursor = (cursor + 1) % options.len(),
            Some(Key::Enter) => return Ok(Some(cursor)),
            Some(Key::Escape) => return Ok(None),
            _ => {}
        }
    }
}
/// Appends `[x] (saved)` to the saved option; the mark stays put while the cursor moves.
fn mark_saved(mut options: Vec<String>, saved: Option<usize>) -> Vec<String> {
    if let Some(option) = saved.and_then(|i| options.get_mut(i)) {
        option.push_str("  [x] (saved)");
    }
    options
}

/// Opens on the saved option and marks it. With nothing saved (a new mapping) this is `choose`.
fn choose_saved(
    ui: &mut impl Ui,
    title: &str,
    options: Vec<String>,
    saved: Option<usize>,
) -> Result<Option<usize>> {
    choose_at(ui, title, &mark_saved(options, saved), saved.unwrap_or(0))
}

fn options(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn number(ui: &mut impl Ui, title: &str, initial: i32, min: i32, max: i32) -> Result<Option<i32>> {
    let mut value = initial;
    loop {
        ui.draw(&format!(
            "{title}\nValue: {value}\nUp/Down: adjust | Enter: confirm | Esc: cancel"
        ))?;
        match ui.key()? {
            Some(Key::Up) => value = (value + 1).min(max),
            Some(Key::Down) => value = (value - 1).max(min),
            Some(Key::Enter) => return Ok(Some(value)),
            Some(Key::Escape) => return Ok(None),
            _ => {}
        }
    }
}

fn select_keys(ui: &mut impl Ui, saved: &[String]) -> Result<Option<Vec<String>>> {
    let groups = mappings::key_groups();
    let mut selected = saved.to_vec();
    loop {
        let mut menu: Vec<_> = groups.iter().map(|(name, _)| name.to_string()).collect();
        menu.push("Done selecting".into());
        let Some(group) = choose(ui, &format!("Shortcut: {}", selected.join("+")), &menu)? else {
            return Ok(None);
        };
        if group == groups.len() {
            if selected.is_empty() {
                continue;
            }
            return Ok(Some(mappings::parse_shortcut(&selected.join("+"))?));
        }
        let keys = &groups[group].1;
        let mut cursor: usize = 0;
        loop {
            let visible = ui.height().saturating_sub(12).max(3);
            let first = cursor.saturating_sub(visible - 1);
            let rows = keys
                .iter()
                .enumerate()
                .skip(first)
                .take(visible)
                .map(|(i, k)| {
                    format!(
                        "{} [{}] {k}{}",
                        if i == cursor { ">" } else { " " },
                        if selected.contains(k) { "x" } else { " " },
                        if saved.contains(k) { " (saved)" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            ui.draw(&format!(
                "{} | Shortcut: {}\nSpace: toggle | Enter: return to groups\n{rows}",
                groups[group].0,
                selected.join("+")
            ))?;
            match ui.key()? {
                Some(Key::Up) => cursor = (cursor + keys.len() - 1) % keys.len(),
                Some(Key::Down) => cursor = (cursor + 1) % keys.len(),
                Some(Key::Space) => {
                    if selected.contains(&keys[cursor]) {
                        selected.retain(|k| k != &keys[cursor]);
                    } else {
                        selected.push(keys[cursor].clone());
                    }
                }
                Some(Key::Enter | Key::Escape) => break,
                _ => {}
            }
        }
    }
}

fn shortcut(ui: &mut impl Ui, saved: &[String]) -> Result<Option<Vec<String>>> {
    let saved_line = if saved.is_empty() {
        String::new()
    } else {
        format!("\nSaved: {}  [x] (saved)", saved.join("+"))
    };
    loop {
        match choose(
            ui,
            &format!("Shortcut input{saved_line}"),
            &options(&["Select keys", "Record shortcut", "Cancel"]),
        )? {
            Some(0) => return select_keys(ui, saved),
            Some(1) => loop {
                let keys = match ui.record() {
                    Ok(keys) => keys,
                    Err(error) => {
                        choose(
                            ui,
                            &format!(
                                "Capture failed: {error}\nMapping unchanged; Select keys remains available."
                            ),
                            &options(&["Back"]),
                        )?;
                        break;
                    }
                };
                match choose(
                    ui,
                    &format!("Captured: {}{saved_line}", keys.join("+")),
                    &options(&["Use", "Record again", "Cancel"]),
                )? {
                    Some(0) => return Ok(Some(keys)),
                    Some(1) => continue,
                    _ => return Ok(None),
                }
            },
            _ => return Ok(None),
        }
    }
}

fn feedback_menu(
    ui: &mut impl Ui,
    initial: crate::feedback::Feedback,
    saved: bool,
    idle: bool,
) -> Result<Option<crate::feedback::Feedback>> {
    use crate::feedback::{Feedback, Octave};
    let mut feedback = initial;
    // Marks always show the saved value; the cursor follows the working value.
    let mark = |index: usize| saved.then_some(index);
    let mut cursor = if feedback.octave.is_some() {
        1
    } else if feedback.tempo.is_some() {
        2
    } else if feedback.arp.is_some() {
        3
    } else {
        0
    };
    loop {
        let title = format!(
            "{}: {feedback}",
            if idle {
                "Idle feedback (tempo stored if arpeggiator Off)"
            } else {
                "Optional hardware feedback"
            }
        );
        let items = options(&[
            "No hardware feedback",
            "Octave",
            "Tempo",
            "Arpeggiator",
            "Done",
            "Cancel",
        ]);
        let items = mark_saved(items, mark(0).filter(|_| !initial.enabled()));
        let Some(choice) = choose_at(ui, &title, &items, cursor)? else {
            return Ok(None);
        };
        cursor = choice;
        match choice {
            0 => return Ok(Some(Feedback::default())),
            1 => {
                let modes = if idle {
                    options(&["Unchanged", "Absolute octave"])
                } else {
                    options(&[
                        "Unchanged",
                        "Absolute octave",
                        "Offset from session baseline (not accumulated)",
                    ])
                };
                let index = |octave| match octave {
                    None => 0,
                    Some(Octave::Absolute(_)) => 1,
                    Some(Octave::Offset(_)) => 2,
                };
                let modes = mark_saved(modes, mark(index(initial.octave)));
                match choose_at(ui, "Octave mode", &modes, index(feedback.octave))? {
                    Some(0) => feedback.octave = None,
                    Some(mode) => {
                        let current = match feedback.octave {
                            Some(Octave::Absolute(n) | Octave::Offset(n)) => n,
                            None => 0,
                        };
                        if let Some(n) = number(ui, "Octave -4..+4", current as i32, -4, 4)? {
                            feedback.octave = Some(if mode == 1 {
                                Octave::Absolute(n as i8)
                            } else {
                                Octave::Offset(n as i8)
                            });
                        }
                    }
                    None => {}
                }
            }
            2 => match choose_at(
                ui,
                "Tempo",
                &mark_saved(
                    options(&["Unchanged", "Set BPM (enables arpeggiator)"]),
                    mark(usize::from(initial.tempo.is_some())),
                ),
                usize::from(feedback.tempo.is_some()),
            )? {
                Some(0) => feedback.tempo = None,
                Some(1) => {
                    if let Some(bpm) = number(
                        ui,
                        "Tempo BPM 1..300",
                        feedback.tempo.unwrap_or(120) as i32,
                        1,
                        300,
                    )? {
                        feedback.tempo = Some(bpm as u16);
                        feedback.arp = Some(true);
                    }
                }
                _ => {}
            },
            3 => {
                let index = |arp: Option<bool>| arp.map(|on| usize::from(on) + 1).unwrap_or(0);
                if let Some(choice) = choose_at(
                    ui,
                    "Arpeggiator",
                    &mark_saved(
                        options(&["Unchanged", "Off", "On"]),
                        mark(index(initial.arp)),
                    ),
                    index(feedback.arp),
                )? {
                    if !idle && choice == 1 && feedback.tempo.is_some() {
                        choose(
                            ui,
                            "Tempo blinking requires arpeggiator On. Remove tempo first to select Off.",
                            &options(&["Back"]),
                        )?;
                    } else {
                        feedback.arp = match choice {
                            0 => None,
                            1 => Some(false),
                            _ => Some(true),
                        };
                    }
                }
            }
            4 => return Ok(Some(feedback)),
            _ => return Ok(None),
        }
    }
}

fn relative_source(samples: &[[u8; 3]]) -> Result<(u8, u8)> {
    let mut source = None;
    for [status, cc, _] in samples {
        if status & 0xf0 != 0xb0 {
            continue;
        }
        let current = ((status & 15) + 1, *cc);
        if source.is_some_and(|old| old != current) {
            return Err("multiple CC sources captured; retry moving only one knob".into());
        }
        source = Some(current);
    }
    source.ok_or_else(|| "no knob CC captured".into())
}

fn confirm_relative_ticks(samples: &[[u8; 3]], channel: u8, cc: u8) -> Result<()> {
    if relative_source(samples)? != (channel, cc)
        || samples.iter().any(|[status, id, raw]| {
            status & 0xf0 == 0xb0 && (*id != cc || *raw == 0 || *raw == 64)
        })
        || !samples
            .iter()
            .any(|[status, id, raw]| status & 0xf0 == 0xb0 && *id == cc && (1..=63).contains(raw))
        || !samples
            .iter()
            .any(|[status, id, raw]| status & 0xf0 == 0xb0 && *id == cc && (65..=127).contains(raw))
    {
        return Err("relative ticks not confirmed in both directions".into());
    }
    Ok(())
}

fn calibrate_motion(
    ui: &mut impl Ui,
    samples: Vec<[u8; 3]>,
    knob: bool,
) -> Result<Option<(Control, mappings::Input, String)>> {
    let mut sources: Vec<((crate::Message, u8, u8), Vec<u16>)> = Vec::new();
    for [status, id, raw] in samples {
        let (message, id, value) = match status & 0xf0 {
            0xb0 => (crate::Message::Cc, id, u16::from(raw)),
            0xe0 if !knob => (
                crate::Message::Bend,
                0,
                u16::from(id) | (u16::from(raw) << 7),
            ),
            _ => continue,
        };
        let identity = (message, (status & 15) + 1, id);
        match sources.iter_mut().find(|(source, _)| *source == identity) {
            Some((_, values)) => values.push(value),
            None => sources.push((identity, vec![value])),
        }
    }
    // Travel as a share of the message's full range, so CC and bend compare.
    let travel = |((message, _, _), values): &(_, Vec<u16>)| {
        let limit = if *message == crate::Message::Bend {
            16383.0
        } else {
            127.0
        };
        f64::from(values.iter().max().unwrap() - values.iter().min().unwrap()) / limit
    };
    sources.sort_by(|a, b| travel(b).total_cmp(&travel(a)));
    // ponytail: a stick sweep wobbles the other axis; keep the dominant one when the rest moved
    // under a quarter as far. The user confirms the observed source and range next.
    if sources.len() > 1
        && travel(&sources[1]) > 0.0
        && travel(&sources[1]) * 4.0 >= travel(&sources[0])
    {
        return Err(
            "multiple sources observed; retry moving only one knob or joystick axis".into(),
        );
    }
    let ((message, channel, id), values) = sources
        .into_iter()
        .next()
        .ok_or("no supported motion messages captured")?;
    let min = *values.iter().min().unwrap();
    let max = *values.iter().max().unwrap();
    let center = *values.last().unwrap();
    if max - min < 3 {
        return Err("not enough movement observed; retry through full travel".into());
    }
    let control = Control {
        message,
        channel,
        id,
        direction: 1,
    };
    if knob {
        if !values.windows(2).any(|v| v[1] > v[0]) || !values.windows(2).any(|v| v[1] < v[0]) {
            return Err("observe both knob directions before saving".into());
        }
        if choose(
            ui,
            &format!(
                "Observed CC{id} channel {channel}, values {min}..{max}.\nOnly non-wrapping ABSOLUTE knobs are supported.\nConfirm values track position and stop rather than wrap; relative encodings are unsupported."
            ),
            &options(&[
                "Cancel / unsupported encoding",
                "Confirm absolute position, no wrap",
            ]),
        )? != Some(1)
        {
            return Ok(None);
        }
        let Some(direction) = choose(
            ui,
            "Observed value direction",
            &options(&["Increasing values", "Decreasing values"]),
        )?
        else {
            return Ok(None);
        };
        let control = Control {
            direction: if direction == 0 { 1 } else { -1 },
            ..control
        };
        let Some(number) = number(ui, "Confirm physical knob number", 1, 1, 8)? else {
            return Ok(None);
        };
        return Ok(Some((
            control,
            mappings::Input::Knob { step: 4 },
            format!(
                "Knob {number} {}",
                if direction == 0 {
                    "increase"
                } else {
                    "decrease"
                }
            ),
        )));
    }
    if choose(
        ui,
        &format!(
            "Observed {} channel {channel} id {id}: {min}..{max}, final {center}.\nConfirm BOTH endpoints were reached and the FINAL value is released neutral.\nAxes act independently: diagonals hold both mapped axes.\nA one-sided CC axis cannot distinguish physical Up from Down.",
            message.text()
        ),
        &options(&[
            "Cancel / recalibrate",
            "Confirm endpoints, neutral, independent axes",
        ]),
    )? != Some(1)
    {
        return Ok(None);
    }
    let mut directions = Vec::new();
    if max - center >= 3 {
        directions.push(1);
    }
    if center - min >= 3 {
        directions.push(-1);
    }
    let choices = directions
        .iter()
        .map(|d| {
            if *d > 0 {
                "Increasing side".to_owned()
            } else {
                "Decreasing side".to_owned()
            }
        })
        .collect::<Vec<_>>();
    let Some(index) = choose(ui, "Select observed axis side", &choices)? else {
        return Ok(None);
    };
    let direction = directions[index];
    let reach = if direction > 0 {
        max - center
    } else {
        center - min
    };
    let Some(deadzone) = number(
        ui,
        "Neutral region (native MIDI units)",
        i32::from((reach / 8).max(2)),
        2,
        i32::from(reach - 1),
    )?
    else {
        return Ok(None);
    };
    let Some(hysteresis) = number(
        ui,
        "Hysteresis (release closer to neutral than activation)",
        0,
        0,
        (deadzone - 1).min(i32::from(reach) - deadzone - 1),
    )?
    else {
        return Ok(None);
    };
    let labels = if center == min || center == max {
        options(&["Joystick excursion (one-sided axis; directions share values)"])
    } else {
        options(&[
            "Joystick Left",
            "Joystick Right",
            "Joystick Up",
            "Joystick Down",
        ])
    };
    let Some(label) = choose(
        ui,
        "Confirm physical direction for the selected value side",
        &labels,
    )?
    else {
        return Ok(None);
    };
    Ok(Some((
        Control {
            direction,
            ..control
        },
        mappings::Input::Joystick {
            center,
            min,
            max,
            deadzone: deadzone as u16,
            hysteresis: hysteresis as u16,
        },
        labels[label].clone(),
    )))
}

/// `second_press`: a launcher pad (not a sequence step) also chooses minimize or close.
fn action_menu(
    ui: &mut impl Ui,
    family: usize,
    input: mappings::Input,
    saved: Option<&crate::actions::Action>,
    second_press: bool,
) -> Result<Option<crate::actions::Action>> {
    use crate::actions::Action;
    if matches!(input, mappings::Input::Joystick { .. })
        || (family == 1
            && matches!(
                input,
                mappings::Input::Knob { .. } | mappings::Input::RelativeKnob { .. }
            ))
    {
        return Err("launch actions require a pad/piano press; joystick actions remain keyboard holds; knobs support audio steps".into());
    }
    if family == 1 {
        let apps = ui.applications()?;
        let mut names = apps
            .iter()
            .map(|(n, p)| {
                format!(
                    "{n} | {}",
                    Path::new(p)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                )
            })
            .collect::<Vec<_>>();
        names.push("Start or repair Wispr Flow".into());
        let saved_action = saved;
        let saved = match saved {
            Some(Action::Application(path) | Action::AppToggle { path, .. }) => {
                apps.iter().position(|(_, p)| p == path)
            }
            Some(Action::Flow) => Some(apps.len()),
            _ => None,
        };
        let Some(i) = choose_saved(
            ui,
            "Installed visible applications (one native launch request per press)",
            names,
            saved,
        )?
        else {
            return Ok(None);
        };
        if i == apps.len() {
            return Ok(Some(Action::Flow));
        }
        let path = apps[i].1.clone();
        if !second_press {
            return Ok(Some(Action::Application(path)));
        }
        let saved_close = match saved_action {
            Some(Action::AppToggle { close, .. }) => Some(usize::from(*close)),
            _ => None,
        };
        let Some(close) = choose_saved(
            ui,
            "Second press while the app is focused (needs the VibeConsole window extension; see README)",
            options(&["Minimize", "Close"]),
            saved_close,
        )?
        else {
            return Ok(None);
        };
        return Ok(Some(Action::AppToggle {
            path,
            close: close == 1,
        }));
    }
    let (saved, saved_step) = match saved {
        Some(Action::Volume { up, step }) => (Some(usize::from(!up)), Some(*step)),
        Some(Action::OutputMute) => (Some(2), None),
        Some(Action::MicrophoneMute) => (Some(3), None),
        _ => (None, None),
    };
    let Some(i) = choose_saved(
        ui,
        "Native audio actions; default device resolved at activation.
Requires wpctl in your desktop session. Brightness/media/lock and absolute volume are not enabled.",
        options(&[
            "Output volume up",
            "Output volume down",
            "Output mute",
            "Microphone mute",
        ]),
        saved,
    )?
    else {
        return Ok(None);
    };
    if i < 2 {
        let Some(step) = number(
            ui,
            "Volume step (%) per activation; ordinary volume capped at 100%",
            saved_step.unwrap_or(5).into(),
            1,
            100,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(Action::Volume {
            up: i == 0,
            step: step as u8,
        }))
    } else {
        Ok(Some(if i == 2 {
            Action::OutputMute
        } else {
            Action::MicrophoneMute
        }))
    }
}

fn mapping_label(mapping: &Mapping, program: Option<&(u8, crate::feedback::Payload)>) -> String {
    let invalid = match &mapping.action {
        crate::actions::Action::Prompt {
            target: crate::actions::Target::Agent(agent),
            ..
        } => mappings::config_path()
            .and_then(|p| mappings::load_config(&p))
            .is_ok_and(|c| !c.agents.iter().any(|a| &a.name == agent)),
        _ => false,
    };
    format!(
        "{} | {} | {} | {}{}{}{} | {} | Feedback: {}",
        mappings::program_label(mapping.context),
        mapping.name(program),
        mapping.input.description(),
        mapping.action_label(),
        if invalid {
            " [INVALID: agent removed]"
        } else {
            ""
        },
        if mapping.risky {
            " | RISKY: hold 1 s"
        } else {
            ""
        },
        if mapping.status {
            " | Status feedback"
        } else {
            ""
        },
        mapping.behavior,
        mapping.feedback
    )
}

// Save first, then replace the session copy: a failed write changes neither one.
fn commit(
    path: &Path,
    current: &mut Vec<Mapping>,
    pad: Control,
    context: u8,
    replacement: Option<Mapping>,
) -> Result<()> {
    let mut next = current.clone();
    next.retain(|mapping| mapping.context != context || mapping.control != pad);
    if let Some(mapping) = &replacement {
        if matches!(
            mapping.input,
            mappings::Input::Knob { .. }
                | mappings::Input::RelativeKnob { .. }
                | mappings::Input::Joystick { .. }
        ) {
            for other in &mut next {
                if other.context == context
                    && other.control.message == pad.message
                    && other.control.channel == pad.channel
                    && other.control.id == pad.id
                {
                    other.input = mapping.input;
                }
            }
        }
    }
    next.extend(replacement);
    next.sort_by_key(|mapping| (mapping.context, mapping.control));
    mappings::save(path, &next)?;
    *current = next;
    Ok(())
}

fn configure_choice(ui: &mut impl Ui) -> Result<Option<usize>> {
    let labels = [
        "Learn a pad",
        "Learn a piano key",
        "Learn a knob",
        "Learn joystick",
        "Edit saved assignment",
        "Remove saved assignment",
        "Finish",
    ];
    let mut cursor: usize = 0;
    loop {
        let width = ui.width().saturating_sub(5);
        let item = |index| {
            format!(
                "{} {}",
                if index == cursor { ">" } else { " " },
                labels[index]
            )
        };
        let mut rows = vec!["Configure controls".to_string()];
        if ui.height() < 18 {
            let visible = ui.height().saturating_sub(10).max(1);
            let first = cursor.saturating_sub(visible - 1);
            rows.extend((first..7).take(visible).map(item));
        } else if width >= 66 && ui.height() >= 19 {
            let left = (width - 3) / 2;
            let right = width - left - 3;
            let border = |size| "─".repeat(size - 2);
            rows.push(String::new());
            rows.push(format!("╭{}╮   ╭{}╮", border(left), border(right)));
            for (a, b) in [
                ("Learn controls".into(), "Saved assignments".into()),
                (item(0), item(4)),
                (item(1), item(5)),
                (item(2), String::new()),
                (item(3), String::new()),
            ] {
                rows.push(format!(
                    "│{}│   │{}│",
                    crate::screen::fit(&format!(" {a}"), left - 2),
                    crate::screen::fit(&format!(" {b}"), right - 2)
                ));
            }
            rows.push(format!("╰{}╯   ╰{}╯", border(left), border(right)));
        } else {
            if ui.height() >= 22 {
                rows.push("Learn controls".into());
            }
            rows.extend((0..4).map(item));
            rows.push("─ Saved assignments ─".into());
            rows.extend((4..6).map(item));
        }
        if ui.height() >= 18 {
            rows.resize(ui.height().saturating_sub(10), String::new());
            let finish = item(6);
            rows.push(format!(
                "{}{}",
                " ".repeat(width.saturating_sub(finish.len()) / 2),
                finish
            ));
        }
        rows.push("Tab / Left / Right: switch pane".into());
        ui.draw(&rows.join("\n"))?;
        match ui.key()? {
            Some(Key::Up) => cursor = (cursor + 6) % 7,
            Some(Key::Down) => cursor = (cursor + 1) % 7,
            Some(Key::Tab) => {
                cursor = if cursor < 4 {
                    4
                } else if cursor < 6 {
                    6
                } else {
                    0
                }
            }
            Some(Key::Left) => cursor = if cursor == 5 { 1 } else { 0 },
            Some(Key::Right) => cursor = if cursor == 1 { 5 } else { 4 },
            Some(Key::Enter) => return Ok(Some(cursor)),
            Some(Key::Escape) => return Ok(None),
            _ => {}
        }
    }
}

/// Configuration failure stages (SPEC: a failure names its stage).
const STAGES: [&str; 3] = ["validation", "controller I/O", "file I/O"];

/// Prefixes an error with the stage it came from.
fn stage<E: std::fmt::Display>(name: &'static str) -> impl Fn(E) -> Box<dyn std::error::Error> {
    move |error| format!("{name}: {error}").into()
}

/// Action families (indexes into `FAMILIES`) a control can use; others are not offered.
fn families(input: mappings::Input) -> Vec<usize> {
    match input {
        mappings::Input::Pad | mappings::Input::Piano => (0..6).collect(),
        mappings::Input::Knob { .. } => vec![0, 2, 6, 7],
        mappings::Input::RelativeKnob { .. } => vec![0, 2],
        mappings::Input::Joystick { .. } => vec![0],
    }
}

const FAMILIES: [&str; 8] = [
    "Keyboard shortcut",
    "Launch application",
    "Fedora system action",
    "Prompt",
    "Run command in tmux",
    "Sequence of steps",
    "Choice knob (one prompt per zone)",
    "Value knob (number written to a file)",
];

fn configure(ui: &mut impl Ui, path: &Path, current: &mut Vec<Mapping>) -> Result<()> {
    configure_steps(ui, path, current).map_err(|error| {
        let text = error.to_string();
        if STAGES.iter().any(|s| text.starts_with(&format!("{s}: "))) {
            error
        } else {
            stage("validation")(error)
        }
    })
}

fn configure_steps(ui: &mut impl Ui, path: &Path, current: &mut Vec<Mapping>) -> Result<()> {
    loop {
        let Some(action) = configure_choice(ui)? else {
            return Ok(());
        };
        if action == 6 {
            return Ok(());
        }
        let motion = match action {
            2 => ui.learn_knob().map_err(stage("controller I/O"))?,
            3 => ui.learn_joystick().map_err(stage("controller I/O"))?,
            _ => None,
        };
        if matches!(action, 2 | 3) && motion.is_none() {
            continue;
        }
        let pad = if let Some((control, _, _)) = &motion {
            *control
        } else if action == 0 || action == 1 {
            let Some(pad) = (if action == 1 {
                ui.learn_piano()
            } else {
                ui.learn()
            })
            .map_err(stage("controller I/O"))?
            else {
                continue;
            };
            pad
        } else {
            let Some(index) = choose(
                ui,
                "Saved assignments",
                &current
                    .iter()
                    .filter(|m| m.context == ui.context())
                    .map(|m| mapping_label(m, ui.program().as_ref()))
                    .collect::<Vec<_>>(),
            )?
            else {
                continue;
            };
            current
                .iter()
                .filter(|m| m.context == ui.context())
                .nth(index)
                .unwrap()
                .control
        };
        let existing = current
            .iter()
            .find(|m| m.context == ui.context() && m.control == pad)
            .map(|m| mapping_label(m, ui.program().as_ref()))
            .unwrap_or_else(|| {
                let name = mappings::control_name(pad, None, ui.context(), ui.program().as_ref());
                format!("{name}: unassigned")
            });
        if action == 5 {
            if choose(
                ui,
                &format!("Remove {existing}?"),
                &options(&["Cancel", "Remove assignment"]),
            )? == Some(1)
            {
                commit(path, current, pad, ui.context(), None).map_err(stage("file I/O"))?;
            }
            continue;
        }
        let saved = current
            .iter()
            .find(|m| m.context == ui.context() && m.control == pad)
            .cloned();
        let input = motion
            .as_ref()
            .map(|(_, input, _)| *input)
            .or_else(|| {
                current
                    .iter()
                    .find(|m| m.context == ui.context() && m.control == pad)
                    .map(|m| m.input)
            })
            .unwrap_or(if action == 1 {
                mappings::Input::Piano
            } else {
                mappings::Input::Pad
            });
        if (action == 1 && input != mappings::Input::Piano)
            || (action == 0 && input != mappings::Input::Pad)
        {
            return Err("pad/piano source collision; this emitted note is already assigned as another control family".into());
        }
        if input == mappings::Input::Piano {
            let config = mappings::load_config(path).map_err(stage("file I/O"))?;
            // Name what carries the octave/tempo/arpeggiator feedback, so the user knows what to change.
            let mut fed = current
                .iter()
                .filter(|m| m.context == ui.context() && m.feedback.enabled())
                .map(|m| m.label.clone())
                .collect::<Vec<_>>();
            if config.idle_for(ui.context()).enabled() {
                fed.insert(0, "the idle settings".into());
            }
            if !fed.is_empty() {
                // Refused before any change: a note, not an operation failure that pauses.
                choose(
                    ui,
                    &format!(
                        "Not saved: validation: piano keys cannot be mapped in {}: octave/tempo/arpeggiator feedback comes from {}. Remove that feedback or use another program.",
                        mappings::program_label(ui.context()),
                        fed.join(", ")
                    ),
                    &options(&["Back"]),
                )?;
                continue;
            }
        }
        // Learning an assigned control replaces it only after an explicit choice (ticket 05).
        if action != 4
            && saved.is_some()
            && choose(
                ui,
                &format!(
                    "Already assigned: {existing}\nReplace it? Nothing changes until you save."
                ),
                &options(&["Cancel", "Replace"]),
            )? != Some(1)
        {
            continue;
        }
        let saved_family = saved.as_ref().map(|m| match &m.action {
            crate::actions::Action::Shortcut => 0,
            crate::actions::Action::Prompt { .. } => 3,
            crate::actions::Action::Command { .. } => 4,
            crate::actions::Action::Sequence(_) => 5,
            crate::actions::Action::Choice(_) => 6,
            crate::actions::Action::Value { .. } => 7,
            action if action.audio() => 2,
            _ => 1,
        });
        let offered = families(input);
        let Some(family) = choose_saved(
            ui,
            "Action family",
            offered.iter().map(|f| FAMILIES[*f].to_string()).collect(),
            saved_family.and_then(|f| offered.iter().position(|o| *o == f)),
        )?
        .map(|i| offered[i]) else {
            continue;
        };
        let mut mapping = if family == 0 {
            let Some(keys) = shortcut(ui, saved.as_ref().map_or(&[], |m| &m.keys))? else {
                continue;
            };
            let behavior = match input {
                mappings::Input::Knob { .. } | mappings::Input::RelativeKnob { .. } => "pulse",
                mappings::Input::Joystick { .. } => "hold",
                _ => {
                    let saved_behavior = saved.as_ref().and_then(|m| match m.behavior {
                        mappings::Behavior::Hold => Some(0),
                        mappings::Behavior::Toggle => Some(1),
                        _ => None,
                    });
                    let Some(b) = choose_saved(
                        ui,
                        "Activation behavior",
                        options(&["hold", "toggle"]),
                        saved_behavior,
                    )?
                    else {
                        continue;
                    };
                    if b == 0 { "hold" } else { "toggle" }
                }
            };
            Mapping::new(pad, &keys.join("+"), behavior)?
        } else if family == 3 {
            if !matches!(input, mappings::Input::Pad | mappings::Input::Piano) {
                return Err("prompts require a pad or piano press".into());
            }
            let Some(action) = prompt_action(ui, path, saved.as_ref().map(|m| &m.action))? else {
                continue;
            };
            Mapping::new_action(pad, input, action)?
        } else if family == 4 {
            if !matches!(input, mappings::Input::Pad | mappings::Input::Piano) {
                return Err("commands require a pad or piano press".into());
            }
            let Some(action) = command_action(ui, saved.as_ref().map(|m| &m.action))? else {
                continue;
            };
            Mapping::new_action(pad, input, action)?
        } else if family == 5 {
            if !matches!(input, mappings::Input::Pad | mappings::Input::Piano) {
                return Err("sequences require a pad or piano press".into());
            }
            let steps = match saved.as_ref().map(|m| &m.action) {
                Some(crate::actions::Action::Sequence(steps)) => steps.clone(),
                _ => Vec::new(),
            };
            let Some(steps) = sequence_menu(ui, path, steps)? else {
                continue;
            };
            Mapping::new_action(pad, input, crate::actions::Action::Sequence(steps))?
        } else if family >= 6 {
            if !matches!(input, mappings::Input::Knob { .. }) {
                return Err("choice and value actions need an absolute knob".into());
            }
            let saved_action = saved.as_ref().map(|m| &m.action);
            let Some(action) = (if family == 6 {
                choice_menu(ui, path, saved_action)?
            } else {
                value_menu(ui, saved_action)?
            }) else {
                continue;
            };
            Mapping::new_action(pad, input, action)?
        } else {
            let Some(action) =
                action_menu(ui, family, input, saved.as_ref().map(|m| &m.action), true)?
            else {
                continue;
            };
            Mapping::new_action(pad, input, action)?
        };
        mapping.context = ui.context();
        mapping.input = input;
        if let (mappings::Input::Knob { step }, false) = (input, mapping.action.dial()) {
            let Some(step) = number(
                ui,
                "Absolute knob movement step (one pulse maximum per MIDI sample)",
                i32::from(step),
                1,
                127,
            )?
            else {
                continue;
            };
            mapping.input = mappings::Input::Knob { step: step as u8 };
        }
        if let mappings::Input::RelativeKnob { step } = input {
            let Some(step) = number(ui, "Relative knob pulses per tick", i32::from(step), 1, 8)?
            else {
                continue;
            };
            mapping.input = mappings::Input::RelativeKnob { step: step as u8 };
        }
        mapping.label = if let Some((_, _, label)) = &motion {
            label.clone()
        } else if let Some(old) = current
            .iter()
            .find(|m| m.context == ui.context() && m.control == pad)
        {
            old.label.clone()
        } else {
            if mapping.input == mappings::Input::Piano {
                format!("Piano channel {} note {}", pad.channel, pad.id)
            } else {
                let Some(label) = ui.label(pad, current)? else {
                    continue;
                };
                label
            }
        };
        let previous = current
            .iter()
            .find(|m| m.context == ui.context() && m.control == pad)
            .map(|m| m.feedback)
            .unwrap_or_default();
        let Some(feedback) = (if mapping.action != crate::actions::Action::Shortcut
            || mapping.input != mappings::Input::Pad
            || mapping.context > 7
        {
            Some(crate::feedback::Feedback::default())
        } else {
            feedback_menu(ui, previous, saved.is_some(), false)?
        }) else {
            continue;
        };
        mapping.feedback = feedback;
        let name = window_name(&mapping.name(ui.program().as_ref()));
        match &mut mapping.action {
            crate::actions::Action::Command { window, .. } => *window = name,
            crate::actions::Action::Sequence(steps) => {
                for step in steps {
                    if let crate::actions::Step::Do(crate::actions::Action::Command {
                        window,
                        ..
                    }) = step
                    {
                        *window = name.clone();
                    }
                }
            }
            _ => {}
        }
        if matches!(mapping.input, mappings::Input::Pad | mappings::Input::Piano) {
            let Some(risky) = choose_saved(
                ui,
                "Fire when",
                options(&[
                    "Pressed",
                    "Held for 1 second (risky; early release sends nothing)",
                ]),
                saved.as_ref().map(|m| usize::from(m.risky)),
            )?
            else {
                continue;
            };
            mapping.risky = risky == 1;
        }
        if mapping.action.observable() {
            let Some(status) = choose_saved(
                ui,
                "Status feedback in the live pane (sent, running, stopped, failed)",
                options(&["Off", "On"]),
                saved.as_ref().map(|m| usize::from(m.status)),
            )?
            else {
                continue;
            };
            mapping.status = status == 1;
        }
        mapping.validate()?;
        // A directory's ${NAME} must resolve now, not when the pad is pressed (ticket 12).
        let config = mappings::load_config(path).map_err(stage("file I/O"))?;
        for (_, dir) in mapping.action.commands() {
            mappings::absolute_directory(dir, &config)?;
        }
        match choose(
            ui,
            &format!("Review: {}", mapping_label(&mapping, ui.program().as_ref())),
            &options(&["Save and configure another", "Save and finish", "Cancel"]),
        )? {
            Some(choice @ (0 | 1)) => {
                commit(path, current, pad, ui.context(), Some(mapping))
                    .map_err(stage("file I/O"))?;
                if choice == 1 {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

pub fn daemon(path: &Path, current: Vec<Mapping>) -> Result<()> {
    let mut ui = Terminal::open_mode(path, true)?;
    let config = mappings::load_config(path)?;
    ui.idle = config.idle;
    ui.start_flow_enabled = config.start_flow;
    ui.run_mappings = current.into_iter().filter(|m| m.context == 0).collect();
    ui.context_verified = false;
    ui.follow_programs = true;
    let flow_ready = match ui.startup_flow() {
        Ok(()) => true,
        Err(error) => {
            ui.pause()?;
            ui.error = format!("Automatic Flow startup failed: {error}");
            eprintln!("{}", ui.error);
            false
        }
    };
    if ui.flow_missing {
        eprintln!("Wispr Flow not installed; automatic startup skipped.");
    }
    if flow_ready {
        ui.wanted_run = true;
    }
    ui.at_menu = true;
    let mut previous = String::new();
    while !ui.stop.load(Ordering::Relaxed) {
        if let Err(error) = ui.tick() {
            if ui.stop.load(Ordering::Relaxed) {
                break;
            }
            return Err(error);
        }
        let status = format!(
            "Program {} {} | {} | {} | {}",
            ui.context + 1,
            if ui.context_verified {
                "verified"
            } else {
                "unverified"
            },
            ui.output
                .as_ref()
                .map(|o| o.keyboard.status(|control| ui.name(control)))
                .unwrap_or_default(),
            ui.notice,
            ui.error,
        ) + &ui
            .status_lines()
            .iter()
            .map(|line| format!(" | {line}"))
            .collect::<String>(); // no screen in the service: status feedback goes to the journal
        if status != previous {
            eprintln!("{status}");
            previous = status;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    ui.pause()
}

// Command pane groups (SPEC-UI). Numbers are the command ids `start` dispatches on.
const COMMANDS: &[(&str, &[(usize, &str, &str)])] = &[
    (
        "Run",
        &[
            (
                1,
                "Run",
                "Enable the verified program's mappings; retain applicable safety confirmations.",
            ),
            (
                3,
                "/pause",
                "Release synthetic keys, cancel pending mapped actions, and remain paused.",
            ),
            (
                5,
                "/release-all",
                "Clear holds/toggles while retaining running mode.",
            ),
            (
                11,
                "/quit",
                "Release keys, attempt owned-feedback restoration, and restore the terminal.",
            ),
        ],
    ),
    (
        "Configure",
        &[
            (
                0,
                "/configure",
                "Learn controls; edit, save, or explicitly remove assignments.",
            ),
            (2, "/list", "Inspect saved mappings and toggle states."),
            (
                18,
                "Settings",
                "Default agent directory, Flow at launch, startup and user variables.",
            ),
            (
                17,
                "Agents",
                "Add, edit, or remove tmux agents; attach with tmux attach -t vibe.",
            ),
            (
                15,
                "Reset settings",
                "Back up and clear every program's mappings and idle feedback.",
            ),
        ],
    ),
    (
        "Programs",
        &[
            (
                10,
                "/prog-select",
                "Select and verify an actual hardware program.",
            ),
            (
                14,
                "/program-name",
                "Review, rename, and verify a stored program name.",
            ),
        ],
    ),
    (
        "Feedback",
        &[
            (
                9,
                "/feedback-idle",
                "Configure the selected program's idle musical settings.",
            ),
            (
                8,
                "/feedback-check",
                "Run the bounded original-program feedback/restoration trial.",
            ),
        ],
    ),
    (
        "Wispr Flow",
        &[
            (
                16,
                "Start Wispr Flow with VibeConsole",
                "Toggle automatic Flow startup for interactive sessions and the login service.",
            ),
            (
                12,
                "Start or repair Wispr Flow",
                "Starts Flow if it is not running, or quits and reopens it once if it is, then checks it opened VibeConsole's keyboard. Output pauses meanwhile and resumes on success.",
            ),
        ],
    ),
    (
        "Inspect",
        &[
            (
                7,
                "/detect",
                "Show paged Note activity, identities, and events without mapped effects.",
            ),
            (
                6,
                "/get",
                "Inspect an observed Note control in the selected context.",
            ),
        ],
    ),
];

/// Command pane rows: a heading has no command; others carry (id, description).
fn command_rows(running: bool) -> Vec<(&'static str, Option<(usize, &'static str)>)> {
    let mut rows = Vec::new();
    for (heading, commands) in COMMANDS {
        rows.push((*heading, None));
        rows.extend(commands.iter().map(|(id, name, about)| {
            if *id == 1 && running {
                (
                    "Pause",
                    Some((*id, "Release synthetic keys and pause output.")),
                )
            } else {
                (*name, Some((*id, *about)))
            }
        }));
    }
    rows
}

/// Highlighted command row; Up/Down skip headings and wrap. Esc quits as before.
fn command_menu(ui: &mut Terminal) -> Result<Option<usize>> {
    loop {
        let running = ui.wanted_run;
        let rows = command_rows(running);
        let Some((command, about)) = rows[ui.menu].1 else {
            ui.menu = (ui.menu + 1) % rows.len();
            continue;
        };
        let about = if command == 16 {
            if ui.start_flow_enabled {
                "On. Enter to turn off automatic Flow startup on the next launch."
            } else {
                "Off. Enter to turn on automatic Flow startup on the next launch."
            }
        } else {
            about
        };
        ui.draw(&format!("{}\n\n{about}\n\nEnter: open", rows[ui.menu].0))?;
        let step = match ui.key()? {
            Some(Key::Up) => rows.len() - 1,
            Some(Key::Down) => 1,
            Some(Key::Enter) if ui.wanted_run == running => return Ok(Some(command)),
            Some(Key::Escape) => return Ok(None),
            _ => continue,
        };
        loop {
            ui.menu = (ui.menu + step) % rows.len();
            if rows[ui.menu].1.is_some() {
                break;
            }
        }
    }
}

pub fn start(path: &Path, mut current: Vec<Mapping>, initial: &str) -> Result<()> {
    let mut ui = Terminal::open(path)?;
    let config = mappings::load_config(path)?;
    ui.idle = config.idle;
    ui.start_flow_enabled = config.start_flow;
    ui.run_mappings = current
        .iter()
        .filter(|mapping| mapping.context == ui.context)
        .cloned()
        .collect();
    if initial == "detect" {
        return ui.detect();
    }
    if initial == "configure" {
        return configure(&mut ui, path, &mut current);
    }
    ui.context_verified = false;
    ui.follow_programs = true;
    let flow_ready = match ui.startup_flow() {
        Ok(()) => true,
        Err(error) => {
            ui.pause()?;
            ui.error = format!("Automatic Flow startup failed: {error}");
            false
        }
    };
    if (initial == "run" || initial == "session") && flow_ready {
        ui.wanted_run = true;
        ui.notice = "Detecting the controller’s current program before enabling actions.".into();
    }
    loop {
        ui.run_mappings = current
            .iter()
            .filter(|mapping| mapping.context == ui.context)
            .cloned()
            .collect();
        ui.at_menu = true;
        let Some(command) = command_menu(&mut ui)? else {
            return Ok(());
        };
        ui.at_menu = false;
        let was_running = ui.wanted_run;
        let result = match command {
            0 => (|| {
                ui.suspend()?;
                configure(&mut ui, path, &mut current)?;
                // A reassigned pad must not keep its old action's status.
                ui.phases.clear();
                ui.probed.clear();
                ui.run_mappings = current
                    .iter()
                    .filter(|mapping| mapping.context == ui.context)
                    .cloned()
                    .collect();
                if let Some(output) = &mut ui.output {
                    output.keyboard.replace(&ui.run_mappings)?;
                }
                Ok(())
            })(),
            1 if ui.wanted_run => ui.pause(),
            1 if !ui.context_verified => choose(
                &mut ui,
                "Select /prog-select before running.\nChoose the matching hardware program, then select Run. No mapped actions are enabled yet.",
                &options(&["Back"]),
            ).map(|_| ()),
            1 => ui.run(
                &current
                    .iter()
                    .filter(|m| m.context == ui.context)
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
            2 => {
                let labels = current
                    .iter()
                    .map(|mapping| {
                        let label = mapping_label(mapping, ui.program().as_ref());
                        if mapping.behavior == mappings::Behavior::Toggle {
                            let active = mapping.context == ui.context
                                && ui
                                    .output
                                    .as_ref()
                                    .is_some_and(|output| output.keyboard.active(mapping.control));
                            format!("{} {label}", crate::screen::bulb(active))
                        } else {
                            label
                        }
                    })
                    .collect::<Vec<_>>();
                choose(&mut ui, "Saved assignments", &labels).map(|_| ())
            }
            3 => ui.pause(),
            5 => ui.release_all(),
            6 => ui.learn().and_then(|pad| {
                if let Some(pad) = pad {
                    let text = current
                        .iter()
                        .find(|m| m.context == ui.context() && m.control == pad)
                        .map(|m| mapping_label(m, ui.program().as_ref()))
                        .unwrap_or_else(|| "Unassigned".into());
                    choose(&mut ui, &text, &options(&["Back"]))?;
                }
                Ok(())
            }),
            7 => ui.detect(),
            8 => ui.feedback_trial(),
            9 => (|| {
                ui.suspend()?;
                let initial = crate::feedback::Feedback {
                    octave: ui.idle.octave.map(crate::feedback::Octave::Absolute),
                    tempo: ui.idle.tempo,
                    arp: ui.idle.arp,
                };
                let Some(feedback) = feedback_menu(&mut ui, initial, true, true)? else {
                    return Ok(());
                };
                let idle = crate::feedback::Settings {
                    octave: feedback.octave.map(|o| match o {
                        crate::feedback::Octave::Absolute(n)
                        | crate::feedback::Octave::Offset(n) => n,
                    }),
                    tempo: feedback.tempo,
                    arp: feedback.arp,
                }
                .validate()?;
                if choose(
                    &mut ui,
                    &format!("Save idle feedback: {feedback}?"),
                    &options(&["Cancel", "Save"]),
                )? == Some(1)
                {
                    ui.retry = None;
                    ui.connecting = false;
                    let mut config = mappings::load_config(path)?;
                    if config.mappings != current {
                        return Err(
                            "Mappings changed externally; reload before saving idle feedback"
                                .into(),
                        );
                    }
                    config.set_idle(ui.context, idle)?;
                    mappings::save_config(path, &config)?;
                    ui.idle = idle;
                    if let Some(mut controller) = ui.controller.take() {
                        ui.resolver = None;
                        ui.error = controller.finish()?;
                        ui.input = None;
                        ui.reconnected = ui.output.is_some();
                    }
                }
                Ok(())
            })(),
            10 => (|| {
                ui.program_connection()?;
                let contexts =
                    crate::feedback::names(ui.input.as_mut().ok_or("MIDI disconnected")?)?;
                ui.program_names = contexts.clone();
                let selected_program = usize::from(ui.context);
                if let Some(context) = choose_at(
                    &mut ui,
                    "/prog-select — select the actual hardware preset.
Selection replaces current RAM with the stored preset.
Running/paused mode is retained; held keys and toggles start inactive.",
                    &contexts,
                    selected_program,
                )? {
                    ui.context_verified = false;
                    ui.feedback_allowed = false;
                    let input = ui
                        .input
                        .as_mut()
                        .ok_or("MIDI disconnected during program selection")?;
                    let snapshot = crate::feedback::select(input, context as u8)?;
                    // Bound draining even if the controller generates continuous arpeggiator traffic.
                    for _ in 0..1024 {
                        if input.next()?.is_none() {
                            break;
                        }
                    }
                    if input.next()?.is_some() {
                        return Err(
                            "Continuous MIDI during selection; stop input and select again".into(),
                        );
                    }
                    ui.program_snapshot = Some(snapshot);
                    ui.context = context as u8;
                    ui.context_verified = true;
                    ui.notice.clear();
                    ui.feedback_allowed = true;
                    ui.idle = mappings::load_config(path)?.idle_for(ui.context);
                    ui.run_mappings = current
                        .iter()
                        .filter(|m| m.context == ui.context)
                        .cloned()
                        .collect();
                    if let Some(output) = &mut ui.output {
                        output.keyboard.replace(&ui.run_mappings)?;
                        }
                    ui.reconnected = true;
                }
                Ok(())
            })(),
            12 => ui.start_flow(),
            14 => ui.rename_program(),
            15 => (|| {
                if !reset_confirmation(&mut ui)? {
                    return Ok(());
                }
                ui.reset_confirmed(&mut current)?;
                let notice = ui.notice.clone();
                choose(&mut ui, &notice, &options(&["Continue"]))?;
                Ok(())
            })(),
            16 => (|| {
                let mut config = mappings::load_config(path)?;
                config.start_flow = !config.start_flow;
                mappings::save_config(path, &config)?;
                ui.start_flow_enabled = config.start_flow;
                ui.notice = format!(
                    "Start Wispr Flow with VibeConsole: {} (takes effect next launch).",
                    if config.start_flow { "On" } else { "Off" }
                );
                Ok(())
            })(),
            17 => (|| { ui.suspend()?; agents_menu(&mut ui, path) })(),
            18 => (|| {
                ui.suspend()?;
                settings_menu(&mut ui, path)?;
                ui.start_flow_enabled = mappings::load_config(path)?.start_flow;
                Ok(())
            })(),
            _ => return Ok(()),
        };
        let result = result.and_then(|()| {
            if ui.context_verified
                && was_running
                && matches!(command, 0 | 6 | 7 | 8 | 9 | 10 | 14 | 17 | 18)
            {
                ui.run_mode(
                    &current
                        .iter()
                        .filter(|m| m.context == ui.context)
                        .cloned()
                        .collect::<Vec<_>>(),
                    false,
                )?;
            } else if refresh_idle_after_menu(command, was_running, ui.context_verified) {
                // Menus may restore the original hardware baseline; paused mode still owns its idle setup.
                ui.selected_idle()?;
            }
            Ok(())
        });
        if let Err(error) = result {
            if matches!(command, 8 | 9 | 10 | 14) {
                ui.context_verified = false;
                ui.feedback_allowed = false;
                ui.program_snapshot = None;
            }
            let cleanup = ui.pause();
            if ui.stop.load(Ordering::Relaxed) {
                return Err(error);
            }
            ui.error = format!(
                "{error}{}",
                cleanup
                    .err()
                    .map(|e| format!("; cleanup failed: {e}"))
                    .unwrap_or_default()
            );
            let text = format!(
                "Operation failed: {}\nSaved configuration preserved; held keys released; paused. Fix the cause, then select Run.",
                ui.error
            );
            choose(&mut ui, &text, &options(&["Back"]))?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_relative_knob_requires_one_source_and_both_signed_directions() {
        let ticks = [
            [0xb0, 16, 1],
            [0xb0, 16, 2],
            [0xb0, 16, 127],
            [0xb0, 16, 126],
        ];
        assert_eq!(relative_source(&ticks).unwrap(), (1, 16));
        confirm_relative_ticks(&ticks, 1, 16).unwrap();
        assert!(confirm_relative_ticks(&ticks[..2], 1, 16).is_err());
        assert!(confirm_relative_ticks(&[[0xb0, 16, 1], [0xb0, 17, 127]], 1, 16).is_err());
        assert!(
            confirm_relative_ticks(&[[0xb0, 16, 1], [0xb0, 16, 64], [0xb0, 16, 127]], 1, 16)
                .is_err()
        );
    }
    use std::collections::VecDeque;
    #[test]
    fn service_follows_verified_programs_and_leaves_unmatched_or_piano_inactive() {
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-service-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        mappings::save(&path, &[]).unwrap();
        let mut ui = Terminal::open_mode(&path, true).unwrap();
        ui.at_menu = true;
        ui.follow_programs = true;
        ui.context_verified = false;
        ui.wanted_run = true;
        let observation = |context| crate::feedback::ProgramObservation::Detected {
            context,
            payload: [0; 245],
            names: (1..=8).map(|i| format!("Program {i}")).collect(),
        };
        assert!(!ui.feedback_allowed);
        ui.observe_program(observation(Some(0))).unwrap();
        assert_eq!(ui.context, 0);
        assert!(ui.context_verified && ui.feedback_allowed && ui.wanted_run);
        ui.observe_program(observation(Some(2))).unwrap();
        assert_eq!(ui.context, 2);
        assert!(ui.context_verified && ui.feedback_allowed && ui.wanted_run);
        ui.observe_program(observation(None)).unwrap();
        assert!(!ui.context_verified && !ui.feedback_allowed);
        assert!(ui.notice.contains("service output stays inactive"));

        let mut piano = Mapping::new(Control::note(1, 60).unwrap(), "Shift", "hold").unwrap();
        piano.context = 1;
        piano.input = mappings::Input::Piano;
        mappings::save(&path, &[piano]).unwrap();
        let mut ui = Terminal::open_mode(&path, true).unwrap();
        ui.at_menu = true;
        ui.follow_programs = true;
        ui.context_verified = false;
        ui.wanted_run = true;
        ui.observe_program(observation(Some(1))).unwrap();
        assert!(ui.wanted_run && ui.suspended && ui.output.is_none());
        assert!(ui.notice.contains("interactive Run confirmation required"));
        ui.observe_program(observation(Some(0))).unwrap();
        assert!(ui.wanted_run && !ui.suspended && ui.context_verified);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn modified_navigation_keys_decode_without_changing_text_keys() {
        for (sequence, key) in [
            (&b"1;2A"[..], Key::Up),
            (&b"1;5B"[..], Key::Down),
            (&b"1;3C"[..], Key::Right),
            (&b"13;5u"[..], Key::Enter),
            (&b"32;3u"[..], Key::Space),
            (&b"27;2u"[..], Key::Escape),
            (&b"27;5;13~"[..], Key::Enter),
            (&b"27;3;32~"[..], Key::Space),
            (&b"27;2;27~"[..], Key::Escape),
        ] {
            assert_eq!(csi_key(sequence), key);
        }
        assert_eq!(csi_key(b"65;2u"), Key::Other); // printable text is not rewritten
        assert_eq!(csi_key(b"1;9A"), Key::Other);
    }
    #[derive(Default)]
    struct Script {
        keys: VecDeque<Key>,
        pads: VecDeque<Control>,
        recordings: VecDeque<Result<Vec<String>>>,
        screens: Vec<String>,
        /// Knob/joystick learning results, in order; empty means learning fails.
        motions: VecDeque<(Control, mappings::Input, String)>,
        /// The editor fails after writing part of the file.
        edit_fails: bool,
    }
    impl Ui for Script {
        fn applications(&self) -> Result<Vec<(String, String)>> {
            Ok(vec![(
                "Recorded app".into(),
                "/nonexistent/recorded.desktop".into(),
            )])
        }
        fn draw(&mut self, text: &str) -> Result<()> {
            self.screens.push(text.into());
            Ok(())
        }
        fn key(&mut self) -> Result<Option<Key>> {
            Ok(Some(self.keys.pop_front().expect("script exhausted")))
        }
        fn record(&mut self) -> Result<Vec<String>> {
            self.recordings
                .pop_front()
                .expect("recording script exhausted")
        }
        fn learn(&mut self) -> Result<Option<Control>> {
            Ok(self.pads.pop_front())
        }
        fn learn_piano(&mut self) -> Result<Option<Control>> {
            Ok(self.pads.pop_front())
        }
        fn learn_knob(&mut self) -> Result<Option<(Control, mappings::Input, String)>> {
            self.motions
                .pop_front()
                .map(Some)
                .ok_or_else(|| "MIDI input unavailable".into())
        }
        fn learn_joystick(&mut self) -> Result<Option<(Control, mappings::Input, String)>> {
            self.learn_knob()
        }
        fn edit_file(&mut self, path: &Path) -> Result<()> {
            if self.edit_fails {
                std::fs::write(path, "half-written")?;
                return Err("editor exited unsuccessfully".into());
            }
            Ok(std::fs::write(path, "Review this change.\n")?)
        }
    }
    #[test]
    fn detected_hardware_context_overrides_old_context_preserves_pause_and_refuses_ambiguity() {
        let directory =
            std::env::temp_dir().join(format!("vibeconsole-follow-ui-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("mappings.tsv");
        mappings::save(&path, &[]).unwrap();
        let mut ui = Terminal::open_mode(&path, true).unwrap();
        ui.at_menu = true;
        ui.context = 2; // last used program was 3; hardware now has Program 1
        ui.context_verified = false;
        ui.wanted_run = true;
        let observation = |context| crate::feedback::ProgramObservation::Detected {
            context,
            payload: [0; 245],
            names: (1..=8).map(|i| format!("Program {i}")).collect(),
        };
        ui.observe_program(observation(Some(0))).unwrap();
        assert_eq!(ui.context, 0);
        assert!(ui.context_verified && ui.wanted_run && !ui.suspended);
        assert!(ui.output.is_none()); // no mappings means no virtual keyboard needed
        ui.pause().unwrap();
        ui.observe_program(observation(Some(2))).unwrap();
        assert_eq!(ui.context, 2);
        assert!(ui.context_verified && !ui.wanted_run && ui.suspended);
        ui.observe_program(observation(None)).unwrap();
        assert!(!ui.context_verified && !ui.feedback_allowed);
        assert!(ui.notice.contains("no unique"));
        ui.at_menu = false;
        assert!(ui.observe_program(observation(Some(0))).is_err()); // interrupt a configuration menu
        assert!(!ui.context_verified);
        drop(ui);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn paused_verified_program_menus_reapply_idle_without_resuming_keys() {
        for command in [9, 10, 14] {
            assert!(refresh_idle_after_menu(command, false, true));
        }
        assert!(!refresh_idle_after_menu(10, true, true));
        assert!(!refresh_idle_after_menu(10, false, false));
        assert!(!refresh_idle_after_menu(3, false, true));
    }

    #[test]
    fn program_name_prompt_rejects_invalid_input_and_overlong_names_without_saving() {
        use Key::*;
        let mut invalid = Script {
            keys: [Character(b'C'), InvalidInput].into(),
            ..Default::default()
        };
        assert!(
            program_name_prompt(&mut invalid, "Program 1")
                .unwrap_err()
                .to_string()
                .contains("Unsupported")
        );
        let mut long = Script {
            keys: (0..17).map(|_| Character(b'A')).chain([Enter]).collect(),
            ..Default::default()
        };
        assert!(
            program_name_prompt(&mut long, "Program 1")
                .unwrap_err()
                .to_string()
                .contains("exceeds 16")
        );
        let mut blank = Script {
            keys: [Space, Enter].into(),
            ..Default::default()
        };
        assert!(
            program_name_prompt(&mut blank, "Program 1")
                .unwrap_err()
                .to_string()
                .contains("blank")
        );
        let mut cancel = Script {
            keys: [Escape].into(),
            ..Default::default()
        };
        assert_eq!(program_name_prompt(&mut cancel, "Program 1").unwrap(), None);
    }

    #[test]
    fn pad_label_picker_prevents_duplicate_save_and_keeps_programs_separate() {
        use Key::*;
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-pad-labels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let pad = Control::note(1, 22).unwrap();
        let mut saved = Mapping::new(Control::note(1, 21).unwrap(), "Shift", "hold").unwrap();
        saved.context = 1;
        saved.label = "Bank A Pad 1".into();
        let mut other_program = saved.clone();
        other_program.context = 0;
        other_program.label = "Bank B Pad 1".into();
        let mut current = vec![other_program, saved.clone()];
        mappings::save(&path, &current).unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut candidate = Mapping::new(pad, "Shift", "toggle").unwrap();
        candidate.context = 1;
        candidate.label = saved.label.clone();
        let error = commit(&path, &mut current, pad, 1, Some(candidate.clone())).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("duplicate physical label in context")
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let labels = pad_labels(pad, &current, 1);
        assert!(
            !labels.contains(&saved.label),
            "picker offers a label that fails on save"
        );
        assert!(labels.contains(&"Bank B Pad 1".into()));
        let mut ui = Script {
            keys: [Down, Enter].into(),
            ..Default::default()
        };
        candidate.label = labels[choose(&mut ui, "Confirm physical pad label", &labels)
            .unwrap()
            .unwrap()]
        .clone();
        commit(&path, &mut current, pad, 1, Some(candidate)).unwrap();
        assert_eq!(mappings::load(&path).unwrap(), current);
        assert!(current.contains(&saved));
        assert!(pad_labels(saved.control, &current, 1).contains(&saved.label));
        let original = Control::from_note(10, 36).unwrap();
        let labels = pad_labels(original, &[], 0);
        assert_eq!(
            labels
                .iter()
                .filter(|label| **label == original.label())
                .count(),
            1
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn detection_splits_banks_by_verified_program_and_falls_back_to_unidentified() {
        let mut ui =
            Terminal::open_mode(Path::new("/unused-vibeconsole-detect-test"), true).unwrap();
        let capture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/mpk-mini3-programs.hex"
        ));
        // Line 0 is RAM; line 2 is stored Program 2, where Bank B Pad 1 sends ch1 note 29.
        let payload = capture
            .lines()
            .filter(|l| !l.starts_with('#'))
            .nth(2)
            .unwrap();
        let payload = payload.split_whitespace();
        let payload = payload.map(|b| u8::from_str_radix(b, 16).unwrap());
        ui.program_snapshot = Some(payload.collect::<Vec<_>>().try_into().unwrap());
        ui.context = 1;
        for packet in [[0x90, 29, 100], [0x90, 60, 100]] {
            let event = ui.detector.observe(packet);
            ui.observe_light(event.as_ref(), Instant::now());
        }
        ui.cc_values.insert((1, 70), 64);
        let row_of = |view: &str, text: &str| {
            view.lines()
                .enumerate()
                .find_map(|(row, line)| line.find(text).map(|column| (row, column)))
                .unwrap_or_else(|| panic!("{text:?} missing from\n{view}"))
        };
        for (size, side_by_side) in [((120, 40), true), ((80, 40), false), ((170, 40), true)] {
            let view = ui.detection_view(size, true);
            let (a, b) = (row_of(&view, " Bank A "), row_of(&view, " Bank B "));
            assert_eq!(a.0 == b.0 && a.1 < b.1, side_by_side);
            assert!(side_by_side || a.0 < b.0);
            let lower = row_of(&view, "Piano · Knobs · Joystick");
            assert!(lower.0 > b.0);
            // Pads 5–8 sit above Pads 1–4, as on the controller.
            assert!(row_of(&view, "Pad 5").0 < row_of(&view, "Pad 1").0);
            // Bank B Pad 1 (#29) is the only lit pad; Bank A Pad 1 (#21) is not.
            let (on, b29, a21) = (
                row_of(&view, "💡 ON "),
                row_of(&view, "#29"),
                row_of(&view, "#21"),
            );
            assert!(on.0 + 1 == b29.0 && (on.0 > b.0 || on.1 >= b.1));
            assert_eq!(view.matches("💡 ON ").count(), 2); // the pad and piano note 60
            assert!(a21.0 == b29.0 || a21.0 < b.0);
            assert_eq!(view.contains("│ Pad 5  │"), size.0 != 120);
            assert!(view.contains("💡 ON  Note ch1 #60") && view.contains(": 64"));
            assert!(!view.contains("Unidentified"));
            let frame = ui.screen(&view).render(size.0, size.1, false);
            assert!(frame.contains("Bank B") && frame.contains("Pad 8"));
            assert!(
                ui.screen(&view)
                    .render(size.0, size.1, true)
                    .contains("\x1b[1;38;2;158;206;106mBank B")
            );
        }
        ui.context_verified = false;
        let view = ui.detection_view((120, 40), true);
        assert!(view.contains(" Unidentified ") && !view.contains("Bank A"));
        assert!(view.contains("💡 ON  Note ch1 #29") && view.contains("CC ch1 #70: 64"));
    }

    #[test]
    fn simulated_input_bulbs_cover_hold_fast_release_motion_and_disconnect_reset() {
        let mut ui =
            Terminal::open_mode(Path::new("/unused-vibeconsole-indicator-test"), true).unwrap();
        let now = Instant::now();
        let pad = Control::from_note(10, 36).unwrap();
        let piano = Control::note(1, 48).unwrap();
        assert!(!ui.input_light(now));
        let event = ui.detector.observe([0x99, 36, 100]);
        ui.observe_light(event.as_ref(), now);
        assert!(ui.note_light(pad, now + Duration::from_secs(2)));
        assert!(ui.input_light(now + Duration::from_secs(2)));
        let event = ui.detector.observe([0x99, 36, 0]);
        ui.observe_light(event.as_ref(), now + Duration::from_millis(10));
        assert!(ui.note_light(pad, now + Duration::from_millis(20)));
        assert!(!ui.note_light(pad, now + Duration::from_millis(250)));
        assert!(!ui.input_light(now + Duration::from_millis(250)));
        for packet in [[0x90, 48, 100], [0x80, 48, 0]] {
            let event = ui.detector.observe(packet);
            ui.observe_light(event.as_ref(), now);
        }
        assert!(ui.detector.down.is_empty());
        assert!(ui.note_light(piano, now));
        assert!(!ui.note_light(piano, now + Duration::from_millis(250)));
        let event = ui.detector.observe([0xb0, 70, 20]);
        ui.observe_light(event.as_ref(), now);
        assert!(ui.input_light(now));
        ui.reset_input_lights();
        assert!(!ui.input_light(now) && !ui.note_light(pad, now) && !ui.note_light(piano, now));
        assert!(ui.input.is_none() && ui.output.is_none());
    }

    #[test]
    fn configure_panes_keep_learning_order_and_keyboard_focus() {
        use Key::*;
        let mut ui = Script {
            keys: [Down, Right, Left, Tab, Tab, Enter].into(),
            ..Default::default()
        };
        assert_eq!(configure_choice(&mut ui).unwrap(), Some(6));
        let first = &ui.screens[0];
        let learning = [
            "Learn a pad",
            "Learn a piano key",
            "Learn a knob",
            "Learn joystick",
        ]
        .map(|label| first.find(label).unwrap());
        assert!(learning.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            first
                .lines()
                .any(|line| line.contains("Learn controls") && line.contains("Saved assignments"))
        );
        assert!(ui.screens[1].contains("> Learn a piano key"));
        assert!(ui.screens[2].contains("> Remove saved assignment"));
        assert!(ui.screens[3].contains("> Learn a piano key"));
        let finish = ui
            .screens
            .last()
            .unwrap()
            .lines()
            .find(|line| line.contains("> Finish"))
            .unwrap();
        assert!(finish.starts_with("                "));
        assert!(ui.keys.is_empty());
    }
    #[test]
    fn running_intent_survives_menu_suspension_but_explicit_pause_stays_paused() {
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-running-mode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        mappings::save(&path, &[]).unwrap();
        let mut ui = Terminal::open_mode(&path, true).unwrap();
        ui.run_mode(&[], false).unwrap();
        assert!(ui.wanted_run && !ui.suspended);
        ui.suspend().unwrap();
        assert!(ui.wanted_run && ui.suspended);
        ui.run_mode(&[], false).unwrap();
        assert!(ui.wanted_run && !ui.suspended);
        ui.context_verified = false;
        ui.suspend().unwrap();
        assert!(ui.wanted_run && ui.run_mode(&[], false).is_err());
        ui.context_verified = true;
        ui.run_mode(&[], false).unwrap();
        assert!(ui.wanted_run && !ui.suspended);
        ui.pause().unwrap();
        assert!(!ui.wanted_run);
        ui.suspend().unwrap();
        assert!(!ui.wanted_run); // selector/configuration must not undo explicit pause
        ui.run_mode(&[], false).unwrap();
        assert!(ui.wanted_run);
        assert_eq!(
            (0..8).map(mappings::program_label).collect::<Vec<_>>(),
            (1..=8).map(|n| format!("Program {n}")).collect::<Vec<_>>()
        );
        assert!(ui.output.is_none() && ui.input.is_none() && ui.controller.is_none());
        drop(ui);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn observed_motion_setup_requires_confirmation_and_preserves_axis_ambiguity() {
        use Key::*;
        let samples = vec![[0xb0, 70, 0], [0xb0, 70, 106], [0xb0, 70, 0]];
        let mut cancel = Script {
            keys: [Enter].into(),
            ..Default::default()
        };
        assert!(
            calibrate_motion(&mut cancel, samples.clone(), true)
                .unwrap()
                .is_none()
        );
        let mut ui = Script {
            keys: [Down, Enter, Enter, Enter].into(),
            ..Default::default()
        };
        let (control, input, label) = calibrate_motion(&mut ui, samples, true).unwrap().unwrap();
        assert_eq!(control.direction, 1);
        assert_eq!(input, mappings::Input::Knob { step: 4 });
        assert_eq!(label, "Knob 1 increase");
        let mut ui = Script {
            keys: [Down, Enter, Enter, Enter, Enter, Enter].into(),
            ..Default::default()
        };
        let (_, input, label) = calibrate_motion(
            &mut ui,
            vec![[0xb0, 1, 0], [0xb0, 1, 127], [0xb0, 1, 0]],
            false,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(input, mappings::Input::Joystick { center: 0, .. }));
        assert!(label.contains("directions share values"));
        let mut ui = Script {
            keys: [Down, Enter, Enter, Enter, Enter, Down, Enter].into(),
            ..Default::default()
        };
        let (_, input, label) = calibrate_motion(
            &mut ui,
            vec![[0xe0, 0, 0], [0xe0, 127, 127], [0xe0, 0, 64]],
            false,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            input,
            mappings::Input::Joystick { center: 8192, .. }
        ));
        assert_eq!(label, "Joystick Right");
        assert!(
            calibrate_motion(
                &mut Script::default(),
                vec![[0xb0, 70, 1], [0xb0, 71, 2]],
                true
            )
            .is_err()
        );
        assert!(
            calibrate_motion(
                &mut Script::default(),
                vec![[0xb0, 70, 1], [0xb0, 70, 2]],
                true
            )
            .is_err()
        );
    }

    #[test]
    fn editing_feedback_starts_on_current_values_and_keeps_them_on_enter() {
        use crate::feedback::{Feedback, Octave};
        use Key::*;
        let initial = Feedback {
            octave: Some(Octave::Absolute(1)),
            tempo: Some(240),
            arp: Some(true),
        };
        let mut ui = Script {
            keys: [
                Enter, Enter, Enter, Down, Enter, Enter, Enter, Down, Enter, Enter, Down, Enter,
            ]
            .into(),
            pads: VecDeque::new(),
            ..Default::default()
        };
        assert_eq!(
            feedback_menu(&mut ui, initial, true, false).unwrap(),
            Some(initial)
        );
        assert!(ui.keys.is_empty());
        for expected in [
            "> Absolute octave",
            "Value: 1",
            "> Set BPM",
            "Value: 240",
            "> On",
        ] {
            assert!(
                ui.screens.iter().any(|s| s.contains(expected)),
                "{expected}"
            );
        }
    }

    #[test]
    fn feedback_menus_couple_active_tempo_allow_stored_idle_and_select_relative_octave() {
        use Key::*;
        let mut ui = Script {
            keys: [
                Down, Down, Enter, Down, Enter, Up, Enter, Down, Enter, Up, Enter, Enter, Down,
                Enter,
            ]
            .into(),
            pads: VecDeque::new(),
            ..Default::default()
        };
        let feedback = feedback_menu(&mut ui, crate::feedback::Feedback::default(), false, false)
            .unwrap()
            .unwrap();
        assert_eq!(feedback.tempo, Some(121));
        assert_eq!(feedback.arp, Some(true));
        assert!(ui.keys.is_empty());
        ui.keys = [Down, Enter, Up, Enter, Down, Enter].into();
        let idle = feedback_menu(&mut ui, feedback, false, true)
            .unwrap()
            .unwrap();
        assert_eq!(idle.tempo, Some(121));
        assert_eq!(idle.arp, Some(false));
        ui.keys = [
            Down, Enter, Down, Down, Enter, Up, Enter, Down, Down, Down, Enter,
        ]
        .into();
        assert_eq!(
            feedback_menu(&mut ui, crate::feedback::Feedback::default(), false, false)
                .unwrap()
                .unwrap()
                .octave,
            Some(crate::feedback::Octave::Offset(1))
        );
        ui.keys = [Escape].into();
        assert!(
            feedback_menu(&mut ui, feedback, false, false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn recording_review_retry_cancel_and_permission_fallback_stay_outside_capture() {
        use Key::*;
        let mut ui = Script {
            keys: [Down, Enter, Down, Enter, Enter].into(),
            pads: VecDeque::new(),
            recordings: [
                Ok(vec!["Ctrl".into(), "K".into()]),
                Ok(vec!["Shift".into()]),
            ]
            .into(),
            ..Default::default()
        };
        assert_eq!(shortcut(&mut ui, &[]).unwrap().unwrap(), ["Shift"]);
        assert!(ui.recordings.is_empty());
        assert!(ui.keys.is_empty());
        ui.keys = [Down, Enter, Escape].into();
        ui.recordings = [Ok(vec!["Escape".into()])].into();
        assert!(shortcut(&mut ui, &[]).unwrap().is_none());
        assert!(ui.keys.is_empty());
        ui.keys = [Down, Enter, Enter, Enter, Escape].into();
        ui.recordings = [Err("permission denied".into())].into();
        assert!(shortcut(&mut ui, &[]).unwrap().is_none());
        assert!(ui.keys.is_empty());
        assert!(
            ui.screens
                .iter()
                .any(|s| s.contains("Select keys remains available"))
        );
    }

    #[test]
    fn editing_marks_saved_choices_without_moving_the_mark_and_new_mappings_have_none() {
        use Key::*;
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-saved-mark-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let a = Control::from_note(10, 36).unwrap();
        let mut current = vec![Mapping::new(a, "Shift", "hold").unwrap()];
        mappings::save(&path, &current).unwrap();
        let saved = std::fs::read(&path).unwrap();
        let mut ui = Script {
            keys: [
                Tab, Enter, Enter, // Edit -> saved pad
                Down, Up, Enter, // family: mark stays while the cursor moves
                Enter, Enter, Enter, Up, Enter,  // Select keys -> Modifiers -> back -> Done
                Enter,  // behavior: saved hold
                Escape, // cancel at feedback
                Escape, // leave configure
            ]
            .into(),
            ..Default::default()
        };
        configure(&mut ui, &path, &mut current).unwrap();
        assert!(ui.keys.is_empty());
        let shown = |text: &str| ui.screens.iter().any(|s| s.contains(text));
        assert!(shown("> Keyboard shortcut  [x] (saved)")); // opens on the saved value
        assert!(shown(
            "  Keyboard shortcut  [x] (saved)\n> Launch application"
        ));
        assert!(shown("Saved: Shift  [x] (saved)"));
        assert!(shown("[x] Shift (saved)")); // key picker starts on the saved chord
        assert!(shown("> hold  [x] (saved)"));
        assert!(shown("> No hardware feedback  [x] (saved)"));
        assert_eq!(std::fs::read(&path).unwrap(), saved); // cancel changes nothing

        ui.screens.clear();
        ui.pads = [Control::from_note(10, 37).unwrap()].into();
        ui.keys = [Enter, Escape, Escape].into(); // learn new pad -> cancel at family
        configure(&mut ui, &path, &mut current).unwrap();
        assert!(ui.screens.iter().any(|s| s.contains("Action family")));
        assert!(!ui.screens.iter().any(|s| s.contains("(saved)")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn configure_action_families_cancel_and_save_without_dispatch() {
        use crate::actions::Action;
        use Key::*;
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-action-menu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let a = Control::from_note(10, 36).unwrap();
        let b = Control::from_note(10, 37).unwrap();
        let mut current = vec![Mapping::new(a, "Shift", "hold").unwrap()];
        mappings::save(&path, &current).unwrap();
        let mut ui = Script {
            // learn, Launch application, the app, second press Close, fire when pressed, save and finish
            keys: [
                Enter, Down, Enter, Enter, Down, Enter, Enter, Enter, Down, Enter,
            ]
            .into(),
            pads: [b].into(),
            ..Default::default()
        };
        configure(&mut ui, &path, &mut current).unwrap();
        assert!(ui.keys.is_empty());
        assert_eq!(
            current[1].action,
            Action::AppToggle {
                path: "/nonexistent/recorded.desktop".into(),
                close: true
            }
        );
        assert!(ui.screens.iter().any(|s| s.starts_with("Second press")));
        // A sequence step launches only; it never asks for a second press.
        let mut step = Script {
            keys: [Enter].into(),
            ..Default::default()
        };
        assert_eq!(
            action_menu(&mut step, 1, mappings::Input::Pad, None, false).unwrap(),
            Some(Action::Application("/nonexistent/recorded.desktop".into()))
        );
        assert!(current[1].keys.is_empty());
        assert_eq!(current[1].behavior, mappings::Behavior::Trigger);
        // Editing opens the family menu on the saved "Launch application"; one Down reaches system.
        ui.keys = [
            Tab, Enter, Down, Enter, Down, Enter, Enter, Enter, Enter, Down, Enter,
        ]
        .into();
        configure(&mut ui, &path, &mut current).unwrap();
        assert!(
            ui.screens
                .iter()
                .any(|s| s.contains("> Launch application  [x] (saved)"))
        );
        assert_eq!(current[1].action, Action::Volume { up: true, step: 5 });
        assert_eq!(current, mappings::load(&path).unwrap());
        assert_eq!(current[0].keys, ["Shift"]);
        assert!(
            action_menu(
                &mut Script::default(),
                1,
                mappings::Input::Knob { step: 4 },
                None,
                true
            )
            .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn grouped_commands_reach_every_command_once_and_headings_are_not_commands() {
        let rows = command_rows(false);
        let mut ids = rows
            .iter()
            .filter_map(|(_, command)| command.map(|(id, _)| id))
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(
            ids,
            (0..=18)
                .filter(|id| ![4, 13].contains(id))
                .collect::<Vec<_>>()
        );
        assert!(
            rows.iter()
                .any(|(name, command)| *name == "Reset settings" && command.is_some())
        );
        let headings = rows.iter().filter(|(_, c)| c.is_none()).map(|(h, _)| *h);
        assert!(headings.eq([
            "Run",
            "Configure",
            "Programs",
            "Feedback",
            "Wispr Flow",
            "Inspect"
        ]));
        assert!(rows[0].1.is_none() && rows[1].0 == "Run");
        let running = command_rows(true);
        assert_eq!(running[1].0, "Pause");
        assert!(
            running
                .iter()
                .any(|(name, command)| *name == "/pause" && command.is_some())
        );
        assert!(!running.iter().any(|(name, _)| *name == "/resume"));
    }

    #[test]
    fn reset_requires_exact_confirmation_and_preserves_file_on_failed_save() {
        use Key::*;
        for (keys, confirmed) in [
            (vec![Escape], false),
            (vec![Character(b'n'), Character(b'o'), Enter], false),
            (
                vec![
                    Character(b'r'),
                    Character(b'e'),
                    Character(b's'),
                    Character(b'e'),
                    Character(b't'),
                    Enter,
                ],
                true,
            ),
        ] {
            let mut ui = Script {
                keys: keys.into(),
                ..Default::default()
            };
            assert_eq!(reset_confirmation(&mut ui).unwrap(), confirmed);
        }
        let dir = std::env::temp_dir().join(format!("vibeconsole-reset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let mut config = mappings::Config::default();
        let mut mapping = Mapping::new(Control::note(10, 36).unwrap(), "Shift", "hold").unwrap();
        mapping.context = 7;
        config.mappings.push(mapping);
        config
            .set_idle(
                0,
                crate::feedback::Settings {
                    octave: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        config
            .set_idle(
                7,
                crate::feedback::Settings {
                    tempo: Some(90),
                    ..Default::default()
                },
            )
            .unwrap();
        mappings::save_config(&path, &config).unwrap();
        let before = std::fs::read(&path).unwrap();
        let backup = reset_file(&path).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), before);
        assert!(
            backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("mappings.tsv.bak-")
        );
        assert_eq!(
            mappings::load_config(&path).unwrap(),
            mappings::Config::default()
        );
        mappings::save_config(&path, &config).unwrap();
        assert!(reset_file(&path).is_err()); // existing timestamped backup is never overwritten
        assert_eq!(mappings::load_config(&path).unwrap(), config);

        let failed = dir.join("failed");
        std::fs::create_dir(&failed).unwrap();
        let failed_path = failed.join("mappings.tsv");
        mappings::save_config(&failed_path, &config).unwrap();
        std::fs::write(failed_path.with_extension("tmp"), "occupied").unwrap();
        assert!(reset_file(&failed_path).is_err());
        assert_eq!(mappings::load_config(&failed_path).unwrap(), config);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reset_pauses_and_makes_no_fake_controller_write() {
        let dir = std::env::temp_dir().join(format!(
            "vibeconsole-reset-controller-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let mapping = Mapping::new(Control::note(10, 36).unwrap(), "Shift", "hold").unwrap();
        mappings::save(&path, &[mapping.clone()]).unwrap();
        let mut current = vec![mapping];
        let writes = Arc::new(std::sync::Mutex::new(0));
        let mut ui = Terminal::open_mode(&path, true).unwrap();
        ui.controller = Some(crate::feedback::Controller::fake_for_reset(Arc::clone(
            &writes,
        )));
        let deadline = Instant::now() + Duration::from_secs(1);
        while ui.controller.as_ref().unwrap().baseline.is_none() {
            ui.controller.as_mut().unwrap().poll().unwrap();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        ui.wanted_run = true;
        ui.run_mappings = current.clone();
        let backup = ui.reset_confirmed(&mut current).unwrap();
        assert!(!ui.wanted_run && ui.suspended && current.is_empty());
        assert_eq!(*writes.lock().unwrap(), 0);
        assert_eq!(
            mappings::load_config(&path).unwrap(),
            mappings::Config::default()
        );
        assert!(backup.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn menus_save_both_banks_cancel_edit_remove_and_preserve_failed_save() {
        use Key::*;
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-menu-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let a = Control::from_note(10, 36).unwrap();
        let b = Control::from_note(10, 44).unwrap();
        let mut keys = Vec::new();
        for (which, save_choice) in [(1, 0), (0, 0)] {
            keys.extend([Enter, Enter, Enter, Enter]); // learn, family, shortcut, modifiers
            keys.extend(std::iter::repeat_n(Down, which));
            keys.extend([Space, Enter]);
            keys.extend(std::iter::repeat_n(Down, 7));
            keys.extend([Enter, Enter, Enter, Enter]); // done, hold, no feedback, fire when pressed
            keys.extend(std::iter::repeat_n(Down, save_choice));
            keys.push(Enter);
        }
        keys.extend([Tab, Enter, Enter, Escape]); // cancel edit after inspecting A
        keys.extend([Tab, Down, Enter, Down, Enter, Down, Enter]); // remove B explicitly
        keys.extend([Tab, Tab, Enter]); // finish
        let mut ui = Script {
            keys: keys.into(),
            pads: [a, b].into(),
            ..Default::default()
        };
        let mut current = Vec::new();
        configure(&mut ui, &path, &mut current).unwrap();
        assert!(ui.keys.is_empty());
        assert!(
            ui.screens
                .iter()
                .any(|s| s.contains("Note ch10 #44 (Program 1 unverified)") && s.contains("Ctrl"))
        );
        assert_eq!(current, mappings::load(&path).unwrap());
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].keys, ["Shift"]);
        std::fs::write(path.with_extension("tmp"), "occupied").unwrap();
        let previous = current.clone();
        assert!(commit(&path, &mut current, a, 0, None).is_err());
        assert_eq!(current, previous);
        assert_eq!(mappings::load(&path).unwrap(), previous);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Ticket 01: each family the menu offers must save for that control, and every failure names its stage.
    #[test]
    fn knob_piano_and_joystick_configuration_save_and_failures_name_their_stage() {
        use Key::*;
        use mappings::Input;
        let dir = std::env::temp_dir().join(format!("vibeconsole-controls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        mappings::save(&path, &[]).unwrap();
        let knob = Control {
            message: crate::Message::Cc,
            channel: 1,
            id: 70,
            direction: 1,
        };
        let joystick = Control {
            message: crate::Message::Bend,
            channel: 1,
            id: 0,
            direction: 1,
        };
        let stick = Input::Joystick {
            center: 8192,
            min: 0,
            max: 16383,
            deadzone: 1024,
            hysteresis: 128,
        };
        let shortcut_ctrl = [
            Enter, Enter, Space, Enter, Down, Down, Down, Down, Down, Down, Down, Enter,
        ];
        let mut keys = vec![Down, Down, Down, Enter]; // learn joystick
        keys.extend([Enter]); // its only family: keyboard shortcut
        keys.extend(shortcut_ctrl);
        keys.extend([Enter]); // review: save and configure another
        keys.extend([Down, Down, Enter, Down, Enter, Enter, Enter, Enter, Enter]); // knob -> system -> volume up, step, knob step, save
        keys.extend([Down, Enter, Enter]); // piano -> keyboard shortcut
        keys.extend(shortcut_ctrl);
        keys.extend([Enter, Enter, Enter]); // hold, fire when pressed, save
        keys.push(Escape);
        let mut ui = Script {
            keys: keys.into(),
            pads: [Control::note(1, 60).unwrap()].into(),
            motions: [
                (joystick, stick, "Joystick Right".into()),
                (knob, Input::Knob { step: 4 }, "Knob 1 increase".into()),
            ]
            .into(),
            ..Default::default()
        };
        let mut current = Vec::new();
        configure(&mut ui, &path, &mut current).unwrap();
        assert!(ui.keys.is_empty());
        let menus = ui
            .screens
            .iter()
            .filter(|s| s.starts_with("Action family\n\n> Keyboard shortcut"))
            .collect::<Vec<_>>();
        assert_eq!(menus.len(), 3);
        assert!(!menus[0].contains("Launch") && !menus[0].contains("Prompt")); // joystick
        assert!(menus[1].contains("Fedora system action") && !menus[1].contains("Prompt")); // knob
        assert!(menus[1].contains("Choice knob") && menus[1].contains("Value knob"));
        assert!(menus[2].contains("Prompt") && !menus[2].contains("Choice knob")); // piano
        assert_eq!(current, mappings::load(&path).unwrap());
        assert_eq!(current.len(), 3);
        assert!(
            current
                .iter()
                .any(|m| m.input == stick && m.keys == ["Ctrl"])
        );
        assert!(
            current
                .iter()
                .any(|m| m.action == crate::actions::Action::Volume { up: true, step: 5 })
        );
        assert!(
            current
                .iter()
                .any(|m| m.input == Input::Piano && m.keys == ["Ctrl"])
        );
        // Shortcuts and audio steps have no observable state: no status option (ticket 10).
        assert!(!ui.screens.iter().any(|s| s.starts_with("Status feedback")));

        // Failures name their stage and leave the file byte-for-byte unchanged.
        let before = std::fs::read(&path).unwrap();
        let mut ui = Script {
            keys: [Down, Down, Enter].into(),
            ..Default::default()
        };
        let error = configure(&mut ui, &path, &mut current)
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("controller I/O: "), "{error}");
        let mut config = mappings::load_config(&path).unwrap();
        config.idle.arp = Some(true);
        config.mappings.retain(|m| m.input != Input::Piano);
        mappings::save_config(&path, &config).unwrap();
        current = config.mappings.clone();
        let before_piano = std::fs::read(&path).unwrap();
        let mut ui = Script {
            keys: [Down, Enter, Enter, Escape].into(), // learn piano -> Back -> leave
            pads: [Control::note(1, 62).unwrap()].into(),
            ..Default::default()
        };
        configure(&mut ui, &path, &mut current).unwrap(); // a note, not a failure that pauses
        assert!(
            ui.screens
                .iter()
                .any(|s| s.starts_with("Not saved: validation: piano keys")
                    && s.contains("feedback comes from the idle settings"))
        );
        assert!(!ui.screens.iter().any(|s| s.starts_with("Action family"))); // refused before any choice
        assert_eq!(std::fs::read(&path).unwrap(), before_piano);
        std::fs::write(path.with_extension("tmp"), "occupied").unwrap();
        let mut ui = Script {
            keys: [Down, Down, Enter, Down, Enter, Enter, Enter, Enter, Enter].into(),
            motions: [(
                Control { id: 71, ..knob },
                Input::Knob { step: 4 },
                "Knob 2 increase".into(),
            )]
            .into(),
            ..Default::default()
        };
        let error = configure(&mut ui, &path, &mut current)
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("file I/O: "), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), before_piano);
        assert_ne!(before, before_piano);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A thumbstick sweep wobbles the other axis a little; the dominant axis is learned, not refused.
    #[test]
    fn joystick_calibration_keeps_the_dominant_axis_and_refuses_two_real_sources() {
        use Key::*;
        let sweep = vec![
            [0xe0, 0, 64],
            [0xb0, 1, 3],
            [0xe0, 127, 127],
            [0xb0, 1, 0],
            [0xe0, 0, 0],
            [0xe0, 0, 64],
        ];
        let mut ui = Script {
            keys: [Down, Enter, Enter, Enter, Enter, Down, Enter].into(),
            ..Default::default()
        };
        let (control, input, label) = calibrate_motion(&mut ui, sweep, false).unwrap().unwrap();
        assert_eq!(control.message, crate::Message::Bend);
        assert!(matches!(
            input,
            mappings::Input::Joystick { center: 8192, .. }
        ));
        assert_eq!(label, "Joystick Right");
        let both = vec![[0xe0, 0, 0], [0xe0, 127, 127], [0xb0, 1, 0], [0xb0, 1, 127]];
        let error = calibrate_motion(&mut Script::default(), both, false).unwrap_err();
        assert!(error.to_string().contains("multiple sources"));
    }

    /// Ticket 05: learning an assigned pad shows its mapping; only Replace changes anything.
    #[test]
    fn learning_an_assigned_pad_warns_and_cancel_changes_nothing() {
        use Key::*;
        let dir = std::env::temp_dir().join(format!("vibeconsole-replace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let a = Control::from_note(10, 36).unwrap();
        let mut current = vec![Mapping::new(a, "Shift", "hold").unwrap()];
        mappings::save(&path, &current).unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut ui = Script {
            keys: [Enter, Enter, Escape].into(), // learn A -> Cancel (default) -> leave
            pads: [a].into(),
            ..Default::default()
        };
        configure(&mut ui, &path, &mut current).unwrap();
        let warning = ui
            .screens
            .iter()
            .find(|s| s.starts_with("Already assigned:"))
            .expect("warning shown");
        assert!(warning.contains("Shift") && warning.contains("> Cancel"));
        assert!(!ui.screens.iter().any(|s| s.starts_with("Action family")));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(current[0].keys, ["Shift"]);
        // Replace: Down to Replace, then shortcut Ctrl, hold, no feedback, pressed, save and finish.
        // The key picker starts with saved Shift selected: add Ctrl, drop Shift.
        let mut keys = vec![
            Enter, Down, Enter, Enter, Enter, Enter, Space, Down, Space, Enter,
        ];
        keys.extend(std::iter::repeat_n(Down, 7));
        keys.extend([Enter, Enter, Enter, Enter, Down, Enter]);
        ui.keys = keys.into();
        ui.pads = [a].into();
        configure(&mut ui, &path, &mut current).unwrap();
        assert!(ui.keys.is_empty());
        assert_eq!(mappings::load(&path).unwrap()[0].keys, ["Ctrl"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Ticket 02: a pad prompt and a sequence prompt step both save and reload unchanged.
    #[test]
    fn pad_prompt_and_sequence_prompt_step_save() {
        use crate::actions::{Action, Step, Target};
        use Key::*;
        let dir = std::env::temp_dir().join(format!("vibeconsole-prompts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        mappings::save(&path, &[]).unwrap();
        let (a, b) = (
            Control::from_note(10, 36).unwrap(),
            Control::from_note(10, 37).unwrap(),
        );
        let new_prompt = |name: &[u8]| {
            let mut keys = vec![Enter]; // "New prompt" (the only file entry)
            keys.extend(name.iter().map(|c| Character(*c)));
            keys.extend([Enter, Enter, Enter]); // name, target claude, paste only
            keys
        };
        let mut keys = vec![Enter, Down, Down, Down, Enter]; // learn A -> Prompt
        keys.extend(new_prompt(b"review"));
        keys.extend([Enter, Down, Enter, Enter]); // fire when pressed, status On, save (Script labels the pad itself)
        keys.extend([Enter, Down, Down, Down, Down, Down, Enter]); // learn B -> sequence
        keys.extend([Enter, Down, Enter]); // add step -> prompt
        keys.extend([Enter, Enter, Enter, Enter]); // review, use existing, claude, paste only
        keys.extend([Down, Down, Enter]); // done
        keys.extend([Enter, Enter, Down, Enter]); // fire when pressed, status Off, save and finish
        let mut ui = Script {
            keys: keys.into(),
            pads: [a, b].into(),
            ..Default::default()
        };
        let mut current = Vec::new();
        configure(&mut ui, &path, &mut current).unwrap();
        assert!(ui.keys.is_empty());
        let prompt = Action::Prompt {
            file: "review".into(),
            target: Target::Agent("claude".into()),
            enter: false,
        };
        assert_eq!(current, mappings::load(&path).unwrap());
        assert_eq!(current[0].action, prompt);
        assert!(current[0].status && !current[1].status);
        assert_eq!(current[1].action, Action::Sequence(vec![Step::Do(prompt)]));
        assert_eq!(
            std::fs::read_to_string(dir.join("prompts/review")).unwrap(),
            "Review this change.\n"
        );
        let before = std::fs::read(&path).unwrap();
        std::fs::write(path.with_extension("tmp"), "occupied").unwrap();
        ui.keys = [
            Enter, Down, Down, Down, Enter, Enter, Enter, Enter, Enter, Enter, Enter, Enter,
        ]
        .into();
        ui.pads = [Control::from_note(10, 38).unwrap()].into();
        let error = configure(&mut ui, &path, &mut current)
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("file I/O: "), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A fast knob turn overflows the action queue: extra steps are skipped without pausing,
    /// while a pad press that cannot be queued is still an error (the caller pauses).
    #[test]
    fn knob_steps_beyond_a_full_queue_are_skipped_but_pad_presses_still_fail() {
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-knob-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        mappings::save(&path, &[]).unwrap();
        let knob = Control {
            channel: 1,
            id: 70,
            message: crate::Message::Cc,
            direction: 1,
        };
        let pad = Control::from_note(10, 36).unwrap();
        let volume = crate::actions::Action::Volume { up: true, step: 5 };
        let mut ui = Terminal::open_mode(&path, true).unwrap();
        let (release, held) = std::sync::mpsc::channel::<()>();
        ui.actions = crate::actions::Dispatcher::with(move |_, _, _| {
            let _ = held.recv(); // every request waits until the test ends
            Ok("done".into())
        });
        ui.run_mappings = vec![
            Mapping::new_action(knob, mappings::Input::Knob { step: 4 }, volume.clone()).unwrap(),
            Mapping::new_action(pad, mappings::Input::Pad, volume).unwrap(),
        ];
        for _ in 0..20 {
            ui.transition(knob, true).unwrap();
        }
        assert!(ui.notice.contains("extra steps skipped"), "{}", ui.notice);
        assert!(ui.transition(pad, true).is_err());
        drop(release);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Ticket 04: after a confirmed capture the session returns to running if it was running;
    /// a failure stays paused and shows the failed stage. The injected runner never touches Flow.
    #[test]
    fn flow_success_restores_running_and_failure_stays_paused_with_its_stage() {
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-flow-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        mappings::save(&path, &[]).unwrap();
        for (ok, was_running) in [(true, true), (true, false), (false, true)] {
            let mut ui = Terminal::open_mode(&path, true).unwrap();
            ui.actions = crate::actions::Dispatcher::with(move |_, _, _| {
                if ok {
                    Ok("Flow helper opened the current keyboard".into())
                } else {
                    Err(
                        "Flow current keyboard capture: timed out. Manual recovery: reopen Flow"
                            .into(),
                    )
                }
            });
            ui.wanted_run = was_running;
            ui.context = 2; // headless = service: Program 3 must resume too (run_mode refuses it)
            ui.begin_flow(None).unwrap();
            assert!(!ui.wanted_run && ui.flow_pending); // keys released, output paused meanwhile
            let deadline = Instant::now() + Duration::from_secs(2);
            while ui.flow_pending {
                ui.tick().unwrap(); // inside a menu: never resumes here
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(!ui.wanted_run);
            ui.at_menu = true; // back on the command list
            ui.tick().unwrap();
            assert_eq!(
                ui.wanted_run,
                ok && was_running,
                "ok {ok}, running {was_running}"
            );
            if ok {
                assert!(
                    ui.notice.contains("opened the current keyboard"),
                    "{}",
                    ui.notice
                );
            }
            if !ok {
                assert!(
                    ui.error.contains("keyboard capture") && ui.error.contains("Manual recovery")
                );
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Tickets 09–10: a status pad shows sent until the worker answers, then the derived state:
    /// running while its process lives, stopped once it exits or the server is killed, failed
    /// when the action failed. Nothing is shown for pads with the option off.
    #[test]
    fn status_feedback_follows_sent_running_stopped_and_failed() {
        assert_eq!(status_word(Some(Phase::Sent), Some(false)), "sent");
        assert_eq!(status_word(Some(Phase::Done), Some(true)), "running");
        assert_eq!(status_word(Some(Phase::Done), Some(false)), "stopped"); // exited or server killed
        assert_eq!(status_word(None, Some(true)), "running"); // started before this session
        assert_eq!(status_word(Some(Phase::Failed), Some(true)), "failed");
        assert_eq!(status_word(Some(Phase::Done), None), "unavailable");
        assert!(crate::actions::pane_alive(true, "1\n0\n"));
        assert!(!crate::actions::pane_alive(true, "1\n")); // remain-on-exit: dead pane
        assert!(!crate::actions::pane_alive(false, "")); // no server or window

        let dir =
            std::env::temp_dir().join(format!("vibeconsole-status-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let (on, off) = (
            Control::from_note(10, 36).unwrap(),
            Control::from_note(10, 37).unwrap(),
        );
        let command = |line: &str| crate::actions::Action::Command {
            window: "w".into(),
            dir: "/tmp".into(),
            line: line.into(),
        };
        let mut shown = Mapping::new_action(on, mappings::Input::Pad, command("ok")).unwrap();
        shown.status = true;
        let hidden = Mapping::new_action(off, mappings::Input::Pad, command("quiet")).unwrap();
        mappings::save(&path, &[shown.clone(), hidden.clone()]).unwrap();
        for ok in [true, false] {
            let mut ui = Terminal::open_mode(&path, true).unwrap();
            ui.actions = crate::actions::Dispatcher::with(move |_, _, _| {
                if ok {
                    Ok("started".into())
                } else {
                    Err("tmux: no server".into())
                }
            });
            ui.run_mappings = vec![shown.clone(), hidden.clone()];
            ui.transition(on, true).unwrap();
            ui.transition(off, true).unwrap();
            assert_eq!(ui.phases.get(&on), Some(&Phase::Sent));
            assert!(!ui.phases.contains_key(&off)); // option off: nothing tracked or shown
            let deadline = Instant::now() + Duration::from_secs(2);
            while ui.phases.get(&on) == Some(&Phase::Sent) {
                ui.tick().unwrap();
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(ui.phases[&on], if ok { Phase::Done } else { Phase::Failed });
            let lines = ui.status_lines();
            assert_eq!(lines.len(), 1);
            assert!(
                lines[0].contains(if ok { "" } else { "failed" }),
                "{lines:?}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn typed(text: &str) -> Vec<Key> {
        text.bytes().map(Key::Character).collect()
    }

    /// Ticket 11–12: Settings saves a default agent directory, the Flow startup toggle and both
    /// variable lists; invalid input shows "Not saved" and leaves the file alone.
    #[test]
    fn settings_save_directory_flow_and_variables_and_refuse_bad_input() {
        use Key::*;
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-settings-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("work")).unwrap();
        let path = dir.join("mappings.tsv");
        mappings::save(&path, &[]).unwrap();
        let work = dir.join("work").to_str().unwrap().to_owned();
        let mut keys = vec![Enter];
        keys.extend(typed("relative"));
        keys.extend([Enter, Enter]); // refused: Not saved -> Back
        keys.push(Enter);
        keys.extend(typed(&format!("{work}/missing")));
        keys.extend([Enter, Enter]); // refused: missing directory
        let refused = keys.len();
        keys.push(Enter);
        keys.extend(typed(&work));
        keys.push(Enter); // saved
        keys.extend([Down, Enter]); // Flow at launch: Off
        keys.extend([Down, Down, Enter, Enter]); // startup variables -> Add
        keys.extend(typed("9X"));
        keys.extend([Enter, Enter, Enter]); // name, value (empty), refused -> Back
        keys.push(Enter); // Add
        keys.extend(typed("API_MODEL"));
        keys.push(Enter);
        keys.extend(typed("opus"));
        keys.extend([Enter, Escape]);
        keys.extend([Down, Down, Down, Enter, Enter]); // user variables -> Add
        keys.extend(typed("WORK"));
        keys.push(Enter);
        keys.extend(typed(&work));
        keys.extend([Enter, Escape, Escape]);
        let mut ui = Script {
            keys: keys.into(),
            ..Default::default()
        };
        let before = std::fs::read(&path).unwrap();
        settings_menu(&mut ui, &path).unwrap();
        assert!(ui.keys.is_empty());
        let notes = ui
            .screens
            .iter()
            .filter(|s| s.starts_with("Not saved:"))
            .count();
        assert!(notes >= 3, "{notes}");
        assert!(refused > 0 && before.len() > 0);
        let config = mappings::load_config(&path).unwrap();
        assert_eq!(config.agent_dir.as_deref(), Some(work.as_str()));
        assert!(!config.start_flow);
        assert_eq!(config.env, [("API_MODEL".to_string(), "opus".to_string())]);
        assert_eq!(config.vars, [("WORK".to_string(), work.clone())]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Ticket 11: an agent's own directory overrides the default; empty keeps the default.
    #[test]
    fn agents_take_a_directory_override_and_refuse_a_missing_one() {
        use Key::*;
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-agent-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo")).unwrap();
        let path = dir.join("mappings.tsv");
        mappings::save(&path, &[]).unwrap();
        let repo = dir.join("repo").to_str().unwrap().to_owned();
        let mut keys = vec![Enter, Enter, Enter, Enter, Enter]; // claude -> Edit -> name, launch, window
        keys.extend(typed("/definitely/not/here"));
        keys.extend([Enter, Enter, Enter, Enter, Enter]); // light defaults -> refused -> Back
        keys.extend([Enter, Enter, Enter, Enter, Enter]);
        keys.extend(typed(&repo));
        keys.extend([Enter, Up, Up, Up, Enter, Enter, Enter, Escape]); // octave +4, tempos kept
        let mut ui = Script {
            keys: keys.into(),
            ..Default::default()
        };
        agents_menu(&mut ui, &path).unwrap();
        assert!(ui.keys.is_empty());
        assert!(
            ui.screens
                .iter()
                .any(|s| s.starts_with("Not saved:") && s.contains("missing"))
        );
        let config = mappings::load_config(&path).unwrap();
        assert_eq!(config.agents[0].dir.as_deref(), Some(repo.as_str()));
        assert_eq!(config.agents[1].dir, None);
        // Ticket 14: the agent's status-light settings are edited and saved with it.
        assert_eq!(
            (
                config.agents[0].octave,
                config.agents[0].thinking_bpm,
                config.agents[0].input_bpm
            ),
            (4, 50, 240)
        );
        assert_eq!(config.agents[1].octave, -1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Ticket 14: status lights show only in a running interactive session on a verified,
    /// non-piano program with Status lights On; the toggle saves, and v12 files load with the
    /// spec defaults.
    #[test]
    fn status_lights_need_a_running_interactive_non_piano_session_and_can_be_turned_off() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-lights-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        mappings::save(&path, &[]).unwrap();
        let mut ui = Terminal::open_mode(&path, true).unwrap();
        ui.light_agents = Some(mappings::Config::default().agents);
        ui.agent_states = vec![
            ("claude".into(), "claude".into(), "needs-input"),
            ("codex".into(), "codex".into(), "idle"),
        ];
        (ui.wanted_run, ui.suspended) = (true, false);
        (ui.context_verified, ui.feedback_allowed) = (true, true);
        let shown = ui.wanted_lights(true).unwrap();
        assert_eq!((shown.octave, shown.tempo), (Some(1), Some(240)));
        assert_eq!(ui.wanted_lights(false), None); // the login service never lights them
        ui.suspended = true; // paused
        assert_eq!(ui.wanted_lights(true), None);
        ui.suspended = false;
        ui.feedback_allowed = false; // unverified program
        assert_eq!(ui.wanted_lights(true), None);
        ui.feedback_allowed = true;
        let mut piano = Mapping::new(Control::note(1, 60).unwrap(), "Shift", "hold").unwrap();
        piano.input = mappings::Input::Piano;
        ui.run_mappings = vec![piano];
        assert_eq!(ui.wanted_lights(true), None); // octave/arpeggiator would change the notes
        ui.run_mappings.clear();
        ui.agent_states[0].2 = "idle";
        assert_eq!(ui.wanted_lights(true), None); // all idle: program feedback
        ui.agent_states[0].2 = "thinking";
        assert!(ui.wanted_lights(true).is_some());
        // Agents -> Status lights (just above Prompts, the last row) toggles and saves Off.
        let mut menu = Script {
            keys: [Key::Up, Key::Up, Key::Enter, Key::Escape].into(),
            ..Default::default()
        };
        agents_menu(&mut menu, &path).unwrap();
        assert!(menu.screens.iter().any(|s| s.contains("Status lights: On")));
        assert!(!mappings::load_config(&path).unwrap().status_lights);
        ui.light_agents = None; // what refresh_agent_states sets for Off
        assert_eq!(ui.wanted_lights(true), None);
        // A v12 file loads with Status lights On and the spec's per-agent defaults.
        let mut old = String::from("vibeconsole-mappings-v12\nidle\t-\t-\t-\n");
        for context in 1..8 {
            old.push_str(&format!("program-idle\t{context}\t-\t-\t-\n"));
        }
        old.push_str("start-flow\ton\nagent-dir\t-\nenv\t0\nvars\t0\nagents\t1\n");
        old.push_str("agent\tcodex\tcodex\tcodex\t-\n");
        std::fs::write(&path, old).unwrap();
        let config = mappings::load_config(&path).unwrap();
        assert!(config.status_lights);
        let codex = &config.agents[0];
        assert_eq!(
            (codex.octave, codex.thinking_bpm, codex.input_bpm),
            (-1, 50, 240)
        );
        mappings::save_config(&path, &config).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains(
            "status-lights\ton\nagents\t1\nagent\tcodex\tcodex\tcodex\t-\t-1\t50\t240\n"
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Ticket 13: the Prompts list reads, creates, edits and deletes; deleting a used prompt
    /// names its users; names that would leave the directory and failed edits change nothing.
    #[test]
    fn prompts_pane_lists_reads_creates_edits_and_deletes_safely() {
        use Key::*;
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-prompts-pane-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("prompts")).unwrap();
        let path = dir.join("mappings.tsv");
        let used = Mapping::new_action(
            Control::from_note(10, 36).unwrap(),
            mappings::Input::Pad,
            crate::actions::Action::Sequence(vec![crate::actions::Step::Do(
                crate::actions::Action::Prompt {
                    file: "review".into(),
                    target: crate::actions::Target::Agent("claude".into()),
                    enter: false,
                },
            )]),
        )
        .unwrap();
        mappings::save(&path, &[used]).unwrap();
        let prompts = dir.join("prompts");
        std::fs::write(prompts.join("review"), "Old review text\n").unwrap();
        // Prompts is the last Agents row: Up from the top reaches it.
        let mut keys = vec![Up, Enter];
        keys.extend([Enter, Enter, Enter]); // review -> Read -> Back
        keys.extend([Down, Enter]); // New prompt
        keys.extend(typed("../escape"));
        keys.extend([Enter, Enter]); // refused -> Back
        keys.extend([Down, Enter]); // New prompt
        keys.extend(typed("plan"));
        keys.push(Enter); // created through the editor; list is now plan, review
        keys.extend([Down, Enter, Down, Down, Enter, Enter]); // review -> Delete -> Cancel
        keys.extend([Escape, Escape]);
        let mut ui = Script {
            keys: keys.into(),
            ..Default::default()
        };
        agents_menu(&mut ui, &path).unwrap();
        assert!(ui.keys.is_empty());
        let shown = |ui: &Script, text: &str| ui.screens.iter().any(|s| s.contains(text));
        assert!(shown(&ui, "Old review text")); // read
        assert!(shown(&ui, "Not saved:")); // ../escape
        assert!(!dir.join("escape").exists());
        assert_eq!(
            std::fs::read_to_string(prompts.join("plan")).unwrap(),
            "Review this change.\n"
        );
        assert!(shown(&ui, "Used by: Program 1")); // delete warning names the sequence
        assert!(prompts.join("review").exists()); // cancelled
        // A failed edit keeps the old prompt and leaves no partial file behind.
        ui.edit_fails = true;
        ui.keys = vec![Up, Enter, Down, Enter, Down, Enter, Enter, Escape, Escape].into();
        agents_menu(&mut ui, &path).unwrap();
        assert!(ui.keys.is_empty());
        assert_eq!(
            std::fs::read_to_string(prompts.join("review")).unwrap(),
            "Old review text\n"
        );
        assert_eq!(std::fs::read_dir(&prompts).unwrap().count(), 2);
        // Delete for real.
        ui.keys = vec![
            Up, Enter, Down, Enter, Down, Down, Enter, Down, Enter, Escape, Escape,
        ]
        .into();
        agents_menu(&mut ui, &path).unwrap();
        assert!(ui.keys.is_empty());
        assert!(!prompts.join("review").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
