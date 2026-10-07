use crate::{
    Control, Detector, Event, Result,
    mappings::{self, Mapping},
    midi,
};
use std::collections::BTreeMap;
use std::io::{self, Write};
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

trait Ui {
    fn draw(&mut self, text: &str) -> Result<()>;
    fn key(&mut self) -> Result<Option<Key>>;
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
    flow_failed: bool,
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
            .unwrap_or_else(|| format!("PAUSED | {} saved assignments", self.run_mappings.len()));
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
                "KeyAI | MIDI {} | Input {}",
                crate::screen::bulb(self.input.is_some()),
                crate::screen::bulb(input_on)
            )
        } else {
            format!(
                "KeyAI | MIDI {} {midi_status} | Input {}",
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
        for (index, (name, command)) in command_rows().iter().enumerate() {
            screen.commands.push(match command {
                None => format!("── {name}"),
                Some(_) if index == self.menu && self.at_menu => format!("> {name}"),
                Some(_) if index == self.menu => format!("• {name}"),
                Some(_) => format!("  {name}"),
            });
        }
        let (width, height) = screen.live_size(size.0, size.1);
        live.extend(self.toggle_preview(width, height.saturating_sub(live.len())));
        screen.live = live;
        screen
    }

    fn reset_input_lights(&mut self) {
        self.detector = Detector::default();
        self.input_flash = None;
        self.note_flashes.clear();
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

    fn toggle_items(&self) -> Vec<(String, bool)> {
        self.run_mappings
            .iter()
            .filter(|mapping| mapping.behavior == mappings::Behavior::Toggle)
            .map(|mapping| {
                (
                    format!("{}\n{}", mapping.action_label(), self.name(mapping.control)),
                    self.output
                        .as_ref()
                        .is_some_and(|output| output.keyboard.active(mapping.control)),
                )
            })
            .collect()
    }

    fn toggle_preview(&self, width: usize, height: usize) -> Vec<String> {
        let items = self.toggle_items();
        if items.is_empty() {
            return Vec::new();
        }
        let count = items
            .len()
            .min(crate::screen::light_columns(width) * height.saturating_sub(1) / 3);
        // ponytail: compact pane previews cards; /list shows every mapping/state in large setups.
        let mut rows = vec![format!("Toggles {count}/{} · /list", items.len())];
        rows.extend(crate::screen::lights(&items[..count], width));
        rows
    }

    fn detection_view(&self, page: usize) -> (String, usize) {
        let now = Instant::now();
        let width = self.width().saturating_sub(5);
        // Pads come from the verified program only; other notes appear once pressed.
        let mut notes = Vec::new();
        let pads = self
            .program()
            .map(|(_, payload)| mappings::pad_controls(&payload));
        for control in pads.into_iter().flatten().flatten() {
            if !notes.contains(&control) {
                notes.push(control);
            }
        }
        for control in self.note_flashes.keys() {
            if !notes.contains(control) {
                notes.push(*control);
            }
        }
        let per_page =
            (self.height().saturating_sub(11) / 2).max(1) * crate::screen::light_columns(width);
        let pages = notes.len().div_ceil(per_page).max(1);
        let first = page.min(pages - 1) * per_page;
        let items = notes
            .iter()
            .skip(first)
            .take(per_page)
            .map(|control| {
                (
                    self.name(*control),
                    self.input.is_some() && self.note_light(*control, now),
                )
            })
            .collect::<Vec<_>>();
        let mut rows = vec![format!(
            "Detect · Note inputs · page {}/{}",
            first / per_page + 1,
            pages
        )];
        rows.extend(crate::screen::lights(&items, width));
        rows.push(
            crate::screen::fit(&self.last_event, width)
                .trim_end()
                .into(),
        );
        rows.push("↑↓: page | Enter/Esc: back".into());
        (rows.join("\n"), pages)
    }

    fn detect(&mut self) -> Result<()> {
        self.suspend()?;
        if self.input.is_none() {
            self.input = Some(midi::Input::open()?);
            self.reset_input_lights();
        }
        let mut page = 0;
        loop {
            self.tick()?;
            let (view, pages) = self.detection_view(page);
            self.draw(&view)?;
            if self.input.is_none() {
                return Err(self.error.clone().into());
            }
            match self.key()? {
                Some(Key::Up | Key::Left) => page = page.saturating_sub(1),
                Some(Key::Down | Key::Right) => page = (page + 1).min(pages - 1),
                Some(Key::Enter | Key::Escape) => return Ok(()),
                _ => {}
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
            flow_failed: false,
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

    fn cancel_actions(&mut self) {
        self.actions.cancel();
        if self.flow_pending {
            self.error = "Flow work cancelled; output remains paused. If Flow was already quit, reopen it manually after KeyAI's keyboard is ready, test physical Shift, then /resume.".into();
        }
        self.flow_pending = false;
    }
    fn pause(&mut self) -> Result<()> {
        self.wanted_run = false;
        self.suspend()
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

    fn start_flow(&mut self, repair: bool) -> Result<()> {
        self.pause()?;
        self.retry = None;
        self.connecting = false;
        if self.output.is_none() {
            self.output = Some(crate::keyboard::Output::new(&self.run_mappings)?);
        }
        let action = if repair {
            crate::actions::Action::FlowRepair
        } else {
            crate::actions::Action::FlowStart
        };
        self.actions
            .submit(action, self.output.as_ref().map(|o| o.node.clone()))?;
        self.flow_pending = true;
        self.flow_failed = false;
        self.error =
            "Flow operation pending; output stays paused. Esc/pause cancels pending work.".into();
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
                    if action.flow() {
                        return self.start_flow(action == crate::actions::Action::FlowRepair);
                    }
                    self.actions.submit(action, None)?;
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
                    self.notice = "Current RAM has no unique stored-program match. Select /prog-select; output stays inactive.".into();
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
                if self
                    .run_mappings
                    .iter()
                    .any(|m| m.input == mappings::Input::Piano)
                {
                    self.wanted_run = false;
                }
                self.suspended = !self.wanted_run;
                if self.output.is_none() && self.wanted_run && !self.run_mappings.is_empty() {
                    self.output = Some(crate::keyboard::Output::new(&self.run_mappings)?);
                }
                if let Some(output) = &mut self.output {
                    output.keyboard.replace(&self.run_mappings)?;
                    output.keyboard.block_reconnected();
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
                        " Piano mappings: confirm arpeggiator Off and intended octave through /run."
                    } else {
                        " Fresh release arms each Note control."
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
        while let Some((flow, notice)) = self.actions.notice() {
            if flow {
                self.flow_pending = false;
                self.flow_failed = notice.starts_with("Action failed:");
            }
            self.error = notice
                .chars()
                .filter(|c| !c.is_control())
                .take(2000)
                .collect();
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
            "Fresh MIDI connection starts inactive; first release arms each Note control. An initial tap may only arm it.".into()
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
        if self.input.is_none() {
            self.input = Some(midi::Input::open()?);
        }
        self.tick()?;
        self.capture = Some(Vec::new());
        let result = (|| -> Result<Option<(Control, mappings::Input, String)>> {
            loop {
                self.draw(&format!(
                    "{}\n{}\nEnter finishes observation; Esc cancels. Shortcuts remain paused.",
                    if knob {
                        "Turn ONE knob slowly both ways to its stops."
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
            calibrate_motion(self, samples, knob)
        })();
        self.capture = None;
        result
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
            Some(27) => {
                if self.byte(30)? == Some(b'[') {
                    Some(match self.byte(30)? {
                        Some(b'A') => Key::Up,
                        Some(b'B') => Key::Down,
                        Some(b'C') => Key::Right,
                        Some(b'D') => Key::Left,
                        _ => Key::Other,
                    })
                } else {
                    Some(Key::Escape)
                }
            }
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

fn select_keys(ui: &mut impl Ui) -> Result<Option<Vec<String>>> {
    let groups = mappings::key_groups();
    let mut selected: Vec<String> = Vec::new();
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
                        "{} [{}] {k}",
                        if i == cursor { ">" } else { " " },
                        if selected.contains(k) { "x" } else { " " }
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

fn shortcut(ui: &mut impl Ui) -> Result<Option<Vec<String>>> {
    loop {
        match choose(
            ui,
            "Shortcut input",
            &options(&["Select keys", "Record shortcut", "Cancel"]),
        )? {
            Some(0) => return select_keys(ui),
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
                    &format!("Captured: {}", keys.join("+")),
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
    idle: bool,
) -> Result<Option<crate::feedback::Feedback>> {
    use crate::feedback::{Feedback, Octave};
    let mut feedback = initial;
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
        let Some(choice) = choose_at(
            ui,
            &title,
            &options(&[
                "No hardware feedback",
                "Octave",
                "Tempo",
                "Arpeggiator",
                "Done",
                "Cancel",
            ]),
            cursor,
        )?
        else {
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
                let selected = match feedback.octave {
                    None => 0,
                    Some(Octave::Absolute(_)) => 1,
                    Some(Octave::Offset(_)) => 2,
                };
                match choose_at(ui, "Octave mode", &modes, selected)? {
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
                &options(&["Unchanged", "Set BPM (enables arpeggiator)"]),
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
                if let Some(choice) = choose_at(
                    ui,
                    "Arpeggiator",
                    &options(&["Unchanged", "Off", "On"]),
                    feedback.arp.map(|on| usize::from(on) + 1).unwrap_or(0),
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

fn calibrate_motion(
    ui: &mut impl Ui,
    samples: Vec<[u8; 3]>,
    knob: bool,
) -> Result<Option<(Control, mappings::Input, String)>> {
    let mut source = None;
    let mut values = Vec::new();
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
        if source.is_some_and(|old| old != identity) {
            return Err(
                "multiple sources observed; retry moving only one knob or joystick axis".into(),
            );
        }
        source = Some(identity);
        values.push(value);
    }
    let (message, channel, id) = source.ok_or("no supported motion messages captured")?;
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

fn action_menu(
    ui: &mut impl Ui,
    family: usize,
    input: mappings::Input,
) -> Result<Option<crate::actions::Action>> {
    use crate::actions::Action;
    if matches!(input, mappings::Input::Joystick { .. })
        || (family == 1 && matches!(input, mappings::Input::Knob { .. }))
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
        names.extend(options(&[
            "Start Wispr Flow (ordered)",
            "Repair/reopen Wispr Flow (explicit graceful quit)",
        ]));
        let Some(i) = choose(
            ui,
            "Installed visible applications (one native launch request per press)",
            &names,
        )?
        else {
            return Ok(None);
        };
        return Ok(Some(if i < apps.len() {
            Action::Application(apps[i].1.clone())
        } else if i == apps.len() {
            Action::FlowStart
        } else {
            Action::FlowRepair
        }));
    }
    let Some(i) = choose(
        ui,
        "Native audio actions; default device resolved at activation.
Requires wpctl in your desktop session. Brightness/media/lock and absolute volume are not enabled.",
        &options(&[
            "Output volume up",
            "Output volume down",
            "Output mute",
            "Microphone mute",
        ]),
    )?
    else {
        return Ok(None);
    };
    if i < 2 {
        let Some(step) = number(
            ui,
            "Volume step (%) per activation; ordinary volume capped at 100%",
            5,
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
    format!(
        "{} | {} | {} | {} | {} | Feedback: {}",
        mappings::program_label(mapping.context),
        mapping.name(program),
        mapping.input.description(),
        mapping.action_label(),
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
            mappings::Input::Knob { .. } | mappings::Input::Joystick { .. }
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

fn configure(ui: &mut impl Ui, path: &Path, current: &mut Vec<Mapping>) -> Result<()> {
    loop {
        let Some(action) = configure_choice(ui)? else {
            return Ok(());
        };
        if action == 6 {
            return Ok(());
        }
        let motion = match action {
            2 => ui.learn_knob()?,
            3 => ui.learn_joystick()?,
            _ => None,
        };
        if matches!(action, 2 | 3) && motion.is_none() {
            continue;
        }
        let pad = if let Some((control, _, _)) = &motion {
            *control
        } else if action == 0 || action == 1 {
            let Some(pad) = (if action == 1 {
                ui.learn_piano()?
            } else {
                ui.learn()?
            }) else {
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
                commit(path, current, pad, ui.context(), None)?;
            }
            continue;
        }
        if choose(
            ui,
            &format!("Current: {existing}"),
            &options(&["Assign / replace action", "Cancel"]),
        )? != Some(0)
        {
            continue;
        }
        let selected_behavior = current
            .iter()
            .find(|m| m.context == ui.context() && m.control == pad)
            .is_some_and(|m| m.behavior == mappings::Behavior::Toggle);
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
        let Some(family) = choose(
            ui,
            "Action family",
            &options(&[
                "Keyboard shortcut",
                "Launch application",
                "Fedora system action",
            ]),
        )?
        else {
            continue;
        };
        let mut mapping = if family == 0 {
            let Some(keys) = shortcut(ui)? else {
                continue;
            };
            let behavior = match input {
                mappings::Input::Knob { .. } => "pulse",
                mappings::Input::Joystick { .. } => "hold",
                _ => {
                    let Some(b) = choose_at(
                        ui,
                        "Activation behavior",
                        &options(&["hold", "toggle"]),
                        usize::from(selected_behavior),
                    )?
                    else {
                        continue;
                    };
                    if b == 0 { "hold" } else { "toggle" }
                }
            };
            Mapping::new(pad, &keys.join("+"), behavior)?
        } else {
            let Some(action) = action_menu(ui, family, input)? else {
                continue;
            };
            Mapping::new_action(pad, input, action)?
        };
        mapping.context = ui.context();
        mapping.input = input;
        if let mappings::Input::Knob { step } = input {
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
            feedback_menu(ui, previous, false)?
        }) else {
            continue;
        };
        mapping.feedback = feedback;
        mapping.validate()?;
        match choose(
            ui,
            &format!("Review: {}", mapping_label(&mapping, ui.program().as_ref())),
            &options(&["Save and configure another", "Save and finish", "Cancel"]),
        )? {
            Some(choice @ (0 | 1)) => {
                commit(path, current, pad, ui.context(), Some(mapping))?;
                if choice == 1 {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

pub fn daemon(path: &Path, current: Vec<Mapping>, feedback: bool, flow: bool) -> Result<()> {
    let mut ui = Terminal::open_mode(path, true)?;
    ui.idle = mappings::load_config(path)?.idle;
    ui.feedback_allowed = feedback;
    let mappings = current
        .into_iter()
        .filter(|m| m.context == 0)
        .collect::<Vec<_>>();
    if flow {
        ui.start_flow(false)?;
        while ui.flow_pending && !ui.stop.load(Ordering::Relaxed) {
            if let Err(error) = ui.tick() {
                if ui.stop.load(Ordering::Relaxed) {
                    break;
                }
                return Err(error);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        eprintln!("{}", ui.error);
        if ui.flow_failed {
            return Err(ui.error.clone().into());
        }
        if ui.stop.load(Ordering::Relaxed) {
            return ui.pause();
        }
        eprintln!(
            "Ordered Flow helper capture verified; dictation remains unverified. Continuing with inactive mappings and release arming."
        );
    }
    ui.run(&mappings)?;
    let mut previous = String::new();
    while !ui.stop.load(Ordering::Relaxed) {
        if let Err(error) = ui.tick() {
            if ui.stop.load(Ordering::Relaxed) {
                break;
            }
            return Err(error);
        }
        let status = format!(
            "{} | {}",
            ui.output
                .as_ref()
                .map(|o| o.keyboard.status(|control| ui.name(control)))
                .unwrap_or_default(),
            ui.error
        );
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
                "/run",
                "Enable the verified program's mappings; retain applicable safety confirmations.",
            ),
            (
                3,
                "/pause",
                "Release synthetic keys, cancel pending mapped actions, and remain paused.",
            ),
            (
                4,
                "/resume",
                "Resume paused output with fresh-activation guards.",
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
                12,
                "/start-flow",
                "Prepare the virtual keyboard, start Flow if absent, and check helper capture.",
            ),
            (
                13,
                "/repair-flow",
                "Pause output and explicitly request targeted graceful Flow reopening.",
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
fn command_rows() -> Vec<(&'static str, Option<(usize, &'static str)>)> {
    let mut rows = Vec::new();
    for (heading, commands) in COMMANDS {
        rows.push((*heading, None));
        rows.extend(
            commands
                .iter()
                .map(|(id, name, about)| (*name, Some((*id, *about)))),
        );
    }
    rows
}

/// Highlighted command row; Up/Down skip headings and wrap. Esc quits as before.
fn command_menu(ui: &mut Terminal) -> Result<Option<usize>> {
    let rows = command_rows();
    loop {
        let Some((command, about)) = rows[ui.menu].1 else {
            ui.menu = (ui.menu + 1) % rows.len();
            continue;
        };
        ui.draw(&format!("{}\n\n{about}\n\nEnter: open", rows[ui.menu].0))?;
        let step = match ui.key()? {
            Some(Key::Up) => rows.len() - 1,
            Some(Key::Down) => 1,
            Some(Key::Enter) => return Ok(Some(command)),
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
    ui.idle = mappings::load_config(path)?.idle;
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
    if (initial == "run" || initial == "session") && !ui.context_verified {
        ui.wanted_run = true;
        ui.notice = "Detecting the controller’s current program before enabling actions.".into();
    } else if initial == "run" || initial == "session" {
        ui.run_mode(
            &current
                .iter()
                .filter(|m| m.context == ui.context)
                .cloned()
                .collect::<Vec<_>>(),
            initial == "run",
        )?;
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
            1 | 4 if !ui.context_verified => choose(
                &mut ui,
                "Select /prog-select before running.\nChoose the matching hardware program, then run or resume. No mapped actions are enabled yet.",
                &options(&["Back"]),
            ).map(|_| ()),
            1 | 4 => ui.run(
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
                let Some(feedback) = feedback_menu(&mut ui, initial, true)? else {
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
                        output.keyboard.block_reconnected();
                    }
                    ui.reconnected = true;
                }
                Ok(())
            })(),
            12 => ui.start_flow(false),
            13 => ui.start_flow(true),
            14 => ui.rename_program(),
            _ => return Ok(()),
        };
        let result = result.and_then(|()| {
            if ui.context_verified && was_running && matches!(command, 0 | 6 | 7 | 8 | 9 | 10 | 14)
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
            choose(
                &mut ui,
                "Operation failed; mappings preserved. Resume explicitly after resolving the error.",
                &options(&["Back"]),
            )?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    #[derive(Default)]
    struct Script {
        keys: VecDeque<Key>,
        pads: VecDeque<Control>,
        recordings: VecDeque<Result<Vec<String>>>,
        screens: Vec<String>,
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
    }
    #[test]
    fn detected_hardware_context_overrides_old_context_preserves_pause_and_refuses_ambiguity() {
        let directory =
            std::env::temp_dir().join(format!("keyai-follow-ui-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("keyai-pad-labels-{}", std::process::id()));
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
    fn simulated_input_bulbs_cover_hold_fast_release_motion_and_disconnect_reset() {
        let mut ui = Terminal::open_mode(Path::new("/unused-keyai-indicator-test"), true).unwrap();
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
        let dir = std::env::temp_dir().join(format!("keyai-running-mode-{}", std::process::id()));
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
            recordings: VecDeque::new(),
            screens: Vec::new(),
        };
        assert_eq!(
            feedback_menu(&mut ui, initial, false).unwrap(),
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
            recordings: VecDeque::new(),
            screens: Vec::new(),
        };
        let feedback = feedback_menu(&mut ui, crate::feedback::Feedback::default(), false)
            .unwrap()
            .unwrap();
        assert_eq!(feedback.tempo, Some(121));
        assert_eq!(feedback.arp, Some(true));
        assert!(ui.keys.is_empty());
        ui.keys = [Down, Enter, Up, Enter, Down, Enter].into();
        let idle = feedback_menu(&mut ui, feedback, true).unwrap().unwrap();
        assert_eq!(idle.tempo, Some(121));
        assert_eq!(idle.arp, Some(false));
        ui.keys = [
            Down, Enter, Down, Down, Enter, Up, Enter, Down, Down, Down, Enter,
        ]
        .into();
        assert_eq!(
            feedback_menu(&mut ui, crate::feedback::Feedback::default(), false)
                .unwrap()
                .unwrap()
                .octave,
            Some(crate::feedback::Octave::Offset(1))
        );
        ui.keys = [Escape].into();
        assert!(feedback_menu(&mut ui, feedback, false).unwrap().is_none());
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
            screens: Vec::new(),
        };
        assert_eq!(shortcut(&mut ui).unwrap().unwrap(), ["Shift"]);
        assert!(ui.recordings.is_empty());
        assert!(ui.keys.is_empty());
        ui.keys = [Down, Enter, Escape].into();
        ui.recordings = [Ok(vec!["Escape".into()])].into();
        assert!(shortcut(&mut ui).unwrap().is_none());
        assert!(ui.keys.is_empty());
        ui.keys = [Down, Enter, Enter, Enter, Escape].into();
        ui.recordings = [Err("permission denied".into())].into();
        assert!(shortcut(&mut ui).unwrap().is_none());
        assert!(ui.keys.is_empty());
        assert!(
            ui.screens
                .iter()
                .any(|s| s.contains("Select keys remains available"))
        );
    }

    #[test]
    fn configure_action_families_cancel_and_save_without_dispatch() {
        use crate::actions::Action;
        use Key::*;
        let dir = std::env::temp_dir().join(format!("keyai-action-menu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let a = Control::from_note(10, 36).unwrap();
        let b = Control::from_note(10, 37).unwrap();
        let mut current = vec![Mapping::new(a, "Shift", "hold").unwrap()];
        mappings::save(&path, &current).unwrap();
        let mut ui = Script {
            keys: [Enter, Enter, Down, Enter, Enter, Down, Enter].into(),
            pads: [b].into(),
            ..Default::default()
        };
        configure(&mut ui, &path, &mut current).unwrap();
        assert!(ui.keys.is_empty());
        assert_eq!(
            current[1].action,
            Action::Application("/nonexistent/recorded.desktop".into())
        );
        assert!(current[1].keys.is_empty());
        assert_eq!(current[1].behavior, mappings::Behavior::Trigger);
        ui.keys = [
            Tab, Enter, Down, Enter, Enter, Down, Down, Enter, Enter, Enter, Down, Enter,
        ]
        .into();
        configure(&mut ui, &path, &mut current).unwrap();
        assert_eq!(current[1].action, Action::Volume { up: true, step: 5 });
        assert_eq!(current, mappings::load(&path).unwrap());
        assert_eq!(current[0].keys, ["Shift"]);
        assert!(action_menu(&mut Script::default(), 1, mappings::Input::Knob { step: 4 }).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn grouped_commands_reach_every_command_once_and_headings_are_not_commands() {
        let rows = command_rows();
        let mut ids = rows
            .iter()
            .filter_map(|(_, command)| command.map(|(id, _)| id))
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(ids, (0..=14).collect::<Vec<_>>());
        let headings = rows.iter().filter(|(_, c)| c.is_none()).map(|(h, _)| *h);
        assert!(headings.eq([
            "Run",
            "Configure",
            "Programs",
            "Feedback",
            "Wispr Flow",
            "Inspect"
        ]));
        assert!(rows[0].1.is_none() && rows[1].0 == "/run"); // the cursor starts on /run
    }

    #[test]
    fn menus_save_both_banks_cancel_edit_remove_and_preserve_failed_save() {
        use Key::*;
        let dir = std::env::temp_dir().join(format!("keyai-menu-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let a = Control::from_note(10, 36).unwrap();
        let b = Control::from_note(10, 44).unwrap();
        let mut keys = Vec::new();
        for (which, save_choice) in [(1, 0), (0, 0)] {
            keys.extend([Enter, Enter, Enter, Enter, Enter]); // learn, assign, family, shortcut, modifiers
            keys.extend(std::iter::repeat_n(Down, which));
            keys.extend([Space, Enter]);
            keys.extend(std::iter::repeat_n(Down, 7));
            keys.extend([Enter, Enter, Enter]); // done, hold, no feedback
            keys.extend(std::iter::repeat_n(Down, save_choice));
            keys.push(Enter);
        }
        keys.extend([Tab, Enter, Enter, Enter, Escape]); // cancel edit after inspecting A
        keys.extend([Tab, Down, Enter, Down, Enter, Down, Enter]); // remove B explicitly
        keys.extend([Tab, Tab, Enter]); // finish
        let mut ui = Script {
            keys: keys.into(),
            pads: [a, b].into(),
            recordings: VecDeque::new(),
            screens: Vec::new(),
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
}
