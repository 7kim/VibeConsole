use crate::mappings::{Behavior, Mapping};
use crate::{Control, Event, Result};
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, uinput::VirtualDevice};
use std::collections::HashSet;
use std::io;
use std::thread;
use std::time::Duration;

pub fn key_code(name: &str) -> Result<KeyCode> {
    let linux_name = match name {
        "Ctrl" => "LEFTCTRL",
        "Shift" => "LEFTSHIFT",
        "Alt" => "LEFTALT",
        "Meta" => "LEFTMETA",
        "Escape" => "ESC",
        "LeftBracket" => "LEFTBRACE",
        "RightBracket" => "RIGHTBRACE",
        "Apostrophe" => "APOSTROPHE",
        other => &other.to_ascii_uppercase(),
    };
    format!("KEY_{linux_name}")
        .parse()
        .map_err(|_| format!("key {name:?} is unsupported by the Linux input backend").into())
}

fn modifier(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::KEY_LEFTCTRL
            | KeyCode::KEY_RIGHTCTRL
            | KeyCode::KEY_LEFTSHIFT
            | KeyCode::KEY_RIGHTSHIFT
            | KeyCode::KEY_LEFTALT
            | KeyCode::KEY_RIGHTALT
            | KeyCode::KEY_LEFTMETA
            | KeyCode::KEY_RIGHTMETA
    )
}

struct Assignment {
    control: Control,
    label: String,
    keys: Vec<KeyCode>,
    behavior: Behavior,
    active: bool,
}

pub(crate) struct Keyboard {
    assignments: Vec<Assignment>,
    down: HashSet<Control>,
    blocked: HashSet<Control>,
    // Includes attempted key-downs: emit can fail after the kernel received the key.
    held: HashSet<KeyCode>,
    running: bool,
}

impl Keyboard {
    pub(crate) fn new(mappings: &[Mapping]) -> Result<Self> {
        let mut seen = HashSet::new();
        let mut assignments = Vec::new();
        for mapping in mappings {
            mapping.validate()?;
            if !seen.insert(mapping.control) {
                return Err("duplicate assignment".into());
            }
            let keys = mapping
                .keys
                .iter()
                .map(|key| key_code(key))
                .collect::<Result<_>>()?;
            assignments.push(Assignment {
                control: mapping.control,
                label: mapping.label.clone(),
                keys,
                behavior: mapping.behavior.clone(),
                active: false,
            });
        }
        Ok(Self {
            assignments,
            down: HashSet::new(),
            blocked: mappings
                .iter()
                .filter(|m| {
                    m.behavior == Behavior::Trigger && m.control.message == crate::Message::Note
                })
                .map(|m| m.control)
                .collect(),
            held: HashSet::new(),
            running: true,
        })
    }

    pub(crate) fn observe(
        &mut self,
        event: Event,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<Option<(Control, bool)>> {
        if let Event::Pulse(control) = event {
            // A pulse acquires/releases only keys not owned by another active mapping.
            // Key-up follows immediately; no timer can delay another control's release.
            let transition = self.observe(Event::Press(control), emit)?;
            self.observe(
                Event::Release(control, crate::ReleaseType::PulseComplete),
                emit,
            )?;
            return Ok(transition.filter(|_| {
                self.assignments
                    .iter()
                    .any(|a| a.control == control && a.behavior == Behavior::Trigger)
            }));
        }
        let (pad, pressed) = match event {
            Event::Press(pad) if self.down.insert(pad) => {
                if self.blocked.contains(&pad) {
                    return Ok(None);
                }
                (pad, true)
            }
            Event::Release(pad, _) | Event::UnmatchedRelease(pad) => {
                self.blocked.remove(&pad);
                if !self.down.remove(&pad) {
                    return Ok(None);
                }
                (pad, false)
            }
            _ => return Ok(None),
        };
        if !self.running {
            return Ok(None);
        }
        let Some(index) = self.assignments.iter().position(|item| item.control == pad) else {
            return Ok(None);
        };
        let assignment = &self.assignments[index];
        let active = match assignment.behavior {
            Behavior::Trigger => return Ok(pressed.then_some((pad, true))),
            Behavior::Hold | Behavior::Pulse => pressed,
            Behavior::Toggle if pressed => !assignment.active,
            Behavior::Toggle => return Ok(None),
        };
        if active == assignment.active {
            return Ok(None);
        }
        let mut keys = assignment.keys.clone();
        if !active {
            keys.reverse();
        }
        for key in keys {
            // ponytail: scan active assignments; use counts if large setups make this measurable.
            let shared = self
                .assignments
                .iter()
                .enumerate()
                .any(|(other, item)| other != index && item.active && item.keys.contains(&key));
            if shared {
                continue;
            }
            if active {
                self.held.insert(key);
                emit(key, true)?;
            } else {
                emit(key, false)?;
                self.held.remove(&key);
            }
        }
        self.assignments[index].active = active;
        Ok(Some((pad, active)))
    }

    pub(crate) fn pause(
        &mut self,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        self.running = false;
        self.clear(emit)
    }

    pub(crate) fn clear(
        &mut self,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        let down = self
            .down
            .iter()
            .filter(|c| c.message == crate::Message::Note)
            .copied()
            .collect();
        let blocked = self.blocked.clone();
        let result = self.cleanup(emit);
        self.down = down;
        self.blocked = blocked; // repeats cannot reacquire keys until a real release
        result
    }

    pub(crate) fn replace(&mut self, mappings: &[Mapping]) -> Result<()> {
        if self.running || !self.held.is_empty() {
            return Err("pause and release keys before editing mappings".into());
        }
        let mut next = Self::new(mappings)?;
        next.down = self.down.clone();
        next.blocked.extend(&self.blocked);
        next.running = false;
        *self = next;
        Ok(())
    }

    pub(crate) fn resume(&mut self) {
        self.running = true;
    }

    pub(crate) fn block_down(&mut self, down: &HashSet<Control>) {
        self.down = down.clone();
        self.blocked.extend(down);
    }

    pub(crate) fn prepare_connection(
        &mut self,
        mappings: &[Mapping],
        down: &HashSet<Control>,
        reconnected: &mut bool,
    ) -> Result<bool> {
        self.replace(mappings)?;
        self.block_down(down);
        let fresh = std::mem::take(reconnected);
        if fresh {
            self.block_reconnected();
        }
        Ok(fresh)
    }

    pub(crate) fn block_reconnected(&mut self) {
        // ponytail: no verified held-pad snapshot; require an observed release for
        // every pad after reconnect. Replace with a verified snapshot when available.
        self.blocked = self
            .assignments
            .iter()
            .filter(|a| a.control.message == crate::Message::Note)
            .map(|a| a.control)
            .collect();
    }

    pub(crate) fn active(&self, control: Control) -> bool {
        self.assignments
            .iter()
            .any(|assignment| assignment.control == control && assignment.active)
    }

    pub(crate) fn status(&self) -> String {
        let active = self
            .assignments
            .iter()
            .filter(|a| a.active)
            .map(|a| a.label.clone())
            .collect::<Vec<_>>();
        let mut down = self
            .down
            .iter()
            .map(|p| {
                self.assignments
                    .iter()
                    .find(|a| a.control == *p)
                    .map(|a| a.label.clone())
                    .unwrap_or_else(|| p.label())
            })
            .collect::<Vec<_>>();
        down.sort();
        format!(
            "{} | {} saved (/list shows keys/modes) | {} await release\nActive holds/toggles: {}\nPhysically down: {}",
            if self.running { "RUNNING" } else { "PAUSED" },
            self.assignments.len(),
            self.blocked.len(),
            if active.is_empty() {
                "none".into()
            } else {
                active.join(", ")
            },
            if down.is_empty() {
                "none".into()
            } else {
                down.join(", ")
            }
        )
    }

    fn cleanup(
        &mut self,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut keys: Vec<_> = self.held.iter().copied().collect();
        keys.sort_by_key(|key| (modifier(*key), key.0));
        let mut failure = None;
        for key in keys {
            match emit(key, false) {
                Ok(()) => {
                    self.held.remove(&key);
                }
                Err(error) => {
                    failure = Some(error);
                }
            }
        }
        self.down.clear();
        self.blocked.clear();
        for assignment in &mut self.assignments {
            assignment.active = false;
        }
        failure.map_or(Ok(()), Err)
    }
}

fn create_output() -> Result<VirtualDevice> {
    // Advertise a standard keyboard range so modifier-only mappings are classified as keyboards.
    let mut supported = AttributeSet::<KeyCode>::new();
    for code in 1..=KeyCode::KEY_RIGHTMETA.0 {
        supported.insert(KeyCode(code));
    }
    let output = VirtualDevice::builder()
        .and_then(|builder| {
            builder
                .name("KeyAI virtual keyboard")
                .with_keys(&supported)?
                .build()
        })
        .map_err(|error| {
            format!(
                "cannot create keyboard through /dev/uinput: {error}; see README.md for permissions"
            )
        })?;
    Ok(output)
}

pub(crate) struct Output {
    pub(crate) keyboard: Keyboard,
    device: VirtualDevice,
    pub(crate) node: std::path::PathBuf,
}

impl Output {
    pub(crate) fn new(mappings: &[Mapping]) -> Result<Self> {
        let mut keyboard = Keyboard::new(mappings)?;
        keyboard.running = false;
        let mut device = create_output()?;
        let started = std::time::Instant::now();
        let node = loop {
            match device.enumerate_dev_nodes_blocking() {
                Ok(mut nodes) => {
                    if let Some(path) = nodes.next().transpose()? {
                        if path.exists() {
                            break path;
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            if started.elapsed() > Duration::from_secs(2) {
                return Err("keyboard registration timed out".into());
            }
            thread::sleep(Duration::from_millis(20));
        };
        Ok(Self {
            keyboard,
            device,
            node,
        })
    }
    pub(crate) fn observe(&mut self, event: Event) -> Result<Option<(Control, bool)>> {
        let device = &mut self.device;
        Ok(self.keyboard.observe(event, &mut |key, down| {
            device.emit(&[InputEvent::new(EventType::KEY.0, key.0, i32::from(down))])
        })?)
    }
    pub(crate) fn pause(&mut self) -> Result<()> {
        let device = &mut self.device;
        self.keyboard.pause(&mut |key, down| {
            device.emit(&[InputEvent::new(EventType::KEY.0, key.0, i32::from(down))])
        })?;
        Ok(())
    }
    pub(crate) fn clear(&mut self) -> Result<()> {
        let device = &mut self.device;
        self.keyboard.clear(&mut |key, down| {
            device.emit(&[InputEvent::new(EventType::KEY.0, key.0, i32::from(down))])
        })?;
        Ok(())
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        if let Err(error) = self.pause() {
            eprintln!("Key release failed: {error}; desktop recovery is unverified.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Detector, ReleaseType};
    use std::sync::mpsc;

    #[test]
    fn trigger_pad_piano_knob_duplicates_pause_and_slow_action_never_delay_key_up() {
        use crate::{
            actions::{Action, Dispatcher},
            mappings::Input,
        };
        let hold = Control::from_note(10, 36).unwrap();
        let pad = Control::from_note(10, 37).unwrap();
        let piano = Control::note(1, 48).unwrap();
        let knob = Control {
            channel: 1,
            id: 70,
            message: crate::Message::Cc,
            direction: 1,
        };
        let maps = vec![
            Mapping::new(hold, "Shift", "hold").unwrap(),
            Mapping::new_action(
                pad,
                Input::Pad,
                Action::Application("/test/one.desktop".into()),
            )
            .unwrap(),
            Mapping::new_action(piano, Input::Piano, Action::OutputMute).unwrap(),
            Mapping::new_action(
                knob,
                Input::Knob { step: 4 },
                Action::Volume { up: true, step: 5 },
            )
            .unwrap(),
        ];
        let mut keyboard = Keyboard::new(&maps).unwrap();
        let mut emitted = Vec::new();
        let mut emit = |k, d| {
            emitted.push((k, d));
            Ok(())
        };
        assert_eq!(
            keyboard.observe(Event::Press(pad), &mut emit).unwrap(),
            None
        ); // first startup tap only arms
        keyboard
            .observe(Event::Release(pad, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        assert_eq!(
            keyboard.observe(Event::Press(pad), &mut emit).unwrap(),
            Some((pad, true))
        );
        for event in [
            Event::Press(pad),
            Event::DuplicatePress(pad),
            Event::Pressure(pad),
        ] {
            assert_eq!(keyboard.observe(event, &mut emit).unwrap(), None);
        }
        keyboard
            .observe(Event::Release(pad, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard
            .observe(Event::UnmatchedRelease(piano), &mut emit)
            .unwrap();
        assert_eq!(
            keyboard.observe(Event::Press(piano), &mut emit).unwrap(),
            Some((piano, true))
        );
        assert_eq!(
            keyboard.observe(Event::Press(piano), &mut emit).unwrap(),
            None
        );
        let mut motion = crate::motion::Motion::new(&maps).unwrap();
        assert!(motion.observe([0xb0, 70, 0]).unwrap().is_empty());
        let pulse = motion.observe([0xb0, 70, 4]).unwrap().pop().unwrap();
        assert_eq!(
            keyboard.observe(pulse, &mut emit).unwrap(),
            Some((knob, true))
        );
        assert!(motion.observe([0xb0, 70, 4]).unwrap().is_empty());
        let (started, rx) = mpsc::channel();
        let (completed, done) = mpsc::channel();
        let dispatch = Dispatcher::with(move |action, _, cancelled| {
            started.send(action.clone()).unwrap();
            while !cancelled() {
                thread::sleep(Duration::from_millis(1));
            }
            completed.send(()).unwrap();
            Err("slow launch cancelled".into())
        });
        keyboard.observe(Event::Press(hold), &mut emit).unwrap();
        dispatch.submit(maps[1].action.clone(), None).unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        dispatch.submit(Action::OutputMute, None).unwrap();
        let start = std::time::Instant::now();
        keyboard
            .observe(Event::Release(hold, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        assert!(start.elapsed() < Duration::from_millis(100));
        keyboard.pause(&mut emit).unwrap();
        dispatch.cancel();
        done.recv_timeout(Duration::from_secs(1)).unwrap();
        keyboard
            .observe(Event::Release(piano, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        assert_eq!(
            keyboard.observe(Event::Press(pad), &mut emit).unwrap(),
            None
        );
        keyboard.resume();
        assert_eq!(
            keyboard.observe(Event::Press(pad), &mut emit).unwrap(),
            None
        );
        keyboard
            .observe(Event::Release(pad, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        assert_eq!(
            keyboard.observe(Event::Press(pad), &mut emit).unwrap(),
            Some((pad, true))
        );
        keyboard.pause(&mut emit).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err()); // queued mute cancelled
        assert_eq!(
            emitted,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTSHIFT, false)
            ]
        );
    }

    #[test]
    fn toggle_bulbs_follow_real_latches_and_cleanup_without_desktop_output() {
        let control = Control::from_note(10, 36).unwrap();
        let mapping = Mapping::new(control, "Shift", "toggle").unwrap();
        let mut keyboard = Keyboard::new(&[mapping.clone()]).unwrap();
        let mut emit = |_, _| Ok(());
        let view = |keyboard: &Keyboard| {
            crate::screen::lights(
                &[(
                    format!("{}\n{}", mapping.label, mapping.action_label()),
                    keyboard.active(control),
                )],
                34,
            )
            .join("\n")
        };
        assert!(view(&keyboard).contains(crate::screen::bulb(false)));
        keyboard.observe(Event::Press(control), &mut emit).unwrap();
        keyboard
            .observe(
                Event::Release(control, crate::ReleaseType::NoteOff),
                &mut emit,
            )
            .unwrap();
        assert!(view(&keyboard).contains(crate::screen::bulb(true)));
        assert!(view(&keyboard).contains("Shift"));
        keyboard.observe(Event::Press(control), &mut emit).unwrap();
        assert!(view(&keyboard).contains(crate::screen::bulb(false)));
        keyboard
            .observe(
                Event::Release(control, crate::ReleaseType::NoteOnVelocityZero),
                &mut emit,
            )
            .unwrap();
        keyboard.observe(Event::Press(control), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        assert!(view(&keyboard).contains(crate::screen::bulb(false)));
        keyboard.replace(&[mapping.clone()]).unwrap();
        assert!(view(&keyboard).contains(crate::screen::bulb(false)));
    }

    #[test]
    fn observed_piano_note_release_and_pad_share_keys_and_cleanup() {
        // 2026-10-07 passive capture: 90 30 7f, 80 30 00; emitted pitch is the identity.
        let piano = Control::note(1, 48).unwrap();
        let pad = Control::from_note(10, 36).unwrap();
        let mut mapping = Mapping::new(piano, "Shift+K", "hold").unwrap();
        mapping.input = crate::mappings::Input::Piano;
        let mut keyboard =
            Keyboard::new(&[Mapping::new(pad, "Shift", "toggle").unwrap(), mapping]).unwrap();
        let mut detector = Detector::default();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        for bytes in [
            [0x99, 36, 127],
            [0x90, 48, 127],
            [0x90, 48, 64],
            [0x80, 48, 0],
        ] {
            if let Some(event) = detector.observe(bytes) {
                keyboard.observe(event, &mut emit).unwrap();
            }
        }
        keyboard.pause(&mut emit).unwrap();
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_K, true),
                (KeyCode::KEY_K, false),
                (KeyCode::KEY_LEFTSHIFT, false)
            ]
        );
        assert!(keyboard.held.is_empty());
    }

    #[test]
    fn context_switch_releases_latch_and_requires_new_context_release() {
        let old = Control::from_note(10, 36).unwrap();
        let new = Control::note(3, 60).unwrap();
        let mut keyboard = Keyboard::new(&[Mapping::new(old, "Shift", "toggle").unwrap()]).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(old), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        keyboard
            .replace(&[Mapping::new(new, "Ctrl", "hold").unwrap()])
            .unwrap();
        keyboard.block_reconnected();
        keyboard.resume();
        keyboard.observe(Event::Press(new), &mut emit).unwrap();
        keyboard
            .observe(Event::UnmatchedRelease(new), &mut emit)
            .unwrap();
        keyboard.observe(Event::Press(new), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTSHIFT, false),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_LEFTCTRL, false)
            ]
        );
    }

    #[test]
    fn disconnect_reconnect_is_inactive_and_waits_for_release_without_replaying_toggles() {
        let a = Control::from_note(10, 36).unwrap();
        let b = Control::from_note(10, 44).unwrap();
        let mappings = [
            Mapping::new(a, "Shift", "toggle").unwrap(),
            Mapping::new(b, "Ctrl", "hold").unwrap(),
        ];
        let mut keyboard = Keyboard::new(&mappings).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        let mut reconnected = true;
        assert!(
            keyboard
                .prepare_connection(&mappings, &HashSet::new(), &mut reconnected)
                .unwrap()
        );
        keyboard.resume();
        // Unknown already-held pads and repeats after reconnect cannot reactivate.
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard
            .observe(Event::Release(a, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard
            .observe(Event::UnmatchedRelease(b), &mut emit)
            .unwrap(); // observed release arms a pad even without a received press
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        // Ordinary resume must not guard already-armed, released pads again.
        keyboard
            .observe(Event::Release(a, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard
            .observe(Event::Release(b, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        assert!(
            !keyboard
                .prepare_connection(&mappings, &HashSet::new(), &mut reconnected)
                .unwrap()
        );
        keyboard.resume();
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTSHIFT, false),
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_LEFTCTRL, false),
                (KeyCode::KEY_LEFTSHIFT, false),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_LEFTCTRL, false)
            ]
        );
        assert!(!keyboard.assignments.iter().any(|a| a.active));
    }

    #[test]
    fn session_toggle_pause_edit_resume_and_release_all_wait_for_fresh_presses() {
        let a = Control::from_note(10, 36).unwrap();
        let b = Control::from_note(10, 44).unwrap();
        let mut keyboard = Keyboard::new(&[Mapping::new(a, "Shift", "toggle").unwrap()]).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        assert!(!keyboard.assignments[0].active);
        // Configuration occurs while paused and tracks controls pressed during editing.
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard
            .replace(&[
                Mapping::new(a, "Ctrl+K", "hold").unwrap(),
                Mapping::new(b, "Shift", "toggle").unwrap(),
            ])
            .unwrap();
        keyboard.resume();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard
            .observe(Event::Release(a, crate::ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard
            .observe(Event::Release(b, crate::ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard.clear(&mut emit).unwrap();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard
            .observe(Event::Release(a, crate::ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard
            .observe(Event::Release(b, crate::ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTSHIFT, false),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_K, true),
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_K, false),
                (KeyCode::KEY_LEFTCTRL, false),
                (KeyCode::KEY_LEFTSHIFT, false),
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTSHIFT, false),
            ]
        );
        let previous = keyboard.assignments.len();
        let mut invalid = Mapping::new(a, "Shift", "hold").unwrap();
        invalid.keys = vec!["Unknown".into()];
        assert!(keyboard.replace(&[invalid]).is_err());
        assert_eq!(keyboard.assignments.len(), previous);
    }

    #[test]
    fn stream_release_arrives_during_silence_before_eof() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let pad = Control::from_note(10, 36).unwrap();
        let mut keyboard = Keyboard::new(&[Mapping::new(pad, "Shift", "hold").unwrap()]).unwrap();
        let (mut writer, reader) = UnixStream::pair().unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut detector = Detector::default();
            let mut emit = |key, down| {
                sender.send((key, down)).unwrap();
                Ok(())
            };
            for message in crate::midi::messages(reader) {
                if let Some(event) = detector.observe(message.unwrap()) {
                    keyboard.observe(event, &mut emit).unwrap();
                }
            }
        });
        writer
            .write_all(&[
                0x99, 36, 32, 0xA9, 36, 64, 0xF0, 0x47, 0, 0x49, 0x67, 1, 0xF8, 0xF7, 0x89, 36, 0,
            ])
            .unwrap();
        let press = receiver.recv_timeout(Duration::from_secs(1));
        let release = receiver.recv_timeout(Duration::from_secs(1));
        // Close only after checking: EOF must not be what delivers the release.
        drop(writer);
        worker.join().unwrap();
        assert_eq!(press.unwrap(), (KeyCode::KEY_LEFTSHIFT, true));
        assert_eq!(
            release.expect("release must arrive without a later message or EOF"),
            (KeyCode::KEY_LEFTSHIFT, false)
        );
    }

    #[test]
    fn holds_toggles_overlap_order_cleanup_and_failed_output() {
        let a = Control::from_note(10, 36).unwrap();
        let b = Control::from_note(10, 37).unwrap();
        let c = Control::from_note(10, 44).unwrap();
        let mappings = vec![
            Mapping::new(a, "Ctrl", "hold").unwrap(),
            Mapping::new(b, "C+Ctrl", "toggle").unwrap(),
            Mapping::new(c, "Shift+Ctrl", "toggle").unwrap(),
        ];
        let mut keyboard = Keyboard::new(&mappings).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        let mut detector = Detector::default();
        for bytes in [
            [0x99, 36, 20],
            [0x99, 37, 20],
            [0x99, 37, 30],
            [0xA9, 37, 40],
            [0x89, 37, 0],
            [0x99, 44, 20],
            [0x99, 44, 0],
            [0x89, 36, 0],
            [0x99, 37, 20],
            [0x89, 37, 0],
        ] {
            if let Some(event) = detector.observe(bytes) {
                keyboard.observe(event, &mut emit).unwrap();
            }
        }
        assert_eq!(
            keyboard
                .assignments
                .iter()
                .map(|item| item.active)
                .collect::<Vec<_>>(),
            [false, false, true]
        );
        keyboard.cleanup(&mut emit).unwrap();
        assert!(keyboard.held.is_empty());
        assert!(keyboard.down.is_empty());
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_C, true),
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_C, false),
                (KeyCode::KEY_LEFTCTRL, false),
                (KeyCode::KEY_LEFTSHIFT, false)
            ]
        );

        // A fresh engine starts inactive and releases nonmodifiers before modifiers.
        let mut keyboard =
            Keyboard::new(&[Mapping::new(a, "K+Shift+Ctrl", "hold").unwrap()]).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Pressure(a), &mut emit).unwrap();
        keyboard
            .observe(Event::Release(a, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.cleanup(&mut emit).unwrap();
        assert_eq!(
            recorded[..6],
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_K, true),
                (KeyCode::KEY_K, false),
                (KeyCode::KEY_LEFTCTRL, false),
                (KeyCode::KEY_LEFTSHIFT, false)
            ]
        );
        assert_eq!(recorded[9].0, KeyCode::KEY_K);

        // Failed key-down remains a cleanup candidate, with no successful activation reported.
        let mut fail = |_, _| Err(io::Error::other("output failed"));
        assert!(keyboard.observe(Event::Press(a), &mut fail).is_err());
        assert!(!keyboard.assignments[0].active);
        let mut released = Vec::new();
        keyboard
            .cleanup(&mut |key, down| {
                released.push((key, down));
                Ok(())
            })
            .unwrap();
        assert_eq!(released, [(KeyCode::KEY_LEFTSHIFT, false)]);

        // Two holds share Ctrl until its last owner lifts.
        let mut keyboard = Keyboard::new(&[
            Mapping::new(a, "Ctrl", "hold").unwrap(),
            Mapping::new(b, "Ctrl+C", "hold").unwrap(),
        ])
        .unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        for event in [
            Event::Press(a),
            Event::Press(b),
            Event::Release(b, ReleaseType::NoteOff),
            Event::Release(a, ReleaseType::NoteOff),
        ] {
            keyboard.observe(event, &mut emit).unwrap();
        }
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_C, true),
                (KeyCode::KEY_C, false),
                (KeyCode::KEY_LEFTCTRL, false)
            ]
        );

        // Cleanup attempts every key even if the first release fails, then permits a retry.
        keyboard
            .observe(Event::Press(b), &mut |_, _| Ok(()))
            .unwrap();
        let mut attempts = Vec::new();
        assert!(
            keyboard
                .cleanup(&mut |key, _| {
                    attempts.push(key);
                    if key == KeyCode::KEY_C {
                        Err(io::Error::other("release failed"))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
        assert_eq!(attempts, [KeyCode::KEY_C, KeyCode::KEY_LEFTCTRL]);
        assert_eq!(keyboard.held, HashSet::from([KeyCode::KEY_C]));
        keyboard.cleanup(&mut |_, _| Ok(())).unwrap();
    }
}
