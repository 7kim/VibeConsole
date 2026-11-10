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

pub fn control(button: gilrs::Button) -> Option<Control> {
    let id = BUTTONS.iter().position(|(b, _)| *b == button)?;
    Some(Control::button(u8::try_from(id).ok()?))
}
pub fn button_name(id: u8) -> &'static str {
    BUTTONS
        .get(usize::from(id))
        .map_or("unknown", |(_, name)| name)
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

pub struct Pad {
    gilrs: gilrs::Gilrs,
    /// Buttons whose press was seen since the last transition.
    pub down: HashSet<Control>,
}
impl Pad {
    pub fn open() -> crate::Result<Self> {
        let gilrs =
            gilrs::Gilrs::new().map_err(|e| format!("Xbox controllers unavailable: {e}"))?;
        Ok(Self {
            gilrs,
            down: HashSet::new(),
        })
    }
    pub fn poll(&mut self) -> Vec<Change> {
        let mut changes = Vec::new();
        while let Some(event) = self.gilrs.next_event() {
            let kind = match event.event {
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
