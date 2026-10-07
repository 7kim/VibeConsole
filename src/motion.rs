use crate::{
    Control, Event, Message, ReleaseType, Result,
    actions::Action,
    mappings::{Input, Mapping},
};
use std::time::{Duration, Instant};

/// A choice knob must rest in a new zone this long before its prompt is sent.
pub const SETTLE: Duration = Duration::from_millis(300);

/// Absolute-position knob (choice or value). Position is unknown until the first message.
struct Dial {
    control: Control,
    action: Action,
    /// Zone whose prompt was last sent.
    sent: Option<usize>,
    /// Zone entered and when, not yet settled.
    pending: Option<(usize, Instant)>,
    /// Last value written.
    written: Option<i64>,
}

/// What a dial asks the session to do.
#[derive(Debug, PartialEq)]
pub enum Dialed {
    /// Send this choice's prompt; (zone, zone count) for the notice.
    Send(Action, usize, usize),
    /// Atomically write `text` to `path`.
    Write(String, String),
}

/// Zone 0..count for a 0..=127 position, equal-width.
pub fn zone(position: u8, count: usize) -> usize {
    usize::from(position.min(127)) * count / 128
}

/// min..max (scaled) for a 0..=127 position, rounded to the nearest step.
pub fn scaled(position: u8, min: i64, max: i64) -> i64 {
    let span = i128::from(max - min) * i128::from(position.min(127));
    min + ((span * 2 + 127) / 254) as i64
}

struct State {
    control: Control,
    input: Input,
    last: Option<i32>,
    remainder: i32,
    direction: i32,
    armed: bool,
    active: bool,
}

#[derive(Default)]
pub struct Motion {
    states: Vec<State>,
    dials: Vec<Dial>,
}

impl Motion {
    pub fn new(mappings: &[Mapping]) -> Result<Self> {
        let mut states = Vec::new();
        let mut dials = Vec::new();
        for mapping in mappings {
            mapping.validate()?;
            if mapping.action.dial() {
                dials.push(Dial {
                    control: mapping.control,
                    action: mapping.action.clone(),
                    sent: None,
                    pending: None,
                    written: None,
                });
            } else if matches!(
                mapping.input,
                Input::Knob { .. } | Input::RelativeKnob { .. } | Input::Joystick { .. }
            ) {
                states.push(State {
                    control: mapping.control,
                    input: mapping.input,
                    last: None,
                    remainder: 0,
                    direction: 0,
                    armed: false,
                    active: false,
                });
            }
        }
        Ok(Self { states, dials })
    }

    /// Feed a MIDI message to choice/value knobs; value changes are returned as writes.
    pub fn dial(&mut self, bytes: [u8; 3], now: Instant) -> Vec<Dialed> {
        let [status, id, raw] = bytes;
        let mut out = Vec::new();
        for dial in &mut self.dials {
            let control = dial.control;
            if status & 0xf0 != 0xb0 || control.channel != (status & 15) + 1 || control.id != id {
                continue;
            }
            // A knob learned as "decreasing values" counts from its other end.
            let position = if control.direction < 0 {
                127 - raw.min(127)
            } else {
                raw
            };
            match &dial.action {
                Action::Choice(choices) => {
                    let zone = zone(position, choices.len());
                    if dial.sent == Some(zone) {
                        dial.pending = None;
                    } else if dial.pending.map(|(z, _)| z) != Some(zone) {
                        dial.pending = Some((zone, now));
                    }
                }
                Action::Value {
                    path,
                    min,
                    max,
                    decimals,
                } => {
                    let value = scaled(position, *min, *max);
                    if dial.written != Some(value) {
                        dial.written = Some(value);
                        out.push(Dialed::Write(
                            path.clone(),
                            format!("{}\n", crate::actions::fixed(value, *decimals)),
                        ));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Choice prompts whose knob has rested in a new zone for `SETTLE`.
    pub fn settle(&mut self, now: Instant) -> Vec<Dialed> {
        let mut out = Vec::new();
        for dial in &mut self.dials {
            let (Some((zone, since)), Action::Choice(choices)) = (dial.pending, &dial.action)
            else {
                continue;
            };
            if now.saturating_duration_since(since) >= SETTLE {
                dial.pending = None;
                dial.sent = Some(zone);
                out.push(Dialed::Send(choices[zone].clone(), zone, choices.len()));
            }
        }
        out
    }

    pub fn observe(&mut self, bytes: [u8; 3]) -> Result<Vec<Event>> {
        let [status, id, raw] = bytes;
        let (message, id, value) = match status & 0xf0 {
            0xb0 => (Message::Cc, id, i32::from(raw)),
            0xe0 => (Message::Bend, 0, i32::from(id) | (i32::from(raw) << 7)),
            _ => return Ok(Vec::new()),
        };
        let mut releases = Vec::new();
        let mut presses = Vec::new();
        for state in &mut self.states {
            let control = state.control;
            if control.message != message
                || control.channel != (status & 15) + 1
                || control.id != id
            {
                continue;
            }
            match state.input {
                Input::RelativeKnob { step } => {
                    let ticks = match raw {
                        0 => 0,
                        1..=63 => i32::from(raw),
                        65..=127 => i32::from(raw) - 128,
                        _ => return Err("invalid relative knob tick; output paused".into()),
                    };
                    if ticks.signum() == i32::from(control.direction) {
                        for _ in 0..ticks.unsigned_abs() * u32::from(step) {
                            presses.push(Event::Pulse(control));
                        }
                    }
                }
                Input::Knob { step } => {
                    if let Some(last) = state.last {
                        let delta = value - last;
                        if delta != 0 {
                            if delta.signum() != state.direction {
                                state.remainder = 0;
                            }
                            state.direction = delta.signum();
                            state.remainder += delta;
                            if state.remainder.abs() >= i32::from(step) {
                                if state.remainder.signum() == i32::from(control.direction) {
                                    presses.push(Event::Pulse(control));
                                }
                                // ponytail: at most one pulse per MIDI sample; finer steps need
                                // denser controller messages, not a queued shortcut burst.
                                state.remainder %= i32::from(step);
                            }
                        }
                    }
                    state.last = Some(value);
                }
                Input::Joystick {
                    center,
                    min,
                    max,
                    deadzone,
                    hysteresis,
                } => {
                    if value < i32::from(min) || value > i32::from(max) {
                        return Err("joystick value outside observed calibration; paused, recalibrate this preset".into());
                    }
                    let delta = value - i32::from(center);
                    let release = i32::from(deadzone - hysteresis);
                    let press = i32::from(deadzone + hysteresis);
                    if !state.armed {
                        // A reconnecting controller may report center before a held direction.
                        // Require an excursion followed by neutral before activating.
                        state.armed = delta.abs() <= release
                            && state
                                .last
                                .is_some_and(|last| (last - i32::from(center)).abs() > release);
                        state.last = Some(value);
                        continue;
                    }
                    let distance = delta * i32::from(control.direction);
                    if state.active && distance <= release {
                        state.active = false;
                        releases.push(Event::Release(control, ReleaseType::Neutral));
                    } else if !state.active && distance > press {
                        state.active = true;
                        presses.push(Event::Press(control));
                    }
                }
                _ => unreachable!(),
            }
        }
        releases.extend(presses); // release the prior direction before acquiring the next
        Ok(releases)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Detector, keyboard::Keyboard};
    use evdev::KeyCode;

    fn knob(direction: i8, keys: &str) -> Mapping {
        let mut mapping = Mapping::new(
            Control {
                message: Message::Cc,
                channel: 1,
                id: 70,
                direction,
            },
            keys,
            "pulse",
        )
        .unwrap();
        mapping.input = Input::Knob { step: 4 };
        mapping.label = format!("Knob 1 {direction}");
        mapping
    }

    #[test]
    fn captured_absolute_knob_stationary_reversal_boundaries_shared_owner_and_reset() {
        // Passive capture contained CC70 channel 1: 1..106, then 105..0.
        let pad = Control::from_note(10, 36).unwrap();
        let mappings = [
            Mapping::new(pad, "Ctrl", "hold").unwrap(),
            knob(1, "Ctrl+K"),
            knob(-1, "Ctrl+J"),
        ];
        let mut motion = Motion::new(&mappings).unwrap();
        let mut keyboard = Keyboard::new(&mappings).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(pad), &mut emit).unwrap();
        for value in [0, 1, 1, 4, 5, 4, 0, 0] {
            for event in motion.observe([0xb0, 70, value]).unwrap() {
                keyboard.observe(event, &mut emit).unwrap();
            }
        }
        // Pause drops any partially accumulated movement; first post-reset sample only seeds.
        keyboard.pause(&mut emit).unwrap();
        motion = Motion::new(&mappings).unwrap();
        keyboard.resume();
        assert!(motion.observe([0xb0, 70, 127]).unwrap().is_empty());
        assert!(motion.observe([0xb0, 70, 127]).unwrap().is_empty());
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_K, true),
                (KeyCode::KEY_K, false),
                (KeyCode::KEY_J, true),
                (KeyCode::KEY_J, false),
                (KeyCode::KEY_LEFTCTRL, false)
            ]
        );
        assert!(Detector::default().observe([0xb0, 70, 0]).is_none()); // no invented Note release
    }

    #[test]
    fn captured_relative_ticks_pulse_in_both_directions_without_replay() {
        let mut clockwise = knob(1, "Right");
        clockwise.input = Input::RelativeKnob { step: 1 };
        let mut counterclockwise = knob(-1, "Left");
        counterclockwise.input = Input::RelativeKnob { step: 1 };
        clockwise.control.id = 16;
        counterclockwise.control.id = 16;
        let mappings = [clockwise, counterclockwise];
        let mut motion = Motion::new(&mappings).unwrap();
        for (raw, direction, count) in [(1, 1, 1), (2, 1, 2), (127, -1, 1), (126, -1, 2), (0, 0, 0)]
        {
            let events = motion.observe([0xb0, 16, raw]).unwrap();
            assert_eq!(events.len(), count);
            assert!(events.iter().all(|event| match event {
                Event::Pulse(control) => control.direction == direction,
                _ => false,
            }));
        }
        motion = Motion::new(&mappings).unwrap();
        assert!(motion.observe([0xb0, 16, 0]).unwrap().is_empty());
        assert_eq!(motion.observe([0xb0, 16, 127]).unwrap().len(), 1);
        for _ in 0..130 {
            assert_eq!(motion.observe([0xb0, 16, 1]).unwrap().len(), 1);
        }
        assert!(motion.observe([0xb0, 16, 64]).is_err());
    }

    #[test]
    fn captured_joystick_center_jitter_reversal_diagonal_overlap_and_inactive_resume() {
        // Passive capture: E0 00 00 / E0 7F 7F / E0 00 40; CC1 0..127..0.
        let pad = Control::from_note(10, 36).unwrap();
        let mut right = Mapping::new(
            Control {
                message: Message::Bend,
                channel: 1,
                id: 0,
                direction: 1,
            },
            "Shift+K",
            "hold",
        )
        .unwrap();
        right.input = Input::Joystick {
            center: 8192,
            min: 0,
            max: 16383,
            deadzone: 1024,
            hysteresis: 128,
        };
        right.label = "Right".into();
        let mut left = right.clone();
        left.control.direction = -1;
        left.keys = crate::mappings::parse_shortcut("Shift+J").unwrap();
        left.label = "Left".into();
        let mut vertical = Mapping::new(
            Control {
                message: Message::Cc,
                channel: 1,
                id: 1,
                direction: 1,
            },
            "Ctrl",
            "hold",
        )
        .unwrap();
        vertical.input = Input::Joystick {
            center: 0,
            min: 0,
            max: 127,
            deadzone: 16,
            hysteresis: 4,
        };
        vertical.label = "Vertical excursion".into();
        let mappings = [
            Mapping::new(pad, "Shift", "hold").unwrap(),
            left,
            right,
            vertical,
        ];
        let mut motion = Motion::new(&mappings).unwrap();
        let mut keyboard = Keyboard::new(&mappings).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(pad), &mut emit).unwrap();
        let bend = |n: u16| [0xe0, (n & 127) as u8, (n >> 7) as u8];
        for value in [
            16383, 8192, 8200, 10000, 9350, 9200, 9000, 10000, 6000, 8192,
        ] {
            for event in motion.observe(bend(value)).unwrap() {
                keyboard.observe(event, &mut emit).unwrap();
            }
        }
        for bytes in [
            [0xb0, 1, 0],
            [0xb0, 1, 127],
            [0xb0, 1, 0],
            [0xb0, 1, 127],
            bend(10000),
        ] {
            for event in motion.observe(bytes).unwrap() {
                keyboard.observe(event, &mut emit).unwrap();
            }
        }
        keyboard.pause(&mut emit).unwrap();
        motion = Motion::new(&mappings).unwrap();
        keyboard.resume();
        for value in [10000, 8192, 10000, 8192] {
            for event in motion.observe(bend(value)).unwrap() {
                keyboard.observe(event, &mut emit).unwrap();
            }
        }
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_K, true),
                (KeyCode::KEY_K, false),
                (KeyCode::KEY_K, true),
                (KeyCode::KEY_K, false),
                (KeyCode::KEY_J, true),
                (KeyCode::KEY_J, false),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_K, true),
                (KeyCode::KEY_K, false),
                (KeyCode::KEY_LEFTCTRL, false),
                (KeyCode::KEY_LEFTSHIFT, false),
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_K, true),
                (KeyCode::KEY_K, false),
                (KeyCode::KEY_LEFTSHIFT, false),
            ]
        );
        let mut limited = mappings[2].clone();
        limited.input = Input::Joystick {
            center: 8192,
            min: 100,
            max: 16000,
            deadzone: 1024,
            hysteresis: 128,
        };
        assert!(Motion::new(&[limited]).unwrap().observe(bend(0)).is_err());
    }

    #[test]
    fn held_joystick_does_not_replay_after_reconnect_center_packet() {
        let control = Control {
            message: Message::Bend,
            channel: 1,
            id: 0,
            direction: 1,
        };
        let mut mapping = Mapping::new(control, "Shift", "hold").unwrap();
        mapping.input = Input::Joystick {
            center: 8192,
            min: 0,
            max: 16383,
            deadzone: 1023,
            hysteresis: 0,
        };
        let mut motion = Motion::new(&[mapping]).unwrap();
        let center = [0xe0, 0, 64];
        let right = [0xe0, 127, 127];

        assert!(motion.observe(center).unwrap().is_empty());
        assert!(motion.observe(right).unwrap().is_empty());
        assert!(motion.observe(center).unwrap().is_empty());
        assert_eq!(motion.observe(right).unwrap(), vec![Event::Press(control)]);
    }

    #[test]
    fn choice_zones_settle_without_sending_while_sweeping_and_value_knob_scales() {
        use crate::actions::{Action, Target, fixed, write_atomic};
        assert_eq!(
            (zone(0, 2), zone(63, 2), zone(64, 2), zone(127, 2)),
            (0, 0, 1, 1)
        );
        assert_eq!(
            (zone(0, 16), zone(7, 16), zone(8, 16), zone(127, 16)),
            (0, 0, 1, 15)
        );
        assert_eq!(
            (0..=127).map(|p| zone(p, 16)).filter(|z| *z == 3).count(),
            8
        );
        assert_eq!(
            (scaled(0, 0, 100), scaled(127, 0, 100), scaled(64, 0, 100)),
            (0, 100, 50)
        );
        assert_eq!(scaled(127, -500, 1500), 1500);
        assert_eq!(fixed(42, 2), "0.42");
        assert_eq!(fixed(-5, 1), "-0.5");
        assert_eq!(fixed(100, 2), "1.00");
        assert_eq!(fixed(7, 0), "7");
        let prompt = |file: &str| Action::Prompt {
            file: file.into(),
            target: Target::Agent("claude".into()),
            enter: true,
        };
        let cc = |id, direction| Control {
            message: Message::Cc,
            channel: 1,
            id,
            direction,
        };
        let dial = |control, action| {
            let mut m = Mapping::new_action(control, Input::Knob { step: 4 }, action).unwrap();
            m.label = format!("Knob {}", control.id);
            m
        };
        let choice = Action::Choice(vec![prompt("sonnet"), prompt("opus")]);
        let value = Action::Value {
            path: "/tmp/vibeconsole-value".into(),
            min: 0,
            max: 100,
            decimals: 2,
        };
        let mut motion =
            Motion::new(&[dial(cc(71, 1), choice), dial(cc(72, -1), value.clone())]).unwrap();
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);
        // Nothing before the first movement.
        assert!(motion.settle(ms(10_000)).is_empty());
        // Sweep 0 → 127 → 10 without resting: nothing sent.
        for (i, v) in [0, 40, 80, 127, 90, 10].into_iter().enumerate() {
            assert!(motion.dial([0xb0, 71, v], ms(i as u64 * 100)).is_empty());
            assert!(motion.settle(ms(i as u64 * 100 + 50)).is_empty());
        }
        // Rest in zone 0 (re-entered at 500 ms): sent once at 800 ms, not at 799.
        assert!(motion.settle(ms(799)).is_empty());
        assert_eq!(
            motion.settle(ms(800)),
            [Dialed::Send(prompt("sonnet"), 0, 2)]
        );
        assert!(motion.settle(ms(5000)).is_empty());
        // Wiggle inside zone 0, and a brief visit to zone 1 that returns: nothing.
        motion.dial([0xb0, 71, 20], ms(6000));
        motion.dial([0xb0, 71, 100], ms(6100));
        motion.dial([0xb0, 71, 30], ms(6200));
        assert!(motion.settle(ms(9000)).is_empty());
        // Rest in zone 1.
        motion.dial([0xb0, 71, 100], ms(10_000));
        assert_eq!(
            motion.settle(ms(10_300)),
            [Dialed::Send(prompt("opus"), 1, 2)]
        );
        // Value knob learned as decreasing: raw 127 is the low end. Unchanged values write once.
        let write = |text: &str| Dialed::Write("/tmp/vibeconsole-value".into(), text.into());
        assert_eq!(motion.dial([0xb0, 72, 127], t), [write("0.00\n")]);
        assert!(motion.dial([0xb0, 72, 127], t).is_empty());
        assert_eq!(motion.dial([0xb0, 72, 0], t), [write("1.00\n")]);
        assert!(motion.dial([0xb0, 73, 0], t).is_empty()); // another CC
        // Atomic write: the file holds the new text and no staging file is left.
        let dir = std::env::temp_dir().join(format!("vibeconsole-value-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("temperature");
        write_atomic(&file, "0.42\n").unwrap();
        write_atomic(&file, "0.43\n").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "0.43\n");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert!(write_atomic(&dir.join("missing/x"), "1\n").is_err());
        std::fs::remove_dir_all(dir).unwrap();
        // Encoding and validation.
        assert_eq!(Action::parse(&value.encode()).unwrap(), value);
        let choice = Action::Choice(vec![prompt("a"), prompt("b")]);
        assert_eq!(Action::parse(&choice.encode()).unwrap(), choice);
        assert!(Action::Choice(vec![prompt("a")]).validate().is_err());
        assert!(Action::Choice(vec![prompt("a"); 17]).validate().is_err());
        assert!(
            Action::Choice(vec![prompt("a"), Action::OutputMute])
                .validate()
                .is_err()
        );
        for (path, min, max, decimals) in [
            ("relative", 0, 1, 2),
            ("/tmp/x", 1, 1, 2),
            ("/tmp/x", 0, 1, 7),
            ("/tmp/", 0, 1, 2),
        ] {
            let bad = Action::Value {
                path: path.into(),
                min,
                max,
                decimals,
            };
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        // Only absolute knobs take dial actions.
        let pad = Control::from_note(10, 36).unwrap();
        assert!(Mapping::new_action(pad, Input::Pad, value).is_err());
    }
}
