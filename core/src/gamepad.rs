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
/// v7 ticket 75: a stick in angle mode fires one control per range: the left stick's from
/// here, the right stick's [`MAX_RANGES`] later.
pub const ANGLE_BASE: u8 = STICK_BASE + 8;
const MAX_RANGES: u8 = crate::mappings::MAX_RANGES as u8;
/// Every control an Xbox controller has: buttons, stick directions, then angle ranges.
pub const CONTROLS: usize = ANGLE_BASE as usize + 2 * MAX_RANGES as usize;

pub fn control(button: gilrs::Button) -> Option<Control> {
    let id = BUTTONS.iter().position(|(b, _)| *b == button)?;
    Some(Control::button(u8::try_from(id).ok()?))
}
pub fn button_name(id: u8) -> String {
    if let Some(range) = id.checked_sub(ANGLE_BASE).filter(|r| *r < 2 * MAX_RANGES) {
        let side = if range < MAX_RANGES { "Left" } else { "Right" };
        return format!("{side} stick angle range {}", range % MAX_RANGES + 1);
    }
    let id = usize::from(id);
    BUTTONS
        .get(id)
        .map(|(_, name)| *name)
        .or_else(|| STICKS.get(id.wrapping_sub(BUTTONS.len())).copied())
        .unwrap_or("unknown")
        .into()
}
/// Which stick (0 left, 1 right) a direction or angle range belongs to.
fn stick_of(control: Control) -> Option<usize> {
    match control.id {
        id if (STICK_BASE..ANGLE_BASE).contains(&id) => Some(usize::from((id - STICK_BASE) / 4)),
        id if usize::from(id) < CONTROLS && id >= ANGLE_BASE => {
            Some(usize::from((id - ANGLE_BASE) / MAX_RANGES))
        }
        _ => None,
    }
}
/// A stick's angle in degrees: 0 = up, clockwise (gilrs: up is positive y).
pub fn angle_of(x: f32, y: f32) -> f32 {
    (x.atan2(y).to_degrees() + 360.0) % 360.0
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

/// After a transition: buttons are forgotten (their release is then unmatched), but a stick
/// or trigger still held past its fire point stays down, so axis jitter can't fire it
/// again; it fires on its next fresh push after it's let go.
pub fn keep_held_analog(down: &mut HashSet<Control>) {
    let lt = control(gilrs::Button::LeftTrigger2);
    let rt = control(gilrs::Button::RightTrigger2);
    down.retain(|c| Some(*c) == lt || Some(*c) == rt || c.id >= STICK_BASE);
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
    /// v7 ticket 75: after a stick's mode changes it fires nothing until it's let go.
    pub wait: [bool; 2],
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
            let (side, y) = match axis {
                gilrs::Axis::LeftStickX => (0, false),
                gilrs::Axis::LeftStickY => (0, true),
                gilrs::Axis::RightStickX => (1, false),
                gilrs::Axis::RightStickY => (1, true),
                _ => return changes,
            };
            let stick = if side == 0 {
                &mut analog.left
            } else {
                &mut analog.right
            };
            if y {
                stick.1 = value;
            } else {
                stick.0 = value;
            }
            let (sx, sy) = *stick;
            // The mouse stick moves the pointer; its directions don't fire.
            let mouse = match tuning.mouse {
                crate::mappings::MouseStick::Left => Some(0),
                crate::mappings::MouseStick::Right => Some(1),
                crate::mappings::MouseStick::Off => None,
            };
            if mouse == Some(side) {
                return changes;
            }
            let dead = f32::from(tuning.dead_zone);
            let press = dead.max(50.0);
            let push = sx.hypot(sy).min(1.0) * 100.0;
            if analog.wait[side] {
                analog.wait[side] = push > dead;
                return changes;
            }
            // Angle mode: the range the stick points into is held; moving to the next range
            // releases the first before pressing the next. Inside the dead zone, none.
            if let mode @ crate::mappings::StickMode::Angle(_) = &tuning.sticks[side] {
                let base = ANGLE_BASE + side as u8 * MAX_RANGES;
                let held = (base..base + MAX_RANGES)
                    .map(Control::button)
                    .find(|c| down.contains(c));
                let target = (push > dead)
                    .then(|| mode.range_at(angle_of(sx, sy)))
                    .flatten()
                    .map(|range| Control::button(base + range as u8));
                if held != target {
                    if let Some(held) = held {
                        set(down, held, false, &mut changes);
                    }
                    if let Some(target) = target.filter(|_| push >= press || held.is_some()) {
                        set(down, target, true, &mut changes);
                    }
                }
                return changes;
            }
            let offset = side as u8 * 4;
            // Up/down for y (gilrs: up is positive), right/left for x.
            let (positive, negative) = if y { (0, 1) } else { (3, 2) };
            let at = |n: u8| Control::button(STICK_BASE + offset + n);
            let percent = value.abs() * 100.0;
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

/// v7 ticket 75: a stick whose mode changes forgets its held directions and ranges and, if
/// pushed now, fires nothing until it's let go. True when any stick's mode changed.
pub fn retune(
    down: &mut HashSet<Control>,
    analog: &mut Analog,
    old: &crate::mappings::Tuning,
    new: &crate::mappings::Tuning,
) -> bool {
    let changed = [0, 1].map(|s| old.sticks[s] != new.sticks[s]);
    let dead = f32::from(new.dead_zone);
    for (side, (x, y)) in [(0, analog.left), (1, analog.right)] {
        if changed[side] {
            down.retain(|c| stick_of(*c) != Some(side));
            analog.wait[side] = x.hypot(y) * 100.0 > dead;
        }
    }
    changed.contains(&true)
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

/// v7 ticket 27: whether headset audio through the controller's 3.5 mm jack can work, from
/// which driver holds the controller (`bound`: driver names with a device bound) and ALSA's
/// card list. Only `xone` carries controller audio; VibeConsole never loads or unloads a
/// kernel module, it only says what it found.
pub fn headset_status(bound: &[&str], cards: &str) -> String {
    let xone = bound.iter().any(|d| d.starts_with("xone"));
    let xpad = bound.contains(&"xpad");
    let audio = cards.lines().any(|line| {
        let line = line.to_lowercase();
        line.contains("xbox") || line.contains("xone") || line.contains("gip")
    });
    match (xone, xpad, audio) {
        (true, _, true) => "Xbox headset audio: available (xone driver, controller audio device present)".into(),
        (true, _, false) => {
            "Xbox headset audio: unavailable: xone is bound but no controller audio device is present".into()
        }
        (false, true, _) => {
            "Xbox headset audio: unavailable: needs the xone driver (the controller uses xpad)".into()
        }
        (false, false, _) => "Xbox headset audio: no Xbox controller on a USB driver".into(),
    }
}
/// The real inputs for `headset_status`: drivers in /sys with a device bound, and
/// /proc/asound/cards.
pub fn headset() -> String {
    let bound = ["xpad", "xone-wired", "xone-dongle", "xone-gip"]
        .into_iter()
        .filter(|driver| {
            std::fs::read_dir(format!("/sys/bus/usb/drivers/{driver}")).is_ok_and(|entries| {
                entries
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy().contains(':'))
            })
        })
        .collect::<Vec<_>>();
    let cards = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
    headset_status(&bound, &cards)
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

/// ponytail: one held-button set for every controller; two pads pressing the same button
/// count as one. Key events by gamepad id if two-controller setups matter.
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
                changes.extend(analog(
                    &mut self.down,
                    &mut self.analog,
                    motion,
                    &self.tuning,
                ));
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
    /// New settings; true when a stick's mode changed ([`retune`]): the caller then runs a
    /// transition.
    pub fn set_tuning(&mut self, tuning: crate::mappings::Tuning) -> bool {
        let changed = retune(&mut self.down, &mut self.analog, &self.tuning, &tuning);
        self.tuning = tuning;
        changed
    }
    /// A transition: queued events are dropped and held buttons forgotten, so the next
    /// fresh press fires and nothing replays.
    pub fn drain(&mut self) {
        while self.gilrs.next_event().is_some() {}
        keep_held_analog(&mut self.down);
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

/// What the Xbox button light shows while VibeConsole owns the controller (v7 ticket 64).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Light {
    /// Output running: steady.
    Running,
    /// Paused, or not running yet: slow blink.
    Paused,
}

/// Where the kernel lists LEDs; xone's controller lights are the `gip*` folders.
pub const LEDS: &str = "/sys/class/leds";

/// The controller's own Xbox button light (v7 ticket 64), through xone's sysfs files:
/// `mode` (GIP: 0 off, 1 on, 2 fast blink, 3 blink, 4 slow blink, 8/9 fade) and
/// `brightness`. xpad registers a light only for Xbox 360 pads (`xpad_led_probe`), so a
/// Series controller on xpad has none and it's reported unavailable. Whatever the lights
/// showed before comes back on drop: quit, error or a dropped session.
pub struct ButtonLight {
    /// Each light's folder with the mode and brightness it had at open.
    saved: Vec<(std::path::PathBuf, String, String)>,
    shown: Option<Light>,
}
impl ButtonLight {
    /// Finds the lights and checks they can be written, without writing. Err is the reason
    /// the light is unavailable, ready to show.
    pub fn open(leds: &std::path::Path) -> std::result::Result<Self, String> {
        let mut dirs = std::fs::read_dir(leds)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with("gip"))
                    .map(|e| e.path())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if dirs.is_empty() {
            return Err("Xbox button light: unavailable: no xone controller light \
                        (xpad has none for Xbox One/Series controllers)"
                .into());
        }
        dirs.sort();
        let mut saved = Vec::new();
        for dir in dirs {
            let read =
                |file: &str| std::fs::read_to_string(dir.join(file)).map(|s| s.trim().to_owned());
            // Opening for write sends nothing to the controller; only a write does.
            let writable = |file: &str| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(dir.join(file))
                    .is_ok()
            };
            match (read("mode"), read("brightness")) {
                (Ok(mode), Ok(brightness)) if writable("mode") && writable("brightness") => {
                    saved.push((dir, mode, brightness));
                }
                _ => {
                    return Err(format!(
                        "Xbox button light: unavailable: {} is not writable \
                         (a udev rule must give your user its mode and brightness)",
                        dir.display()
                    ));
                }
            }
        }
        Ok(Self { saved, shown: None })
    }
    /// After a controller comes or goes: the lights there now, keeping the values found
    /// first for lights already known (they may show VibeConsole's mode by now). None when
    /// no light is left or usable.
    pub fn reopen(old: Option<Self>, leds: &std::path::Path) -> Option<Self> {
        let mut new = Self::open(leds).ok();
        if let Some(mut old) = old {
            if let Some(new) = &mut new {
                for (dir, mode, brightness) in &mut new.saved {
                    if let Some((_, m, b)) = old.saved.iter().find(|(d, ..)| d == dir) {
                        (*mode, *brightness) = (m.clone(), b.clone());
                    }
                }
            }
            // Lights that stay are restored by `new`; unplugged ones can't be.
            old.shown = None;
        }
        new
    }
    /// The line `doctor` and Xbox tuning show: available, or why not.
    pub fn status() -> String {
        Self::open(std::path::Path::new(LEDS)).map_or_else(|why| why, |light| light.describe())
    }
    pub fn describe(&self) -> String {
        let names = self
            .saved
            .iter()
            .filter_map(|(dir, ..)| dir.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        format!("Xbox button light: available ({})", names.join(", "))
    }
    /// Sets every light's mode; writes nothing when it already shows `light`.
    pub fn show(&mut self, light: Light) -> crate::Result<()> {
        if self.shown == Some(light) {
            return Ok(());
        }
        let mode = match light {
            Light::Running => "1",
            Light::Paused => "4",
        };
        for (dir, ..) in &self.saved {
            std::fs::write(dir.join("mode"), mode)
                .map_err(|e| format!("Xbox button light: cannot set {}: {e}", dir.display()))?;
        }
        self.shown = Some(light);
        Ok(())
    }
}
impl Drop for ButtonLight {
    /// Puts back the mode and brightness found at open. A light never shown is left alone;
    /// an unplugged one can't be restored and is skipped.
    fn drop(&mut self) {
        if self.shown.is_none() {
            return;
        }
        for (dir, mode, brightness) in &self.saved {
            let _ = std::fs::write(dir.join("mode"), mode);
            let _ = std::fs::write(dir.join("brightness"), brightness);
        }
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
    /// v7 ticket 75: in angle mode a stick fires the range it points into (0° = up,
    /// clockwise), never its linear directions; moving on releases before pressing; the
    /// dead zone fires nothing; a mode change waits for the stick to be let go.
    #[test]
    fn angle_mode_fires_the_range_the_stick_points_into() {
        use crate::mappings::{StickMode, Tuning};
        let mut tuning = Tuning::default(); // dead zone 15%
        tuning.sticks[0] = StickMode::sectors();
        let (mut down, mut pad) = (HashSet::new(), Analog::default());
        // Moves the left stick to `deg` at `push` (0–1) in one step.
        let mut push = |deg: f32,
                        push: f32,
                        tuning: &Tuning,
                        down: &mut HashSet<Control>,
                        pad: &mut Analog| {
            let (x, y) = (deg.to_radians().sin() * push, deg.to_radians().cos() * push);
            pad.left = (x, y);
            analog(down, pad, Motion::Axis(gilrs::Axis::LeftStickY, y), tuning)
        };
        let range = |i: u8| Control::button(ANGLE_BASE + i);
        let press = |c| Change::Event(Event::Press(c));
        let release = |c| Change::Event(Event::Release(c, ReleaseType::ButtonUp));
        assert_eq!(angle_of(0.0, 1.0), 0.0);
        assert_eq!(angle_of(1.0, 0.0), 90.0);
        assert_eq!(angle_of(-1.0, 0.0), 270.0);
        // 65° fires the 45°–90° sector, not "left" or "up".
        assert_eq!(
            push(65.0, 0.8, &tuning, &mut down, &mut pad),
            [press(range(1))]
        );
        assert_eq!(down, HashSet::from([range(1)]));
        assert_eq!(
            push(70.0, 0.9, &tuning, &mut down, &mut pad),
            [],
            "same range: held once"
        );
        assert_eq!(
            push(100.0, 0.8, &tuning, &mut down, &mut pad),
            [release(range(1)), press(range(2))]
        );
        // Back inside the dead zone: released; a drift-sized push fires nothing.
        assert_eq!(
            push(100.0, 0.1, &tuning, &mut down, &mut pad),
            [release(range(2))]
        );
        assert_eq!(
            push(300.0, 0.3, &tuning, &mut down, &mut pad),
            [],
            "under half travel"
        );
        assert_eq!(
            push(300.0, 0.6, &tuning, &mut down, &mut pad),
            [press(range(6))]
        );
        push(0.0, 0.0, &tuning, &mut down, &mut pad);
        // The right stick stays linear.
        pad.right = (0.8, 0.0);
        let right = analog(
            &mut down,
            &mut pad,
            Motion::Axis(gilrs::Axis::RightStickX, 0.8),
            &tuning,
        );
        assert_eq!(right, [press(Control::button(STICK_BASE + 7))]);
        // The user's own ranges, one wrapping through 0°; a gap fires nothing.
        tuning.sticks[0] = StickMode::Angle(vec![(50, 80), (330, 30)]);
        assert_eq!(
            push(65.0, 0.8, &tuning, &mut down, &mut pad),
            [press(range(0))]
        );
        assert_eq!(
            push(40.0, 0.8, &tuning, &mut down, &mut pad),
            [release(range(0))]
        );
        assert_eq!(
            push(10.0, 0.8, &tuning, &mut down, &mut pad),
            [press(range(1))]
        );
        assert_eq!(push(345.0, 0.8, &tuning, &mut down, &mut pad), []);
        // Changing the mode while pushed: its held range is forgotten, and nothing fires until
        // the stick is let go and pushed again.
        let mut linear = tuning.clone();
        linear.sticks[0] = StickMode::Linear;
        assert!(retune(&mut down, &mut pad, &tuning, &linear));
        assert!(!down.contains(&range(1)) && down.contains(&Control::button(STICK_BASE + 7)));
        assert_eq!(
            push(270.0, 0.9, &linear, &mut down, &mut pad),
            [],
            "still pushed"
        );
        assert_eq!(push(0.0, 0.0, &linear, &mut down, &mut pad), []);
        pad.left = (-0.9, 0.0);
        let left = analog(
            &mut down,
            &mut pad,
            Motion::Axis(gilrs::Axis::LeftStickX, -0.9),
            &linear,
        );
        assert_eq!(
            left,
            [press(Control::button(STICK_BASE + 2))],
            "fresh push: linear left"
        );
        assert!(
            !retune(&mut down, &mut pad, &linear, &linear.clone()),
            "no mode change"
        );
        assert_eq!(button_name(ANGLE_BASE + 17), "Right stick angle range 2");
    }

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
    fn headset_audio_is_reported_from_the_driver_and_cards_never_guessed() {
        let cards = " 0 [M3 ]: USB-Audio - MPK mini 3\n";
        let xbox_card = " 5 [Controller ]: XboxGIP - Xbox Wireless Controller audio\n";
        assert!(headset_status(&[], cards).contains("no Xbox controller"));
        assert!(headset_status(&["xpad"], cards).contains("needs the xone driver"));
        assert!(headset_status(&["xone-wired"], cards).contains("no controller audio device"));
        assert!(
            headset_status(&["xone-wired"], &format!("{cards}{xbox_card}")).contains("available")
        );
        assert!(headset_status(&["xpad"], xbox_card).contains("needs the xone driver"));
    }

    /// Review fix: a trigger held through a transition doesn't fire again on jitter; it
    /// fires once it's been let go and pulled again.
    #[test]
    fn held_triggers_wait_for_a_fresh_pull_after_a_transition() {
        let tuning = crate::mappings::Tuning::default();
        let mut pad = Analog::default();
        let mut down = HashSet::new();
        let a = control(gilrs::Button::South).unwrap();
        let rt = control(gilrs::Button::RightTrigger2).unwrap();
        down.insert(a);
        let mut feed = |down: &mut HashSet<Control>, v| {
            analog(
                down,
                &mut pad,
                Motion::Trigger(gilrs::Button::RightTrigger2, v),
                &tuning,
            )
        };
        assert_eq!(feed(&mut down, 0.9), [Change::Event(Event::Press(rt))]);
        keep_held_analog(&mut down);
        assert_eq!(
            down,
            [rt].into(),
            "buttons forgotten, the held trigger kept"
        );
        assert!(
            feed(&mut down, 0.92).is_empty(),
            "jitter doesn't fire again"
        );
        assert_eq!(
            feed(&mut down, 0.0),
            [Change::Event(Event::Release(rt, ReleaseType::ButtonUp))]
        );
        assert_eq!(feed(&mut down, 0.8), [Change::Event(Event::Press(rt))]);
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

    /// A stand-in for /sys/class/leds: one xone light (`gip0`) with the given mode, plus a
    /// keyboard light VibeConsole must never touch.
    fn leds(name: &str, mode: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("vibeconsole-leds-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (dir, mode) in [("gip0", mode), ("input2::capslock", "x")] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("mode"), format!("{mode}\n")).unwrap();
            std::fs::write(root.join(dir).join("brightness"), "20\n").unwrap();
        }
        root
    }
    fn mode(root: &std::path::Path, dir: &str) -> String {
        std::fs::read_to_string(root.join(dir).join("mode")).unwrap()
    }

    #[test]
    fn button_light_follows_running_and_paused_and_is_restored_on_quit() {
        let root = leds("follow", "3");
        let mut light = ButtonLight::open(&root).unwrap();
        assert_eq!(light.describe(), "Xbox button light: available (gip0)");
        assert_eq!(mode(&root, "gip0"), "3\n", "opening writes nothing");
        light.show(Light::Running).unwrap();
        assert_eq!(mode(&root, "gip0"), "1");
        light.show(Light::Paused).unwrap();
        assert_eq!(mode(&root, "gip0"), "4");
        // The same state again writes nothing: the file isn't rewritten.
        std::fs::write(root.join("gip0/mode"), "marker").unwrap();
        light.show(Light::Paused).unwrap();
        assert_eq!(mode(&root, "gip0"), "marker");
        std::fs::write(root.join("gip0/brightness"), "5").unwrap();
        drop(light);
        assert_eq!(mode(&root, "gip0"), "3", "quit puts the old mode back");
        assert_eq!(
            std::fs::read_to_string(root.join("gip0/brightness")).unwrap(),
            "20"
        );
        assert_eq!(
            mode(&root, "input2::capslock"),
            "x\n",
            "other lights untouched"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_reopen_after_plugging_in_keeps_each_lights_original_mode() {
        let root = leds("reopen", "3");
        let mut light = ButtonLight::open(&root).unwrap();
        light.show(Light::Paused).unwrap();
        // A second controller arrives; the first already shows VibeConsole's slow blink.
        std::fs::create_dir_all(root.join("gip1")).unwrap();
        std::fs::write(root.join("gip1/mode"), "1").unwrap();
        std::fs::write(root.join("gip1/brightness"), "20").unwrap();
        let mut light = ButtonLight::reopen(Some(light), &root).unwrap();
        assert_eq!(mode(&root, "gip0"), "4", "reopening writes nothing");
        light.show(Light::Running).unwrap();
        assert_eq!(
            (mode(&root, "gip0"), mode(&root, "gip1")),
            ("1".into(), "1".into())
        );
        drop(light);
        assert_eq!(
            (mode(&root, "gip0"), mode(&root, "gip1")),
            ("3".into(), "1".into())
        );
        // The last controller unplugged: nothing left to drive.
        std::fs::remove_dir_all(root.join("gip0")).unwrap();
        std::fs::remove_dir_all(root.join("gip1")).unwrap();
        assert!(ButtonLight::reopen(None, &root).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_light_never_shown_is_not_rewritten_on_quit() {
        let root = leds("unshown", "1");
        drop(ButtonLight::open(&root).unwrap());
        assert_eq!(mode(&root, "gip0"), "1\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn no_xone_light_or_no_write_permission_is_reported_unavailable() {
        let root = leds("none", "1");
        std::fs::remove_dir_all(root.join("gip0")).unwrap();
        let error = ButtonLight::open(&root).err().unwrap();
        assert!(
            error.starts_with("Xbox button light: unavailable: "),
            "{error}"
        );
        assert!(error.contains("xpad"), "{error}");
        std::fs::remove_dir_all(&root).unwrap();

        use std::os::unix::fs::PermissionsExt;
        let root = leds("readonly", "1");
        let file = root.join("gip0/mode");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        if std::fs::OpenOptions::new().write(true).open(&file).is_err() {
            // (Skipped when the tests run as root, which can write anything.)
            let error = ButtonLight::open(&root).err().unwrap();
            assert!(
                error.contains("not writable") && error.contains("gip0"),
                "{error}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_failed_write_names_its_stage() {
        let root = leds("gone", "1");
        let mut light = ButtonLight::open(&root).unwrap();
        std::fs::remove_dir_all(root.join("gip0")).unwrap(); // unplugged
        let error = light.show(Light::Running).unwrap_err().to_string();
        assert!(error.starts_with("Xbox button light: "), "{error}");
        drop(light); // restoring an unplugged light doesn't panic
        std::fs::remove_dir_all(root).unwrap();
    }
}
