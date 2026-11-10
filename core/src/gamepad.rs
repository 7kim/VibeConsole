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
/// LB is the layer key (v7 ticket 09): held, every other button uses layer 2.
pub fn is_layer_key(control: Control) -> bool {
    Some(control) == self::control(gilrs::Button::LeftTrigger)
}
/// The Xbox button (v7 ticket 13): held with the D-pad it cycles profiles; held alone
/// for a second it toggles the lock. Reserved like LB.
pub fn is_guide(control: Control) -> bool {
    Some(control) == self::control(gilrs::Button::Mode)
}
/// Buttons VibeConsole keeps for itself; they can't be mapped.
pub fn is_reserved(control: Control) -> bool {
    is_layer_key(control) || is_guide(control)
}
#[derive(Debug, PartialEq)]
pub enum GuideAction {
    Cycle { forward: bool },
    ToggleLock,
}
/// The Xbox-button gesture: D-pad presses while it's held cycle (right/down forward,
/// left/up back) and are swallowed; holding it alone for one second toggles the lock,
/// once. Letting go early, a pause or a clear cancels.
#[derive(Default)]
pub struct Guide {
    since: Option<std::time::Instant>,
    used: bool,
}
impl Guide {
    /// The action for `event`, and whether the event is the gesture's (not for mappings).
    pub fn event(&mut self, event: &Event, now: std::time::Instant) -> (Option<GuideAction>, bool) {
        let dpad = |c: &Control, buttons: [gilrs::Button; 2]| {
            buttons.iter().any(|b| control(*b) == Some(*c))
        };
        match event {
            Event::Press(c) if is_guide(*c) => {
                self.since = Some(now);
                self.used = false;
                (None, true)
            }
            Event::Release(c, _) | Event::UnmatchedRelease(c) | Event::DuplicatePress(c)
                if is_guide(*c) =>
            {
                self.since = None;
                (None, true)
            }
            Event::Press(c) if self.since.is_some() => {
                let forward = dpad(c, [gilrs::Button::DPadRight, gilrs::Button::DPadDown]);
                if forward || dpad(c, [gilrs::Button::DPadLeft, gilrs::Button::DPadUp]) {
                    self.used = true;
                    (Some(GuideAction::Cycle { forward }), true)
                } else {
                    (None, false)
                }
            }
            _ => (None, false),
        }
    }
    pub fn tick(&mut self, now: std::time::Instant) -> Option<GuideAction> {
        let since = self.since?;
        if self.used || now.duration_since(since) < std::time::Duration::from_secs(1) {
            return None;
        }
        self.used = true;
        Some(GuideAction::ToggleLock)
    }
    pub fn cancel(&mut self) {
        self.since = None;
    }
}

/// The mappings for a layer: layer 1 is the layout (defaults plus saved), layer 2 only
/// what was saved for it.
pub fn layer(saved: &[Mapping], second: bool) -> Vec<Mapping> {
    if second {
        saved
            .iter()
            .filter(|m| m.context == crate::mappings::GAMEPAD_LAYER_2)
            .cloned()
            .collect()
    } else {
        layout(saved)
    }
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
            // The mouse stick moves the pointer; its directions don't fire.
            let mouse = match tuning.mouse {
                crate::mappings::MouseStick::Left => 0,
                crate::mappings::MouseStick::Right => 4,
                crate::mappings::MouseStick::Off => u8::MAX,
            };
            if offset == mouse {
                return changes;
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

/// v7 ticket 11: rumble when a source (agent or folder) newly finishes or needs the user;
/// staying in that state, or going busy/idle, doesn't rumble again.
pub fn rumble_due(before: &[(String, &str)], now: &[(String, &str)]) -> bool {
    let alerting = |state: &str| matches!(state, "finished" | "needs-input" | "needs-decision");
    now.iter().any(|(name, state)| {
        alerting(state)
            && before
                .iter()
                .find(|(n, _)| n == name)
                .is_none_or(|(_, was)| was != state)
    })
}

/// v7 ticket 22: how far the pointer moves for one step of `dt` seconds with the mouse
/// stick at `stick` (-1–1, gilrs: up is positive). Inside the dead zone nothing moves; past
/// it, speed scales from 0 to `mouse_speed` × 40 px/s at full push. `carry` keeps the
/// fractions, so slow moves aren't lost.
pub fn pointer_step(
    stick: (f32, f32),
    tuning: &crate::mappings::Tuning,
    dt: f32,
    carry: &mut (f32, f32),
) -> (i32, i32) {
    let dead = f32::from(tuning.dead_zone) / 100.0;
    let push = |v: f32| {
        if v.abs() <= dead {
            0.0
        } else {
            v.signum() * (v.abs() - dead) / (1.0 - dead).max(0.01)
        }
    };
    let speed = f32::from(tuning.mouse_speed) * 40.0 * dt;
    carry.0 += push(stick.0) * speed;
    carry.1 -= push(stick.1) * speed; // screen y grows downward
    let step = (carry.0.trunc() as i32, carry.1.trunc() as i32);
    carry.0 -= step.0 as f32;
    carry.1 -= step.1 as f32;
    step
}
/// The virtual pointer the mouse stick moves (created only when Tuning turns it on).
pub struct Pointer(evdev::uinput::VirtualDevice);
impl Pointer {
    pub fn new() -> crate::Result<Self> {
        use evdev::{AttributeSet, KeyCode, RelativeAxisCode};
        let axes = AttributeSet::from_iter([RelativeAxisCode::REL_X, RelativeAxisCode::REL_Y]);
        // A button makes desktops treat it as a mouse.
        let buttons = AttributeSet::from_iter([KeyCode::BTN_LEFT]);
        let device = evdev::uinput::VirtualDevice::builder()
            .and_then(|b| {
                b.name("VibeConsole pointer")
                    .with_relative_axes(&axes)?
                    .with_keys(&buttons)?
                    .build()
            })
            .map_err(|e| format!("cannot create the pointer through /dev/uinput: {e}"))?;
        Ok(Self(device))
    }
    pub fn move_by(&mut self, (dx, dy): (i32, i32)) -> crate::Result<()> {
        use evdev::{EventType, InputEvent, RelativeAxisCode};
        if (dx, dy) == (0, 0) {
            return Ok(());
        }
        self.0.emit(&[
            InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_X.0, dx),
            InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_Y.0, dy),
        ])?;
        Ok(())
    }
}

pub struct Pad {
    gilrs: gilrs::Gilrs,
    /// The rumble playing now; dropping it stops it.
    effect: Option<gilrs::ff::Effect>,
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
            effect: None,
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
    /// One short rumble on every controller that supports it, when Tuning has it on.
    /// Rumble is hardware feedback: it plays only while VibeConsole owns the controller.
    pub fn rumble(&mut self) -> crate::Result<()> {
        use gilrs::ff::{BaseEffect, BaseEffectType, EffectBuilder, Repeat, Replay, Ticks};
        if !self.tuning.rumble {
            return Ok(());
        }
        let ids = self
            .gilrs
            .gamepads()
            .filter(|(_, pad)| pad.is_ff_supported())
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(());
        }
        let length = Ticks::from_ms(300);
        let effect = EffectBuilder::new()
            .add_effect(BaseEffect {
                kind: BaseEffectType::Strong { magnitude: 40_000 },
                scheduling: Replay {
                    play_for: length,
                    ..Default::default()
                },
                envelope: Default::default(),
            })
            .repeat(Repeat::For(length))
            .gamepads(&ids)
            .finish(&mut self.gilrs)
            .map_err(|e| format!("Rumble failed: {e}"))?;
        effect.play().map_err(|e| format!("Rumble failed: {e}"))?;
        self.stop_rumble();
        self.effect = Some(effect);
        Ok(())
    }
    pub fn stop_rumble(&mut self) {
        if let Some(effect) = self.effect.take() {
            let _ = effect.stop();
        }
    }
    pub fn connected(&self) -> Vec<String> {
        self.gilrs
            .gamepads()
            .map(|(_, pad)| pad.name().to_owned())
            .collect()
    }
}

impl Drop for Pad {
    /// Quitting (or any error that drops the session) stops a rumble in progress.
    fn drop(&mut self) {
        self.stop_rumble();
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

    /// v7 ticket 09: holding LB switches to layer 2. The switch is a transition: a button
    /// held across it is released, doesn't fire on release, and fires its layer-2 action on
    /// its next press. LB itself is the layer key, never a mapping.
    #[test]
    fn holding_lb_switches_layers_as_a_transition() {
        use evdev::KeyCode;
        use std::cell::RefCell;
        let a = control(gilrs::Button::South).unwrap();
        assert!(is_layer_key(control(gilrs::Button::LeftTrigger).unwrap()));
        assert!(layer(&[], false).iter().all(|m| !is_layer_key(m.control)));
        let mut second = Mapping::new(a, "F7", "hold").unwrap();
        second.context = crate::mappings::GAMEPAD_LAYER_2;
        let saved = [second];
        assert_eq!(layer(&saved, true).len(), 1, "layer 2 has no defaults");
        let sent = RefCell::new(Vec::new());
        let mut emit = |key, down| {
            sent.borrow_mut().push((key, down));
            Ok(())
        };
        let mut keyboard = crate::keyboard::Keyboard::new(&layer(&saved, false)).unwrap();
        keyboard.resume();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        // LB pressed: what the session does on a layer change.
        keyboard.pause(&mut emit).unwrap();
        keyboard.replace(&layer(&saved, true)).unwrap();
        keyboard.resume();
        assert_eq!(
            *sent.borrow(),
            [(KeyCode::KEY_ENTER, true), (KeyCode::KEY_ENTER, false)]
        );
        sent.borrow_mut().clear();
        keyboard
            .observe(Event::Release(a, ReleaseType::ButtonUp), &mut emit)
            .unwrap();
        assert!(sent.borrow().is_empty());
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        assert_eq!(*sent.borrow(), [(KeyCode::KEY_F7, true)]);
    }

    /// v7 ticket 10: Akai pads and Xbox buttons share one keyboard; interleaved presses
    /// each fire once and release their own key.
    #[test]
    fn midi_pads_and_xbox_buttons_interleave_in_one_keyboard() {
        use evdev::KeyCode;
        let pad = Control::from_note(10, 36).unwrap();
        let a = control(gilrs::Button::South).unwrap();
        let mut all = vec![Mapping::new(pad, "Shift", "hold").unwrap()];
        all.extend(layout(&[]));
        let mut keyboard = crate::keyboard::Keyboard::new(&all).unwrap();
        keyboard.resume();
        let mut sent = Vec::new();
        let mut emit = |key, down| {
            sent.push((key, down));
            Ok(())
        };
        for event in [
            Event::Press(pad),
            Event::Press(a),
            Event::Release(pad, ReleaseType::NoteOff),
            Event::Release(a, ReleaseType::ButtonUp),
        ] {
            keyboard.observe(event, &mut emit).unwrap();
        }
        assert_eq!(
            sent,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_ENTER, true),
                (KeyCode::KEY_LEFTSHIFT, false),
                (KeyCode::KEY_ENTER, false),
            ]
        );
    }

    #[test]
    fn rumble_once_when_a_source_newly_finishes_or_needs_you() {
        let state = |pairs: &[(&str, &'static str)]| {
            pairs
                .iter()
                .map(|(n, s)| (n.to_string(), *s))
                .collect::<Vec<(String, &str)>>()
        };
        let busy = state(&[("claude", "thinking"), ("/work", "thinking")]);
        let done = state(&[("claude", "finished"), ("/work", "thinking")]);
        assert!(rumble_due(&busy, &done));
        assert!(
            !rumble_due(&done, &done),
            "still finished: no second rumble"
        );
        let asks = state(&[("claude", "finished"), ("/work", "needs-input")]);
        assert!(rumble_due(&done, &asks));
        assert!(!rumble_due(&asks, &busy), "going busy again doesn't rumble");
        assert!(rumble_due(&[], &state(&[("codex", "needs-decision")])));
        assert!(!rumble_due(&[], &state(&[("codex", "idle")])));
    }

    #[test]
    fn xbox_button_with_the_dpad_cycles_and_a_one_second_hold_toggles_the_lock() {
        use std::time::{Duration, Instant};
        let at = Instant::now();
        let ms = |n| at + Duration::from_millis(n);
        let guide = control(gilrs::Button::Mode).unwrap();
        let right = control(gilrs::Button::DPadRight).unwrap();
        let up = control(gilrs::Button::DPadUp).unwrap();
        let a = control(gilrs::Button::South).unwrap();
        let mut g = Guide::default();
        assert_eq!(
            g.event(&Event::Press(right), ms(0)),
            (None, false),
            "D-pad alone is a mapping"
        );
        assert_eq!(g.event(&Event::Press(guide), ms(0)), (None, true));
        assert_eq!(
            g.event(&Event::Press(right), ms(100)),
            (Some(GuideAction::Cycle { forward: true }), true)
        );
        assert_eq!(
            g.event(&Event::Press(up), ms(200)),
            (Some(GuideAction::Cycle { forward: false }), true)
        );
        assert_eq!(g.event(&Event::Press(a), ms(250)), (None, false));
        assert_eq!(g.tick(ms(1500)), None, "used for cycling: no lock toggle");
        g.event(&Event::Release(guide, ReleaseType::ButtonUp), ms(1600));
        g.event(&Event::Press(guide), ms(2000));
        assert_eq!(g.tick(ms(2900)), None);
        assert_eq!(g.tick(ms(3000)), Some(GuideAction::ToggleLock));
        assert_eq!(g.tick(ms(3500)), None, "once per hold");
        g.event(&Event::Release(guide, ReleaseType::ButtonUp), ms(3600));
        g.event(&Event::Press(guide), ms(4000));
        g.event(&Event::Release(guide, ReleaseType::ButtonUp), ms(4500));
        assert_eq!(g.tick(ms(5500)), None, "letting go early cancels");
        g.event(&Event::Press(guide), ms(6000));
        g.cancel();
        assert_eq!(g.tick(ms(7500)), None, "a pause or clear cancels");
        assert!(is_reserved(guide) && !is_reserved(a));
    }

    /// v7 ticket 22: the mouse stick moves the pointer by its push past the dead zone, scaled
    /// by speed, keeps fractions, and its directions stop firing.
    #[test]
    fn mouse_stick_moves_by_push_past_the_dead_zone_and_keeps_fractions() {
        let tuning = crate::mappings::Tuning {
            dead_zone: 15,
            mouse_speed: 10,
            mouse: crate::mappings::MouseStick::Left,
            ..Default::default()
        };
        let mut carry = (0.0, 0.0);
        assert_eq!(
            pointer_step((0.1, -0.1), &tuning, 0.1, &mut carry),
            (0, 0),
            "drift"
        );
        // Full right push for 0.1 s at speed 10: 40 px; full up push moves up (negative y).
        assert_eq!(
            pointer_step((1.0, 1.0), &tuning, 0.1, &mut carry),
            (40, -40)
        );
        let mut carry = (0.0, 0.0);
        let mut total = 0;
        for _ in 0..10 {
            total += pointer_step((0.2, 0.0), &tuning, 0.01, &mut carry).0;
        }
        assert_eq!(total, 2, "slow moves add up instead of vanishing");
        let mut down = HashSet::new();
        let mut pad = Analog::default();
        assert!(
            analog(
                &mut down,
                &mut pad,
                Motion::Axis(gilrs::Axis::LeftStickY, 0.9),
                &tuning
            )
            .is_empty()
        );
        assert_eq!(
            pad.left.1, 0.9,
            "the position is still kept for the pointer"
        );
        assert!(
            !analog(
                &mut down,
                &mut pad,
                Motion::Axis(gilrs::Axis::RightStickY, 0.9),
                &tuning
            )
            .is_empty()
        );
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
