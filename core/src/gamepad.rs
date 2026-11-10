//! Xbox controllers over USB through gilrs (v7 ticket 07). Buttons become `Control`s with
//! `Message::Button`, so the keyboard state machine, transitions and /configure treat them
//! like pads. Their mappings live in `GAMEPAD_CONTEXT`, active alongside any MIDI context.
use crate::mappings::{GAMEPAD_CONTEXT, Mapping};
use crate::{Control, Event, ReleaseType};
use std::collections::HashSet;

/// Every mappable button, in id order. gilrs calls the bumpers `LeftTrigger`/`RightTrigger`
/// and the triggers `LeftTrigger2`/`RightTrigger2`; `Mode` is the Xbox button.
/// ponytail: no Share button; gilrs 0.11 has no portable name for it.
pub const BUTTONS: [(gilrs::Button, &str); 17] = [
    (gilrs::Button::South, "A"),
    (gilrs::Button::East, "B"),
    (gilrs::Button::West, "X"),
    (gilrs::Button::North, "Y"),
    (gilrs::Button::LeftTrigger, "LB"),
    (gilrs::Button::RightTrigger, "RB"),
    (gilrs::Button::LeftTrigger2, "LT"),
    (gilrs::Button::RightTrigger2, "RT"),
    (gilrs::Button::Select, "View"),
    (gilrs::Button::Start, "Menu"),
    (gilrs::Button::Mode, "Xbox"),
    (gilrs::Button::LeftThumb, "Left stick press"),
    (gilrs::Button::RightThumb, "Right stick press"),
    (gilrs::Button::DPadUp, "D-pad up"),
    (gilrs::Button::DPadDown, "D-pad down"),
    (gilrs::Button::DPadLeft, "D-pad left"),
    (gilrs::Button::DPadRight, "D-pad right"),
];

/// Stick directions (v7 ticket 08) follow the buttons: left up/down/left/right, then
/// right up/down/left/right.
pub const STICK_BASE: u8 = 17;
const STICKS: [&str; 8] = [
    "Left stick up",
    "Left stick down",
    "Left stick left",
    "Left stick right",
    "Right stick up",
    "Right stick down",
    "Right stick left",
    "Right stick right",
];
/// Every control an Xbox controller has: buttons, then stick directions.
pub const CONTROLS: usize = BUTTONS.len() + STICKS.len();

pub fn control(button: gilrs::Button) -> Option<Control> {
    let id = BUTTONS.iter().position(|(b, _)| *b == button)?;
    Some(Control::button(u8::try_from(id).ok()?))
}
pub fn button_name(id: u8) -> &'static str {
    let id = usize::from(id);
    BUTTONS
        .get(id)
        .map(|(_, name)| *name)
        .or_else(|| STICKS.get(id.wrapping_sub(BUTTONS.len())).copied())
        .unwrap_or("unknown")
}

/// The default layout, so a new controller is useful before anything is configured.
pub fn default_layout() -> Vec<Mapping> {
    [
        (gilrs::Button::South, "Enter"),
        (gilrs::Button::East, "Escape"),
        (gilrs::Button::West, "Backspace"),
        (gilrs::Button::North, "Tab"),
        (gilrs::Button::LeftTrigger, "Ctrl+Shift+Tab"),
        (gilrs::Button::RightTrigger, "Ctrl+Tab"),
        (gilrs::Button::DPadUp, "Up"),
        (gilrs::Button::DPadDown, "Down"),
        (gilrs::Button::DPadLeft, "Left"),
        (gilrs::Button::DPadRight, "Right"),
    ]
    .into_iter()
    .map(|(button, keys)| {
        let control = control(button).expect("listed button");
        let mut mapping = Mapping::new(control, keys, "hold").expect("valid default");
        mapping.context = GAMEPAD_CONTEXT;
        mapping.label = control.label();
        mapping
    })
    .collect()
}
/// The layout in use: each saved Xbox mapping replaces that button's default.
/// ponytail: a default can be replaced but not removed; add "unassigned" if asked.
pub fn layout(saved: &[Mapping]) -> Vec<Mapping> {
    let saved = saved
        .iter()
        .filter(|m| m.context == GAMEPAD_CONTEXT)
        .cloned()
        .collect::<Vec<_>>();
    let mut layout = default_layout()
        .into_iter()
        .filter(|d| !saved.iter().any(|m| m.control == d.control))
        .collect::<Vec<_>>();
    layout.extend(saved);
    layout
}

/// What happened on a controller, already decided against the buttons held.
#[derive(Debug, PartialEq)]
pub enum Change {
    Event(Event),
    Connected(String),
    Disconnected(String),
}
/// gilrs events reduced to what VibeConsole uses.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Pressed(gilrs::Button),
    Released(gilrs::Button),
    Connected,
    Disconnected,
}
/// A press is new only once; a release counts only for a press that was seen, so a button
/// held through a transition never fires on its release.
pub fn change(down: &mut HashSet<Control>, kind: Kind, name: &str) -> Option<Change> {
    Some(match kind {
        Kind::Pressed(button) => {
            let control = control(button)?;
            Change::Event(if down.insert(control) {
                Event::Press(control)
            } else {
                Event::DuplicatePress(control)
            })
        }
        Kind::Released(button) => {
            let control = control(button)?;
            Change::Event(if down.remove(&control) {
                Event::Release(control, ReleaseType::ButtonUp)
            } else {
                Event::UnmatchedRelease(control)
            })
        }
        Kind::Connected => Change::Connected(name.to_owned()),
        Kind::Disconnected => {
            down.clear();
            Change::Disconnected(name.to_owned())
        }
    })
}

/// Where the triggers (0–1) and sticks (-1–1, x then y) are now, for the hints window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Analog {
    pub lt: f32,
    pub rt: f32,
    pub left: (f32, f32),
    pub right: (f32, f32),
}
/// Analog movement reduced to what VibeConsole uses.
#[derive(Clone, Copy, Debug)]
pub enum Motion {
    Trigger(gilrs::Button, f32),
    Axis(gilrs::Axis, f32),
}
/// A trigger presses at the tuned fire point and releases 5% under it, so resting a
/// finger on it doesn't chatter. A stick direction presses at half travel (or the dead
/// zone, if larger) and releases once back inside the dead zone; drift never fires.
pub fn analog(
    down: &mut HashSet<Control>,
    analog: &mut Analog,
    motion: Motion,
    tuning: &crate::mappings::Tuning,
) -> Vec<Change> {
    let mut changes = Vec::new();
    fn set(down: &mut HashSet<Control>, control: Control, on: bool, changes: &mut Vec<Change>) {
        if on && down.insert(control) {
            changes.push(Change::Event(Event::Press(control)));
        } else if !on && down.remove(&control) {
            changes.push(Change::Event(Event::Release(
                control,
                ReleaseType::ButtonUp,
            )));
        }
    }
    match motion {
        Motion::Trigger(button, value) => {
            let Some(control) = control(button) else {
                return changes;
            };
            match button {
                gilrs::Button::LeftTrigger2 => analog.lt = value,
                gilrs::Button::RightTrigger2 => analog.rt = value,
                _ => return changes,
            }
            let percent = value * 100.0;
            let fire = f32::from(tuning.trigger);
            let held = down.contains(&control);
            let on = if held {
                percent >= fire - 5.0
            } else {
                percent >= fire
            };
            set(down, control, on, &mut changes);
        }
        Motion::Axis(axis, value) => {
            let (stick, offset, y) = match axis {
                gilrs::Axis::LeftStickX => (&mut analog.left, 0, false),
                gilrs::Axis::LeftStickY => (&mut analog.left, 0, true),
                gilrs::Axis::RightStickX => (&mut analog.right, 4, false),
                gilrs::Axis::RightStickY => (&mut analog.right, 4, true),
                _ => return changes,
            };
            if y {
                stick.1 = value;
            } else {
                stick.0 = value;
            }
            // Up/down for y (gilrs: up is positive), right/left for x.
            let (positive, negative) = if y { (0, 1) } else { (3, 2) };
            let at = |n: u8| Control::button(STICK_BASE + offset + n);
            let percent = value.abs() * 100.0;
            let dead = f32::from(tuning.dead_zone);
            let press = dead.max(50.0);
            // Releases first, so flipping sides never holds both directions.
            for (n, sign) in [(positive, 1.0), (negative, -1.0)] {
                let control = at(n);
                let toward = value * sign > 0.0;
                if !(toward && percent > dead && down.contains(&control)) {
                    set(down, control, false, &mut changes);
                }
            }
            for (n, sign) in [(positive, 1.0), (negative, -1.0)] {
                if value * sign > 0.0 && percent >= press {
                    set(down, at(n), true, &mut changes);
                }
            }
        }
    }
    changes
}

pub struct Pad {
    gilrs: gilrs::Gilrs,
    /// Buttons whose press was seen since the last transition.
    pub down: HashSet<Control>,
    pub analog: Analog,
    pub tuning: crate::mappings::Tuning,
}
impl Pad {
    pub fn open() -> crate::Result<Self> {
        let gilrs =
            gilrs::Gilrs::new().map_err(|e| format!("Xbox controllers unavailable: {e}"))?;
        Ok(Self {
            gilrs,
            down: HashSet::new(),
            analog: Analog::default(),
            tuning: crate::mappings::Tuning::default(),
        })
    }
    pub fn poll(&mut self) -> Vec<Change> {
        let mut changes = Vec::new();
        let triggers = [gilrs::Button::LeftTrigger2, gilrs::Button::RightTrigger2];
        while let Some(event) = self.gilrs.next_event() {
            let motion = match event.event {
                gilrs::EventType::ButtonChanged(button, value, _) if triggers.contains(&button) => {
                    Some(Motion::Trigger(button, value))
                }
                gilrs::EventType::AxisChanged(axis, value, _) => Some(Motion::Axis(axis, value)),
                _ => None,
            };
            if let Some(motion) = motion {
                let tuning = self.tuning;
                changes.extend(analog(&mut self.down, &mut self.analog, motion, &tuning));
                continue;
            }
            let kind = match event.event {
                // Triggers fire from their analog value, at the tuned point.
                gilrs::EventType::ButtonPressed(button, _)
                | gilrs::EventType::ButtonReleased(button, _)
                    if triggers.contains(&button) =>
                {
                    continue;
                }
                gilrs::EventType::ButtonPressed(button, _) => Kind::Pressed(button),
                gilrs::EventType::ButtonReleased(button, _) => Kind::Released(button),
                gilrs::EventType::Connected => Kind::Connected,
                gilrs::EventType::Disconnected => Kind::Disconnected,
                _ => continue,
            };
            let name = self.gilrs.gamepad(event.id).name().to_owned();
            changes.extend(change(&mut self.down, kind, &name));
        }
        changes
    }
    /// A transition: queued events are dropped and held buttons forgotten, so the next
    /// fresh press fires and nothing replays.
    pub fn drain(&mut self) {
        while self.gilrs.next_event().is_some() {}
        self.down.clear();
        self.analog = Analog::default();
    }
    pub fn connected(&self) -> Vec<String> {
        self.gilrs
            .gamepads()
            .map(|(_, pad)| pad.name().to_owned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_press_once_release_only_when_seen_and_unplug_forgets_held_buttons() {
        let mut down = HashSet::new();
        let a = control(gilrs::Button::South).unwrap();
        assert_eq!(a.label(), "Xbox A");
        assert_eq!(
            change(&mut down, Kind::Pressed(gilrs::Button::South), "pad"),
            Some(Change::Event(Event::Press(a)))
        );
        assert_eq!(
            change(&mut down, Kind::Pressed(gilrs::Button::South), "pad"),
            Some(Change::Event(Event::DuplicatePress(a)))
        );
        assert_eq!(
            change(&mut down, Kind::Disconnected, "Xbox Series"),
            Some(Change::Disconnected("Xbox Series".into()))
        );
        // Held through the unplug: its release is ignored, the next press fires.
        assert_eq!(
            change(&mut down, Kind::Released(gilrs::Button::South), "pad"),
            Some(Change::Event(Event::UnmatchedRelease(a)))
        );
        assert_eq!(
            change(&mut down, Kind::Pressed(gilrs::Button::South), "pad"),
            Some(Change::Event(Event::Press(a)))
        );
        assert_eq!(
            change(&mut down, Kind::Released(gilrs::Button::South), "pad"),
            Some(Change::Event(Event::Release(a, ReleaseType::ButtonUp)))
        );
        assert_eq!(
            change(&mut down, Kind::Pressed(gilrs::Button::C), "pad"),
            None
        );
    }

    /// A held button drives its key; a transition (unplug, replug, layer or profile change)
    /// releases it, and the button fires again only on its next fresh press.
    #[test]
    fn xbox_buttons_drive_keys_and_a_transition_releases_them_without_replay() {
        use evdev::KeyCode;
        use std::cell::RefCell;
        let mut keyboard = crate::keyboard::Keyboard::new(&layout(&[])).unwrap();
        let sent = RefCell::new(Vec::new());
        let mut emit = |key, down| {
            sent.borrow_mut().push((key, down));
            Ok(())
        };
        let mut down = HashSet::new();
        let mut step =
            |kind, down: &mut HashSet<Control>, keyboard: &mut crate::keyboard::Keyboard| {
                let Some(Change::Event(event)) = change(down, kind, "pad") else {
                    panic!("no event");
                };
                keyboard.observe(event, &mut emit).unwrap();
            };
        let (press, release) = (
            Kind::Pressed(gilrs::Button::South),
            Kind::Released(gilrs::Button::South),
        );
        step(press, &mut down, &mut keyboard);
        step(release, &mut down, &mut keyboard);
        assert_eq!(
            *sent.borrow(),
            [(KeyCode::KEY_ENTER, true), (KeyCode::KEY_ENTER, false)]
        );
        sent.borrow_mut().clear();
        step(press, &mut down, &mut keyboard);
        // The transition: keys released, then the pad forgets what it saw (Pad::drain).
        keyboard
            .clear(&mut |key, down| {
                sent.borrow_mut().push((key, down));
                Ok(())
            })
            .unwrap();
        down.clear();
        assert_eq!(
            *sent.borrow(),
            [(KeyCode::KEY_ENTER, true), (KeyCode::KEY_ENTER, false)]
        );
        sent.borrow_mut().clear();
        step(release, &mut down, &mut keyboard);
        assert!(
            sent.borrow().is_empty(),
            "a release from before the transition sends nothing"
        );
        step(press, &mut down, &mut keyboard);
        assert_eq!(*sent.borrow(), [(KeyCode::KEY_ENTER, true)]);
    }

    /// v7 ticket 08: a trigger fires once at the tuned fire point and re-arms only after it
    /// eases off; a stick ignores drift inside the dead zone and reports its direction.
    #[test]
    fn triggers_fire_at_the_tuned_point_and_sticks_ignore_their_dead_zone() {
        let tuning = crate::mappings::Tuning {
            dead_zone: 15,
            trigger: 30,
            ..Default::default()
        };
        let mut pad = Analog::default();
        let mut down = HashSet::new();
        let lt = control(gilrs::Button::LeftTrigger2).unwrap();
        let mut feed = |kind| analog(&mut down, &mut pad, kind, &tuning);
        assert!(feed(Motion::Trigger(gilrs::Button::LeftTrigger2, 0.2)).is_empty());
        assert_eq!(
            feed(Motion::Trigger(gilrs::Button::LeftTrigger2, 0.35)),
            [Change::Event(Event::Press(lt))]
        );
        assert!(feed(Motion::Trigger(gilrs::Button::LeftTrigger2, 0.9)).is_empty());
        // Easing just under the fire point doesn't release (5% hysteresis) ...
        assert!(feed(Motion::Trigger(gilrs::Button::LeftTrigger2, 0.27)).is_empty());
        // ... well under it does, and re-arms.
        assert_eq!(
            feed(Motion::Trigger(gilrs::Button::LeftTrigger2, 0.1)),
            [Change::Event(Event::Release(lt, ReleaseType::ButtonUp))]
        );
        assert_eq!(
            feed(Motion::Trigger(gilrs::Button::LeftTrigger2, 0.31)),
            [Change::Event(Event::Press(lt))]
        );
        let up = Control::button(STICK_BASE); // Left stick up
        let down_dir = Control::button(STICK_BASE + 1);
        assert_eq!(up.label(), "Xbox Left stick up");
        assert!(feed(Motion::Axis(gilrs::Axis::LeftStickY, 0.1)).is_empty());
        // A push fires at half travel (or the dead zone, if larger).
        assert!(feed(Motion::Axis(gilrs::Axis::LeftStickY, 0.4)).is_empty());
        assert_eq!(
            feed(Motion::Axis(gilrs::Axis::LeftStickY, 0.6)),
            [Change::Event(Event::Press(up))]
        );
        // Flipping straight to the other side releases first.
        assert_eq!(
            feed(Motion::Axis(gilrs::Axis::LeftStickY, -0.7)),
            [
                Change::Event(Event::Release(up, ReleaseType::ButtonUp)),
                Change::Event(Event::Press(down_dir))
            ]
        );
        assert_eq!(
            feed(Motion::Axis(gilrs::Axis::LeftStickY, -0.05)),
            [Change::Event(Event::Release(
                down_dir,
                ReleaseType::ButtonUp
            ))]
        );
        assert_eq!(pad.left, (0.0, -0.05));
        assert_eq!(pad.lt, 0.31);
    }

    #[test]
    fn default_layout_applies_until_a_button_is_mapped() {
        let defaults = layout(&[]);
        let a = control(gilrs::Button::South).unwrap();
        let enter = defaults.iter().find(|m| m.control == a).unwrap();
        assert_eq!(enter.keys, ["Enter"]);
        for m in &defaults {
            assert_eq!(m.context, GAMEPAD_CONTEXT);
            m.validate().unwrap_or_else(|e| panic!("{}: {e}", m.label));
        }
        let mut mine = Mapping::new(a, "F5", "hold").unwrap();
        mine.context = GAMEPAD_CONTEXT;
        let mixed = layout(&[mine.clone()]);
        assert_eq!(mixed.len(), defaults.len());
        assert_eq!(mixed.iter().find(|m| m.control == a).unwrap().keys, ["F5"]);
    }
}
