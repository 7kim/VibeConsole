use crate::{
    Control, Event, Message, ReleaseType, Result,
    mappings::{Input, Mapping},
};

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
}

impl Motion {
    pub fn new(mappings: &[Mapping]) -> Result<Self> {
        let mut states = Vec::new();
        for mapping in mappings {
            mapping.validate()?;
            if matches!(mapping.input, Input::Knob { .. } | Input::Joystick { .. }) {
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
        Ok(Self { states })
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
}
