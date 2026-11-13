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
/// v7 ticket 76: stick gestures in angle mode follow the ranges. Each stick has the six
/// turns, then a flick into each range, then a hold in each range.
pub const GESTURE_BASE: u8 = ANGLE_BASE + 2 * MAX_RANGES;
/// The turns, in id order: degrees, clockwise, name.
pub const TURNS: [(u16, bool, &str); 6] = [
    (90, true, "quarter turn clockwise"),
    (90, false, "quarter turn counterclockwise"),
    (180, true, "half circle clockwise"),
    (180, false, "half circle counterclockwise"),
    (360, true, "full circle clockwise"),
    (360, false, "full circle counterclockwise"),
];
const PER_STICK: u8 = TURNS.len() as u8 + 2 * MAX_RANGES;
/// Every control an Xbox controller has: buttons, stick directions, angle ranges, gestures.
pub const CONTROLS: usize = GESTURE_BASE as usize + 2 * PER_STICK as usize;

/// A stick gesture (v7 ticket 76), each its own control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gesture {
    /// One of [`TURNS`], from any start.
    Turn(usize),
    /// A quick push into a range and straight back.
    Flick(usize),
    /// Resting in a range for [`HOLD_MS`].
    Hold(usize),
}
impl Gesture {
    pub fn control(self, side: usize) -> Control {
        let at = match self {
            Self::Turn(t) => t as u8,
            Self::Flick(r) => TURNS.len() as u8 + r as u8,
            Self::Hold(r) => TURNS.len() as u8 + MAX_RANGES + r as u8,
        };
        Control::button(GESTURE_BASE + side as u8 * PER_STICK + at)
    }
    /// The stick and gesture a control id stands for.
    pub fn of(id: u8) -> Option<(usize, Self)> {
        let at = id.checked_sub(GESTURE_BASE)?;
        if at >= 2 * PER_STICK {
            return None;
        }
        let (side, at) = (usize::from(at / PER_STICK), at % PER_STICK);
        let turns = TURNS.len() as u8;
        Some((
            side,
            if at < turns {
                Self::Turn(usize::from(at))
            } else if at < turns + MAX_RANGES {
                Self::Flick(usize::from(at - turns))
            } else {
                Self::Hold(usize::from(at - turns - MAX_RANGES))
            },
        ))
    }
    fn name(self) -> String {
        match self {
            Self::Turn(t) => TURNS[t].2.into(),
            Self::Flick(r) => format!("flick into range {}", r + 1),
            Self::Hold(r) => format!("hold in range {}", r + 1),
        }
    }
}

pub fn control(button: gilrs::Button) -> Option<Control> {
    let id = BUTTONS.iter().position(|(b, _)| *b == button)?;
    Some(Control::button(u8::try_from(id).ok()?))
}
pub fn button_name(id: u8) -> String {
    if let Some((side, gesture)) = Gesture::of(id) {
        let side = if side == 0 { "Left" } else { "Right" };
        return format!("{side} stick {}", gesture.name());
    }
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
/// v7 ticket 110: the kind of Xbox control an id is, shown wherever Xbox controls are
/// listed instead of "pad": button, trigger, D-pad or stick (its press, directions, angle
/// ranges and gestures). Share isn't read (gilrs reports it only as an unknown button).
pub fn kind(id: u8) -> &'static str {
    match id {
        6 | 7 => "Xbox trigger",
        13..=16 => "Xbox D-pad",
        11 | 12 => "Xbox stick",
        id if id >= STICK_BASE => "Xbox stick",
        _ => "Xbox button",
    }
}
/// Which stick (0 left, 1 right) a direction or angle range belongs to.
fn stick_of(control: Control) -> Option<usize> {
    match control.id {
        id if (STICK_BASE..ANGLE_BASE).contains(&id) => Some(usize::from((id - STICK_BASE) / 4)),
        id if id >= GESTURE_BASE => Gesture::of(id).map(|(side, _)| side),
        id if id >= ANGLE_BASE => Some(usize::from((id - ANGLE_BASE) / MAX_RANGES)),
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
/// what was saved for it. Xbox program 1.
pub fn layer(saved: &[Mapping], second: bool) -> Vec<Mapping> {
    layer_in(saved, 1, second)
}
/// v7 ticket 115: a layer of one Xbox program (1–8).
pub fn layer_in(saved: &[Mapping], program: u8, second: bool) -> Vec<Mapping> {
    if second {
        let context = crate::mappings::xbox_context(program, true);
        saved
            .iter()
            .filter(|m| m.context == context)
            .cloned()
            .collect()
    } else {
        layout_in(saved, program)
    }
}
/// The layout in use for Xbox program 1: each saved Xbox mapping replaces that button's
/// default.
pub fn layout(saved: &[Mapping]) -> Vec<Mapping> {
    layout_in(saved, 1)
}
/// v7 ticket 115: an Xbox program's layer-1 layout; the built-in defaults apply in every
/// program until a button is saved there.
pub fn layout_in(saved: &[Mapping], program: u8) -> Vec<Mapping> {
    let context = crate::mappings::xbox_context(program, false);
    let saved = saved
        .iter()
        .filter(|m| m.context == context)
        .cloned()
        .collect::<Vec<_>>();
    let mut layout = default_layout()
        .into_iter()
        .filter(|d| !saved.iter().any(|m| m.control == d.control))
        .map(|mut d| {
            d.context = context;
            d
        })
        .collect::<Vec<_>>();
    layout.extend(saved);
    layout
}

/// v7 ticket 115: what View and Menu do. Held, LB, LT, RB or RT picks an Xbox program
/// (View: 1–4, Menu: 5–8) and nothing else fires; let go without a pick, View or Menu fires
/// its own mapping then (fire on release: an exception the user agreed to on 12 Nov 2026).
#[derive(Default)]
pub struct ProgramKeys {
    /// View or Menu held, and whether it picked a program.
    held: Option<(Control, bool)>,
}
#[derive(Debug, PartialEq)]
pub enum ProgramStep {
    /// Not ours: handle the event as usual.
    Pass,
    /// Taken: nothing fires.
    Swallow,
    /// Switch to this Xbox program (1–8).
    Select(u8),
    /// View or Menu tapped alone: fire its own mapping now.
    Tap(Control),
}
impl ProgramKeys {
    pub fn event(&mut self, event: &Event) -> ProgramStep {
        let view = control(gilrs::Button::Select);
        let menu = control(gilrs::Button::Start);
        let picks = [
            gilrs::Button::LeftTrigger,
            gilrs::Button::LeftTrigger2,
            gilrs::Button::RightTrigger,
            gilrs::Button::RightTrigger2,
        ]
        .map(control);
        let key = |c: &Control| Some(*c) == view || Some(*c) == menu;
        match event {
            Event::Press(c) if key(c) => {
                if self.held.is_none() {
                    self.held = Some((*c, false));
                }
                ProgramStep::Swallow
            }
            Event::DuplicatePress(c) if key(c) => ProgramStep::Swallow,
            Event::Release(c, _) | Event::UnmatchedRelease(c) if key(c) => match self.held {
                Some((held, used)) if held == *c => {
                    self.held = None;
                    if used || matches!(event, Event::UnmatchedRelease(_)) {
                        ProgramStep::Swallow
                    } else {
                        ProgramStep::Tap(*c)
                    }
                }
                _ => ProgramStep::Swallow,
            },
            Event::Press(c) => match (&mut self.held, picks.iter().position(|p| *p == Some(*c))) {
                (Some((held, used)), Some(at)) => {
                    *used = true;
                    let base = if Some(*held) == view { 0 } else { 4 };
                    ProgramStep::Select(base + at as u8 + 1)
                }
                _ => ProgramStep::Pass,
            },
            _ => ProgramStep::Pass,
        }
    }
    /// A transition: a View or Menu hold in progress is forgotten (its release fires nothing).
    pub fn cancel(&mut self) {
        self.held = None;
    }
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
    /// v7 ticket 76: a stick in angle mode with gestures mapped; [`Gestures`] fires its
    /// ranges and gestures instead of [`analog`].
    pub gestures: [bool; 2],
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
                if analog.gestures[side] {
                    return changes;
                }
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

/// v7 ticket 76: gesture timings in milliseconds. ponytail: fixed provisional values; tune
/// them from developer test 76 on the real controller, and move them into Tuning (a config
/// bump) if they need to differ per user.
/// Resting this long in one range makes the stroke plain angle mode (the range is held).
pub const SETTLE_MS: u64 = 250;
/// A flick goes out and back to rest within this.
pub const FLICK_MS: u64 = 250;
/// Resting this long in a range fires its hold gesture.
pub const HOLD_MS: u64 = 700;
/// A turn completes within this, from leaving rest to coming back.
pub const TURN_MS: u64 = 1200;
/// Degrees a quarter, half and full turn must sweep: room for a sloppy thumb.
fn reach(degrees: u16) -> f32 {
    match degrees {
        90 => 70.0,
        180 => 155.0,
        _ => 320.0,
    }
}

/// One stick's movement from leaving rest until it's back at rest.
#[derive(Clone, Copy, Debug, Default)]
struct Stroke {
    /// When it left rest; None at rest.
    start: Option<u64>,
    angle: f32,
    /// Degrees swept so far, clockwise positive.
    travel: f32,
    range: Option<usize>,
    /// When it entered the range it's in.
    since: u64,
    push: f32,
    farthest: f32,
    /// The range it pointed into at its farthest push.
    flick: Option<usize>,
    /// Rested in a range: plain angle mode until let go, no gesture.
    sectors: bool,
    /// A hold fired or a transition cut it: nothing more until let go.
    inert: bool,
}

/// v7 ticket 76: stick gestures, like EA FC's skill moves. Only a stick in angle mode with a
/// gesture mapped uses this; there a gesture fires once when it completes, and the ranges
/// it passes don't fire.
#[derive(Default)]
pub struct Gestures {
    strokes: [Stroke; 2],
    /// The gesture controls mapped on either layer.
    mapped: HashSet<Control>,
}
impl Gestures {
    /// The gestures `mappings` use, and which sticks therefore detect them.
    pub fn set(&mut self, mappings: &[Mapping], analog: &mut Analog) {
        self.mapped = mappings
            .iter()
            .map(|m| m.control)
            .filter(|c| c.message == crate::Message::Button && Gesture::of(c.id).is_some())
            .collect();
        analog.gestures = [0, 1].map(|side| self.mapped.iter().any(|c| stick_of(*c) == Some(side)));
    }
    /// A transition: a stroke in progress fires nothing, now or when it ends.
    pub fn cancel(&mut self) {
        for stroke in &mut self.strokes {
            stroke.inert |= stroke.start.is_some();
        }
    }
    /// After a stick mode change: the stick starts over.
    fn forget(&mut self, side: usize) {
        self.strokes[side] = Stroke::default();
    }
    fn mapped(&self, side: usize, gesture: Gesture) -> bool {
        self.mapped.contains(&gesture.control(side))
    }
    /// A stick with gestures moved to `(x, y)` at `now`.
    pub fn motion(
        &mut self,
        side: usize,
        (x, y): (f32, f32),
        tuning: &crate::mappings::Tuning,
        down: &mut HashSet<Control>,
        now: u64,
    ) -> Vec<Change> {
        let mut changes = Vec::new();
        let mode = &tuning.sticks[side];
        if !matches!(mode, crate::mappings::StickMode::Angle(_)) {
            return changes;
        }
        let dead = f32::from(tuning.dead_zone);
        let push = x.hypot(y).min(1.0) * 100.0;
        let base = ANGLE_BASE + side as u8 * MAX_RANGES;
        let held = (base..base + MAX_RANGES)
            .map(Control::button)
            .find(|c| down.contains(c));
        if push <= dead {
            if let Some(held) = held {
                down.remove(&held);
                changes.push(Change::Event(Event::Release(held, ReleaseType::ButtonUp)));
            }
            let stroke = self.strokes[side];
            if let Some(start) = stroke.start.filter(|_| !stroke.sectors && !stroke.inert) {
                let fired = self.finish(side, &stroke, now.saturating_sub(start), tuning);
                changes.extend(fired.into_iter().flat_map(tap));
            }
            self.strokes[side] = Stroke::default();
            return changes;
        }
        let angle = angle_of(x, y);
        let range = mode.range_at(angle);
        let stroke = &mut self.strokes[side];
        if stroke.start.is_none() {
            *stroke = Stroke {
                start: Some(now),
                angle,
                range,
                since: now,
                ..Stroke::default()
            };
        } else {
            let mut turn = angle - stroke.angle;
            if turn > 180.0 {
                turn -= 360.0;
            } else if turn < -180.0 {
                turn += 360.0;
            }
            stroke.travel += turn;
            stroke.angle = angle;
            if range != stroke.range {
                stroke.range = range;
                stroke.since = now;
            }
        }
        stroke.push = push;
        if push > stroke.farthest {
            stroke.farthest = push;
            stroke.flick = range;
        }
        if stroke.inert {
            return changes;
        }
        if stroke.sectors {
            // Plain angle mode until let go: the range pointed into is held.
            let target = range.map(|r| Control::button(base + r as u8));
            if held != target {
                if let Some(held) = held {
                    down.remove(&held);
                    changes.push(Change::Event(Event::Release(held, ReleaseType::ButtonUp)));
                }
                if let Some(target) = target {
                    down.insert(target);
                    changes.push(Change::Event(Event::Press(target)));
                }
            }
            return changes;
        }
        changes.extend(self.dwell(side, tuning, down, now));
        changes
    }
    /// Time passing with the sticks still: a rest in a range holds it or fires its hold.
    pub fn tick(
        &mut self,
        tuning: &crate::mappings::Tuning,
        down: &mut HashSet<Control>,
        now: u64,
    ) -> Vec<Change> {
        (0..2)
            .flat_map(|side| self.dwell(side, tuning, down, now))
            .collect()
    }
    fn dwell(
        &mut self,
        side: usize,
        tuning: &crate::mappings::Tuning,
        down: &mut HashSet<Control>,
        now: u64,
    ) -> Vec<Change> {
        let stroke = self.strokes[side];
        let (Some(start), Some(range)) = (stroke.start, stroke.range) else {
            return Vec::new();
        };
        let press = f32::from(tuning.dead_zone).max(50.0);
        if stroke.sectors || stroke.inert || stroke.push < press {
            return Vec::new();
        }
        // A turn under way isn't a rest, until it's too late to be a turn.
        let turning = stroke.travel.abs() >= reach(90)
            && now.saturating_sub(start) <= TURN_MS
            && (0..TURNS.len()).any(|t| self.mapped(side, Gesture::Turn(t)));
        let rest = now.saturating_sub(stroke.since);
        if turning {
            return Vec::new();
        }
        if self.mapped(side, Gesture::Hold(range)) {
            // A hold gesture replaces its range's plain press.
            if rest >= HOLD_MS {
                self.strokes[side].inert = true;
                return tap(Gesture::Hold(range).control(side));
            }
            return Vec::new();
        }
        if rest >= SETTLE_MS {
            self.strokes[side].sectors = true;
            let control = Control::button(ANGLE_BASE + side as u8 * MAX_RANGES + range as u8);
            down.insert(control);
            return vec![Change::Event(Event::Press(control))];
        }
        Vec::new()
    }
    /// What a stroke that came back to rest fired: the biggest mapped turn it completed in
    /// time, else a mapped flick, else (a short push that wasn't a turn) its range, once.
    fn finish(
        &self,
        side: usize,
        stroke: &Stroke,
        took: u64,
        tuning: &crate::mappings::Tuning,
    ) -> Option<Control> {
        let turn = (0..TURNS.len())
            .filter(|&t| {
                let (degrees, clockwise, _) = TURNS[t];
                took <= TURN_MS
                    && (stroke.travel > 0.0) == clockwise
                    && stroke.travel.abs() >= reach(degrees)
                    && self.mapped(side, Gesture::Turn(t))
            })
            .max_by_key(|&t| TURNS[t].0);
        if let Some(turn) = turn {
            return Some(Gesture::Turn(turn).control(side));
        }
        let press = f32::from(tuning.dead_zone).max(50.0);
        if stroke.travel.abs() >= reach(90) || stroke.farthest < press {
            return None;
        }
        let range = stroke.flick?;
        if took <= FLICK_MS && self.mapped(side, Gesture::Flick(range)) {
            return Some(Gesture::Flick(range).control(side));
        }
        Some(Control::button(
            ANGLE_BASE + side as u8 * MAX_RANGES + range as u8,
        ))
    }
}
/// A gesture or a quick range: pressed and let go at once.
fn tap(control: Control) -> Vec<Change> {
    vec![
        Change::Event(Event::Press(control)),
        Change::Event(Event::Release(control, ReleaseType::ButtonUp)),
    ]
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

/// v7 ticket 27: whether headset audio through the controller's 3.5 mm jack can work, from
/// which driver holds the controller (`bound`: driver names with a device bound) and ALSA's
/// card list. Only `xone` carries controller audio; VibeConsole never loads or unloads a
/// kernel module, it only says what it found.
pub fn headset_status(bound: &[&str], cards: &str) -> String {
    let xone = bound.iter().any(|d| d.starts_with("xone"));
    let xpad = bound.contains(&"xpad");
    let audio = headset_card(cards);
    match (xone, xpad, audio) {
        (true, _, true) => "Xbox headset audio: available (xone driver, controller audio device present)".into(),
        (true, _, false) => {
            "Xbox headset audio: unavailable: xone is bound but no controller audio device is present \
             (plug a headset into the controller)"
                .into()
        }
        (false, true, _) => "Xbox headset audio: unavailable: needs the xone driver (the controller \
             uses xpad); install xone yourself (github.com/dlundqvist/xone, its README), then \
             replug the controller"
            .into(),
        (false, false, _) => "Xbox headset audio: no Xbox controller on a USB driver".into(),
    }
}
/// USB drivers in /sys that have a device bound now.
pub fn bound_drivers() -> Vec<&'static str> {
    ["xpad", "xone-wired", "xone-dongle", "xone-gip"]
        .into_iter()
        .filter(|driver| {
            std::fs::read_dir(format!("/sys/bus/usb/drivers/{driver}")).is_ok_and(|entries| {
                entries
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy().contains(':'))
            })
        })
        .collect()
}
/// The real inputs for `headset_status`: drivers in /sys with a device bound, and
/// /proc/asound/cards.
pub fn headset() -> String {
    let cards = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
    headset_status(&bound_drivers(), &cards)
}
/// v7 ticket 129: which driver holds the controller now, from `bound_drivers`.
pub fn driver_in_use(bound: &[&str]) -> Option<&'static str> {
    if bound.iter().any(|d| d.starts_with("xone")) {
        Some("xone")
    } else if bound.contains(&"xpad") {
        Some("xpad")
    } else {
        None
    }
}
/// The ALSA card id of the controller's headset (`Headset` for xone), from
/// /proc/asound/cards; only a plain identifier is accepted.
pub fn headset_card_id(cards: &str) -> Option<String> {
    cards.lines().find_map(|line| {
        let lower = line.to_lowercase();
        if !(lower.contains("xbox") || lower.contains("xone") || lower.contains("gip")) {
            return None;
        }
        let id = line.split_once('[')?.1.split_once(']')?.0.trim();
        (!id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .then(|| id.to_owned())
    })
}
/// xone's modules, dependents first (its `dkms.conf`), so `modprobe -r -a` can unload them.
pub const XONE_MODULES: [&str; 9] = [
    "xone_gip_gamepad",
    "xone_gip_headset",
    "xone_gip_chatpad",
    "xone_gip_madcatz_strat",
    "xone_gip_madcatz_glam",
    "xone_gip_pdp_jaguar",
    "xone_wired",
    "xone_dongle",
    "xone_gip",
];
/// VibeConsole's own modprobe file: present only while xpad is chosen over an installed xone.
pub const DRIVER_FILE: &str = "vibeconsole-xbox-driver.conf";
/// xone's blacklist (its `install.sh`), set aside while xpad is chosen.
pub const XONE_FILE: &str = "xone-blacklist.conf";
const XONE_OFF: &str = "xone-blacklist.conf.vibeconsole-off";
/// v7 ticket 129, run as root (`pkexec vibeconsole xbox-driver <xpad|xone>`). First the
/// controller is unbound from the old driver (its sound card goes with it, so PipeWire lets
/// go), then the new driver is loaded and bound to it, and only then is the choice saved so
/// it survives a reboot: modprobe's `blacklist` stops only automatic loading, so xpad writes
/// `DRIVER_FILE` (blacklisting xone) and sets xone's own blacklist of xpad aside; xone undoes
/// both. Unloading the old modules is best effort: one still in use stays loaded, bound to
/// nothing. A failed load binds the controller back to the old driver. `etc` is
/// /etc/modprobe.d, `usb` /sys/bus/usb/drivers, `loaded` the text of /proc/modules. Never
/// installs, builds or signs a driver. Errors name their stage.
pub fn switch_driver(
    driver: &str,
    etc: &std::path::Path,
    usb: &std::path::Path,
    loaded: &str,
    xone_installed: bool,
    run: &mut dyn FnMut(&[&str]) -> crate::Result<String>,
) -> crate::Result<String> {
    if !matches!(driver, "xpad" | "xone") {
        return Err("xbox-driver: choose xpad or xone".into());
    }
    if driver == "xone" && !xone_installed {
        return Err("xbox-driver: unavailable: xone isn't installed (github.com/dlundqvist/xone; install it yourself)".into());
    }
    let (old, from, to, module) = if driver == "xpad" {
        ("xone", "xone-wired", "xpad", "xpad")
    } else {
        ("xpad", "xpad", "xone-wired", "xone_wired")
    };
    let mut interfaces = std::fs::read_dir(usb.join(from))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name.contains(':'))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    interfaces.sort();
    let attach = |dir: &str, file: &str| {
        for interface in &interfaces {
            std::fs::write(usb.join(dir).join(file), interface)?;
        }
        std::io::Result::Ok(())
    };
    attach(from, "unbind").map_err(|e| format!("xbox-driver: unbind: {e}"))?;
    if let Err(e) = run(&["modprobe", module]) {
        let back = match attach(from, "bind") {
            Ok(()) => format!("the controller is back on {old}"),
            Err(_) => "replug the controller".to_owned(),
        };
        return Err(format!("xbox-driver: load: {e}; {back}").into());
    }
    // Loading binds a free controller; an already loaded driver needs telling.
    for interface in &interfaces {
        if !usb.join(to).join(interface).exists() {
            let _ = std::fs::write(usb.join(to).join("bind"), interface);
        }
    }
    let loaded = loaded
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect::<Vec<_>>();
    let mut unload = vec!["modprobe", "-r", "-a"];
    if driver == "xpad" {
        unload.extend(XONE_MODULES.iter().filter(|m| loaded.contains(m)));
    } else if loaded.contains(&"xpad") {
        unload.push("xpad");
    }
    let note = if unload.len() > 3 && run(&unload).is_err() {
        format!("; some {old} modules stay loaded until a reboot (they hold nothing)")
    } else {
        String::new()
    };
    let save = |e: std::io::Error| format!("xbox-driver: save: {e}");
    if driver == "xpad" {
        if xone_installed {
            crate::actions::write_atomic(
                &etc.join(DRIVER_FILE),
                "# Written by VibeConsole (Settings -> Xbox tuning -> Xbox driver: xpad).\n\
                 blacklist xone_wired\nblacklist xone_dongle\nblacklist xone_gip\n",
            )
            .map_err(|e| format!("xbox-driver: save: {e}"))?;
        }
        if etc.join(XONE_FILE).exists() {
            std::fs::rename(etc.join(XONE_FILE), etc.join(XONE_OFF)).map_err(save)?;
        }
    } else {
        if etc.join(XONE_OFF).exists() {
            std::fs::rename(etc.join(XONE_OFF), etc.join(XONE_FILE)).map_err(save)?;
        }
        match std::fs::remove_file(etc.join(DRIVER_FILE)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(save(e).into()),
            _ => {}
        }
    }
    Ok(format!("Xbox driver switched to {driver}{note}"))
}
/// v7 ticket 130: VibeConsole's udev rule that lets the `input` group (the user is in it to
/// read controllers) write xone's controller light. udev runs `RUN+=` programs with the
/// device's sysfs path in `%p` (udev(7)); the same pattern as github.com/medusalix/xone's
/// "LED control" rule and ublue-os/bazzite#5315.
pub const LIGHT_RULE_FILE: &str = "70-vibeconsole-xbox-light.rules";
pub const LIGHT_RULE: &str = "# Written by VibeConsole (Settings -> Xbox tuning -> Install the light rule).\n\
ACTION==\"add\", SUBSYSTEM==\"leds\", KERNEL==\"gip*\", \
RUN+=\"/usr/bin/chgrp input /sys%p/mode /sys%p/brightness\", \
RUN+=\"/usr/bin/chmod g+w /sys%p/mode /sys%p/brightness\"\n";
/// v7 ticket 130, run as root (`pkexec vibeconsole xbox-light-rule <install|remove>`): writes
/// or removes only VibeConsole's own rule in `rules` (/etc/udev/rules.d), then reloads udev
/// and replays the `leds` add events so it applies without a replug. Errors name the stage.
pub fn light_rule(
    action: &str,
    rules: &std::path::Path,
    run: &mut dyn FnMut(&[&str]) -> crate::Result<String>,
) -> crate::Result<String> {
    let file = rules.join(LIGHT_RULE_FILE);
    match action {
        "install" => crate::actions::write_atomic(&file, LIGHT_RULE)
            .map_err(|e| format!("xbox-light-rule: save: {e}"))?,
        "remove" => match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("xbox-light-rule: save: {e}").into());
            }
            _ => {}
        },
        _ => return Err("xbox-light-rule: choose install or remove".into()),
    }
    run(&["udevadm", "control", "--reload"])
        .map_err(|e| format!("xbox-light-rule: reload: {e}"))?;
    run(&[
        "udevadm",
        "trigger",
        "--subsystem-match=leds",
        "--action=add",
    ])
    .map_err(|e| format!("xbox-light-rule: apply: {e}"))?;
    Ok(format!(
        "Xbox light rule {}",
        if action == "install" {
            "installed"
        } else {
            "removed"
        }
    ))
}
/// Whether VibeConsole's light rule is installed.
pub fn light_rule_installed() -> bool {
    std::path::Path::new("/etc/udev/rules.d")
        .join(LIGHT_RULE_FILE)
        .exists()
}
/// Whether xone's modules are installed for the running kernel (`modinfo` finds them).
pub fn xone_installed(run: &mut dyn FnMut(&[&str]) -> crate::Result<String>) -> bool {
    run(&["modinfo", "-n", "xone_wired"]).is_ok_and(|path| !path.trim().is_empty())
}

/// Runs a program with fixed arguments and returns its output; injected in tests.
pub type Runner = Box<dyn FnMut(&[&str]) -> crate::Result<String> + Send>;
/// The real runner: one argument per value, never a shell. A failure names the program.
pub fn run_program() -> Runner {
    Box::new(|args: &[&str]| {
        let (program, rest) = args.split_first().ok_or("no program")?;
        let output = std::process::Command::new(program)
            .args(rest)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("{program}: {e}"))?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!("{program}: {}", err.trim()).into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    })
}

/// v7 ticket 108: the controller headset's PipeWire nodes and the configured defaults,
/// read from `pw-dump`. Nodes are found by their ALSA card name (xone's card is "Microsoft
/// Xbox Headset"); ids are only used as `wpctl` arguments.
#[derive(Debug, Default, PartialEq)]
pub struct AudioGraph {
    /// (id, node.name) of the controller's sink, then its source.
    pub headset: [Option<(u32, String)>; 2],
    /// `default.configured.audio.sink` and `.source` node names; None when not configured.
    pub configured: [Option<String>; 2],
    /// Every audio sink and source's (id, node.name), to find a remembered default again.
    pub nodes: Vec<(u32, String)>,
}
pub fn audio_graph(pw_dump: &str) -> AudioGraph {
    let mut graph = AudioGraph::default();
    let Ok(serde_json::Value::Array(objects)) = serde_json::from_str(pw_dump) else {
        return graph;
    };
    for object in &objects {
        let props = &object["info"]["props"];
        let kind = object["type"].as_str().unwrap_or_default();
        if kind.ends_with(":Node") {
            let slot = match props["media.class"].as_str() {
                Some("Audio/Sink") => 0,
                Some("Audio/Source") => 1,
                _ => continue,
            };
            let (Some(id), Some(name)) = (object["id"].as_u64(), props["node.name"].as_str())
            else {
                continue;
            };
            let Ok(id) = u32::try_from(id) else { continue };
            graph.nodes.push((id, name.to_owned()));
            let card = props["alsa.card_name"].as_str().unwrap_or_default();
            if card.to_lowercase().contains("xbox") && graph.headset[slot].is_none() {
                graph.headset[slot] = Some((id, name.to_owned()));
            }
        } else if kind.ends_with(":Metadata") && object["props"]["metadata.name"] == "default" {
            for entry in object["metadata"].as_array().into_iter().flatten() {
                let slot = match entry["key"].as_str() {
                    Some("default.configured.audio.sink") => 0,
                    Some("default.configured.audio.source") => 1,
                    _ => continue,
                };
                graph.configured[slot] = entry["value"]["name"].as_str().map(str::to_owned);
            }
        }
    }
    graph
}

/// v7 ticket 108: "Use the controller headset". While the option is on and the headset's
/// nodes exist, they are the default speaker and mic; the defaults from before come back
/// when it's unplugged, the option goes off, or VibeConsole quits (drop).
pub struct HeadsetAudio {
    run: Runner,
    /// The configured defaults from before; Some while VibeConsole holds the headset.
    previous: Option<[Option<String>; 2]>,
    /// (option, card present) at the last `follow`, so a failure isn't retried each tick.
    seen: Option<(bool, bool)>,
}
impl HeadsetAudio {
    pub fn new(run: Runner) -> Self {
        Self {
            run,
            previous: None,
            seen: None,
        }
    }
    pub fn holding(&self) -> bool {
        self.previous.is_some()
    }
    fn graph(&mut self) -> crate::Result<AudioGraph> {
        let dump = (self.run)(&["pw-dump"]).map_err(|e| format!("headset audio: read: {e}"))?;
        Ok(audio_graph(&dump))
    }
    fn set_default(&mut self, id: u32) -> crate::Result<()> {
        (self.run)(&["wpctl", "set-default", &id.to_string()])
            .map(drop)
            .map_err(|e| format!("headset audio: set default: {e}").into())
    }
    /// Makes the controller's speaker and mic the defaults, remembering the ones before.
    pub fn use_headset(&mut self) -> crate::Result<()> {
        let graph = self.graph()?;
        let [Some(sink), Some(source)] = &graph.headset else {
            return Err(
                "Xbox headset audio: unavailable: no controller audio device in \
                        PipeWire (needs xone and a headset in the controller)"
                    .into(),
            );
        };
        let (sink, source) = (sink.0, source.0);
        if self.previous.is_none() {
            self.previous = Some(graph.configured);
        }
        self.set_default(sink)?;
        self.set_default(source)
    }
    /// Puts back the defaults from before; nothing when VibeConsole holds nothing. A default
    /// that was never configured is cleared; one whose device is gone is left to WirePlumber.
    pub fn restore(&mut self) -> crate::Result<()> {
        let Some(previous) = self.previous.take() else {
            return Ok(());
        };
        let graph = self.graph()?;
        for (slot, name) in previous.into_iter().enumerate() {
            match name {
                None => {
                    (self.run)(&["wpctl", "clear-default", &slot.to_string()])
                        .map_err(|e| format!("headset audio: clear default: {e}"))?;
                }
                Some(name) => {
                    if let Some((id, _)) = graph.nodes.iter().find(|(_, n)| *n == name) {
                        self.set_default(*id)?;
                    }
                }
            }
        }
        Ok(())
    }
    /// Each status refresh: `wanted` is the option, `present` whether the controller's audio
    /// card is listed. Acts only when one of them changes, so a failure isn't repeated.
    pub fn follow(&mut self, wanted: bool, present: bool) -> crate::Result<()> {
        let changed = self.seen != Some((wanted, present));
        self.seen = Some((wanted, present));
        if self.holding() && !(wanted && present) {
            return self.restore();
        }
        if wanted && present && !self.holding() && changed {
            return self.use_headset();
        }
        Ok(())
    }
}
impl Drop for HeadsetAudio {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
/// Whether ALSA lists the controller's audio card (xone's "Microsoft Xbox Headset").
pub fn headset_card(cards: &str) -> bool {
    cards.lines().any(|line| {
        let line = line.to_lowercase();
        line.contains("xbox") || line.contains("xone") || line.contains("gip")
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
    /// v7 ticket 76: stick gestures, timed from `clock`.
    gestures: Gestures,
    clock: std::time::Instant,
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
            gestures: Gestures::default(),
            clock: std::time::Instant::now(),
        })
    }
    /// The gestures the saved mappings use (v7 ticket 76).
    pub fn set_gestures(&mut self, mappings: &[Mapping]) {
        self.gestures.set(mappings, &mut self.analog);
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
                let side = match motion {
                    Motion::Axis(gilrs::Axis::LeftStickX | gilrs::Axis::LeftStickY, _) => 0,
                    Motion::Axis(gilrs::Axis::RightStickX | gilrs::Axis::RightStickY, _) => 1,
                    _ => continue,
                };
                if !self.analog.gestures[side] {
                    continue;
                }
                if self.analog.wait[side] {
                    self.gestures.forget(side);
                    continue;
                }
                let stick = [self.analog.left, self.analog.right][side];
                let now = self.clock.elapsed().as_millis() as u64;
                changes.extend(self.gestures.motion(
                    side,
                    stick,
                    &self.tuning,
                    &mut self.down,
                    now,
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
        let now = self.clock.elapsed().as_millis() as u64;
        changes.extend(self.gestures.tick(&self.tuning, &mut self.down, now));
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
        self.gestures.cancel();
    }
    /// One short rumble on every controller that supports it, when Tuning has it on.
    /// Rumble is hardware feedback: it plays only while VibeConsole owns the controller.
    /// v7 ticket 86: a short (300 ms) or long (900 ms) rumble; Off, or rumble turned off in
    /// Xbox tuning, plays nothing.
    pub fn rumble(&mut self, rumble: crate::status::Rumble) -> crate::Result<()> {
        use gilrs::ff::{BaseEffect, BaseEffectType, EffectBuilder, Repeat, Replay, Ticks};
        let ms = match rumble {
            crate::status::Rumble::Off => return Ok(()),
            crate::status::Rumble::Short => 300,
            crate::status::Rumble::Long => 900,
        };
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
        let length = Ticks::from_ms(ms);
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
    /// v7 ticket 86: running, showing the most urgent agent state's pattern.
    Agent(crate::status::Light),
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
                        (xpad has none for Xbox One/Series controllers; switch to xone in \
                        Settings → Xbox tuning)"
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
                         (Settings → Xbox tuning → Install the light rule gives your user its \
                         mode and brightness)",
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
        use crate::status::Light as Pattern;
        let mode = match light {
            Light::Running | Light::Agent(Pattern::Steady) => "1",
            Light::Paused => "4",
            Light::Agent(Pattern::Off) => "0",
            Light::Agent(Pattern::FastBlink) => "2",
            Light::Agent(Pattern::Blink) => "3",
            Light::Agent(Pattern::SlowPulse) => "8",
        };
        // v7 ticket 114: every controller's light; one that fails (unplugged) is dropped and
        // the others keep showing. Only when none is left is it an error.
        let mut failed = None;
        self.saved
            .retain(|(dir, ..)| match std::fs::write(dir.join("mode"), mode) {
                Ok(()) => true,
                Err(e) => {
                    failed = Some(format!(
                        "Xbox button light: cannot set {}: {e}",
                        dir.display()
                    ));
                    false
                }
            });
        if let (Some(error), true) = (failed, self.saved.is_empty()) {
            return Err(error.into());
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

    /// v7 ticket 108: the controller's speaker and mic are found in `pw-dump` by their card;
    /// using the headset makes them the defaults, and unplug, the option going off or quit
    /// puts the old ones back (a never-configured default is cleared). No node: nothing runs.
    #[test]
    fn controller_headset_becomes_the_default_and_the_old_defaults_come_back() {
        let node = |id: u32, class: &str, name: &str, card: &str| {
            format!(
                r#"{{"id":{id},"type":"PipeWire:Interface:Node","info":{{"props":{{"media.class":"{class}","node.name":"{name}","alsa.card_name":"{card}"}}}}}}"#
            )
        };
        let defaults = |sink: &str| {
            format!(
                r#"{{"id":30,"type":"PipeWire:Interface:Metadata","props":{{"metadata.name":"default"}},"metadata":[{{"subject":0,"key":"default.configured.audio.sink","type":"Spa:String:JSON","value":{{"name":"{sink}"}}}}]}}"#
            )
        };
        let dump = format!(
            "[{},{},{},{}]",
            node(65, "Audio/Sink", "alsa_output.nommo", "Razer Nommo Chroma"),
            node(
                117,
                "Audio/Sink",
                "alsa_output.usb-Microsoft_Controller-00.stereo-fallback",
                "Microsoft Xbox Headset"
            ),
            node(
                71,
                "Audio/Source",
                "alsa_input.usb-Microsoft_Controller-00.mono-fallback",
                "Microsoft Xbox Headset"
            ),
            defaults("alsa_output.nommo"),
        );
        let graph = audio_graph(&dump);
        assert_eq!(graph.headset[0].as_ref().map(|n| n.0), Some(117));
        assert_eq!(graph.headset[1].as_ref().map(|n| n.0), Some(71));
        assert_eq!(
            graph.configured,
            [Some("alsa_output.nommo".to_owned()), None]
        );

        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let fake = |dump: String, calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>| -> Runner {
            Box::new(move |args: &[&str]| {
                calls.lock().unwrap().push(args.join(" "));
                Ok(if args[0] == "pw-dump" {
                    dump.clone()
                } else {
                    String::new()
                })
            })
        };
        let mut audio = HeadsetAudio::new(fake(dump.clone(), calls.clone()));
        audio.follow(false, true).unwrap(); // option off: nothing
        assert!(calls.lock().unwrap().is_empty());
        audio.follow(true, true).unwrap();
        audio.follow(true, true).unwrap(); // nothing changed: nothing again
        assert_eq!(
            *calls.lock().unwrap(),
            ["pw-dump", "wpctl set-default 117", "wpctl set-default 71"]
        );
        calls.lock().unwrap().clear();
        audio.follow(true, false).unwrap(); // unplugged
        assert_eq!(
            *calls.lock().unwrap(),
            ["pw-dump", "wpctl set-default 65", "wpctl clear-default 1"]
        );
        calls.lock().unwrap().clear();
        audio.follow(true, true).unwrap(); // back: used again, then quit puts it back
        drop(audio);
        assert_eq!(
            *calls.lock().unwrap(),
            [
                "pw-dump",
                "wpctl set-default 117",
                "wpctl set-default 71",
                "pw-dump",
                "wpctl set-default 65",
                "wpctl clear-default 1"
            ]
        );

        // xpad, or no headset node: unavailable with the reason; no default changes.
        calls.lock().unwrap().clear();
        let mut none = HeadsetAudio::new(fake(
            format!(
                "[{}]",
                node(65, "Audio/Sink", "alsa_output.nommo", "Razer Nommo Chroma")
            ),
            calls.clone(),
        ));
        let error = none.follow(true, true).unwrap_err().to_string();
        assert!(
            error.contains("unavailable") && error.contains("xone"),
            "{error}"
        );
        assert!(!none.holding());
        drop(none);
        assert_eq!(*calls.lock().unwrap(), ["pw-dump"]);
        assert!(headset_card(
            " 3 [Headset ]: xone-gip-headse - Microsoft Xbox Headset\n"
        ));
        assert!(!headset_card(
            " 0 [Chroma ]: USB-Audio - Razer Nommo Chroma\n"
        ));
    }

    /// v7 ticket 129: switching to xpad unloads xone, loads xpad, then writes our blacklist
    /// of xone and sets xone's blacklist of xpad aside; back to xone undoes both. A failed load
    /// leaves every file as it was; xone not installed, or another word, runs nothing.
    #[test]
    fn driver_switch_unloads_loads_then_saves_and_a_failure_changes_no_file() {
        let root =
            std::env::temp_dir().join(format!("vibeconsole-modprobe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (etc, usb) = (root.join("etc"), root.join("usb"));
        std::fs::create_dir_all(&etc).unwrap();
        for dir in ["xone-wired", "xpad"] {
            std::fs::create_dir_all(usb.join(dir)).unwrap();
        }
        for interface in ["1-1:1.0", "1-1:1.1"] {
            std::fs::write(usb.join("xone-wired").join(interface), "").unwrap();
        }
        let wrote = |dir: &str, file: &str| std::fs::read_to_string(usb.join(dir).join(file)).ok();
        std::fs::write(etc.join(XONE_FILE), "blacklist xpad\nblacklist mt76x2u\n").unwrap();
        let loaded = "xone_gip_headset 28672 3 - Live\nxone_gip_gamepad 20480 0 - Live\nxone_wired 24576 0 - Live\nxone_gip 81920 3 xone_gip_gamepad - Live\nsnd 1 0 - Live\n";
        let mut ran = Vec::new();
        let mut run = |args: &[&str]| -> crate::Result<String> {
            ran.push(args.join(" "));
            Ok(String::new())
        };
        let done = switch_driver("xpad", &etc, &usb, loaded, true, &mut run).unwrap();
        assert_eq!(done, "Xbox driver switched to xpad");
        // Unbound from xone first, then xpad loaded and bound, then xone unloaded.
        assert_eq!(wrote("xone-wired", "unbind").as_deref(), Some("1-1:1.1"));
        assert_eq!(wrote("xpad", "bind").as_deref(), Some("1-1:1.1"));
        assert_eq!(
            ran,
            [
                "modprobe xpad",
                "modprobe -r -a xone_gip_gamepad xone_gip_headset xone_wired xone_gip"
            ]
        );
        assert!(
            std::fs::read_to_string(etc.join(DRIVER_FILE))
                .unwrap()
                .contains("blacklist xone_wired")
        );
        assert!(!etc.join(XONE_FILE).exists() && etc.join(XONE_OFF).exists());

        // A module still in use (the headset card held open) stays loaded; the switch holds.
        let mut busy = |args: &[&str]| -> crate::Result<String> {
            if args[1] == "-r" {
                Err("modprobe: WARNING: Module xone_gip_headset is in use.".into())
            } else {
                Ok(String::new())
            }
        };
        let done = switch_driver("xpad", &etc, &usb, loaded, true, &mut busy).unwrap();
        assert!(done.contains("stay loaded until a reboot"), "{done}");

        // Back to xone undoes both files.
        std::fs::remove_dir_all(usb.join("xone-wired")).unwrap();
        std::fs::create_dir_all(usb.join("xone-wired")).unwrap();
        for interface in ["1-1:1.0", "1-1:1.1"] {
            std::fs::write(usb.join("xpad").join(interface), "").unwrap();
        }
        ran.clear();
        let mut run = |args: &[&str]| -> crate::Result<String> {
            ran.push(args.join(" "));
            Ok(String::new())
        };
        switch_driver("xone", &etc, &usb, "xpad 61440 0 - Live\n", true, &mut run).unwrap();
        assert_eq!(ran, ["modprobe xone_wired", "modprobe -r -a xpad"]);
        assert_eq!(wrote("xone-wired", "bind").as_deref(), Some("1-1:1.1"));
        assert!(!etc.join(DRIVER_FILE).exists());
        assert!(etc.join(XONE_FILE).exists() && !etc.join(XONE_OFF).exists());

        // A failed load binds the controller back and saves nothing.
        let mut failing = |args: &[&str]| -> crate::Result<String> {
            if args == ["modprobe", "xone_wired"] {
                Err("modprobe: FATAL: Module xone_wired not found".into())
            } else {
                Ok(String::new())
            }
        };
        std::fs::remove_file(usb.join("xpad/bind")).ok();
        std::fs::write(etc.join(DRIVER_FILE), "kept\n").unwrap();
        let error = switch_driver("xone", &etc, &usb, "xpad 1 0 - Live\n", true, &mut failing)
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("xbox-driver: load:")
                && error.ends_with("the controller is back on xpad"),
            "{error}"
        );
        assert_eq!(wrote("xpad", "bind").as_deref(), Some("1-1:1.1"));
        assert_eq!(
            std::fs::read_to_string(etc.join(DRIVER_FILE)).unwrap(),
            "kept\n"
        );

        let mut never = |_: &[&str]| -> crate::Result<String> { panic!("nothing may run") };
        assert!(
            switch_driver("xone", &etc, &usb, "", false, &mut never)
                .unwrap_err()
                .to_string()
                .contains("xone isn't installed")
        );
        assert!(switch_driver("rm -rf", &etc, &usb, "", true, &mut never).is_err());
        assert_eq!(
            headset_card_id(
                " 0 [Chroma         ]: USB-Audio - Razer Nommo Chroma\n 3 [Headset        ]: xone-gip-headse - Microsoft Xbox Headset\n"
            ),
            Some("Headset".into())
        );
        assert_eq!(
            headset_card_id(" 3 [Head;set ]: xone-gip-headse - Microsoft Xbox Headset\n"),
            None
        );
        assert_eq!(driver_in_use(&["xone-wired"]), Some("xone"));
        assert_eq!(driver_in_use(&["xpad"]), Some("xpad"));
        assert_eq!(driver_in_use(&[]), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// v7 ticket 130: the light rule is VibeConsole's own file with fixed text; install and
    /// remove reload udev and replay the leds events; a bad word runs nothing.
    #[test]
    fn light_rule_writes_only_its_own_file_and_reloads_udev() {
        let rules = std::env::temp_dir().join(format!("vibeconsole-rules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&rules);
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("70-snap.snapd.rules"), "other\n").unwrap();
        let mut ran = Vec::new();
        let mut run = |args: &[&str]| -> crate::Result<String> {
            ran.push(args.join(" "));
            Ok(String::new())
        };
        light_rule("install", &rules, &mut run).unwrap();
        let text = std::fs::read_to_string(rules.join(LIGHT_RULE_FILE)).unwrap();
        assert!(text.contains(r#"KERNEL=="gip*""#) && text.contains("chgrp input /sys%p/mode"));
        light_rule("remove", &rules, &mut run).unwrap();
        assert!(!rules.join(LIGHT_RULE_FILE).exists());
        assert_eq!(
            std::fs::read_to_string(rules.join("70-snap.snapd.rules")).unwrap(),
            "other\n"
        );
        assert_eq!(
            ran,
            [
                "udevadm control --reload",
                "udevadm trigger --subsystem-match=leds --action=add",
                "udevadm control --reload",
                "udevadm trigger --subsystem-match=leds --action=add"
            ]
        );
        let mut never = |_: &[&str]| -> crate::Result<String> { panic!("nothing may run") };
        assert!(light_rule("chmod", &rules, &mut never).is_err());
        std::fs::remove_dir_all(&rules).unwrap();
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

    /// v7 ticket 86: the button light shows each agent pattern as xone's GIP mode (0 off,
    /// 1 steady, 2 fast blink, 3 blink, 8 fade for the slow pulse) and is restored on quit.
    #[test]
    fn button_light_shows_agent_patterns_and_is_restored_on_quit() {
        use crate::status::Light as Pattern;
        let root = leds("agents", "3");
        let mut light = ButtonLight::open(&root).unwrap();
        for (pattern, gip) in [
            (Pattern::FastBlink, "2"),
            (Pattern::SlowPulse, "8"),
            (Pattern::Blink, "3"),
            (Pattern::Steady, "1"),
            (Pattern::Off, "0"),
        ] {
            light.show(Light::Agent(pattern)).unwrap();
            assert_eq!(mode(&root, "gip0"), gip, "{pattern:?}");
        }
        light.show(Light::Paused).unwrap();
        assert_eq!(mode(&root, "gip0"), "4", "paused still shows paused");
        drop(light);
        assert_eq!(mode(&root, "gip0"), "3", "quit puts the old mode back");
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

    /// v7 ticket 76: a stick path as (milliseconds, degrees, push 0–1) points, fed to the
    /// detector with a tick at each; returns every control pressed, in order.
    fn stroke(
        gestures: &mut Gestures,
        tuning: &crate::mappings::Tuning,
        points: &[(u64, f32, f32)],
    ) -> Vec<Control> {
        let mut down = HashSet::new();
        let mut pressed = Vec::new();
        for &(at, degrees, push) in points {
            let (x, y) = (
                degrees.to_radians().sin() * push,
                degrees.to_radians().cos() * push,
            );
            let mut changes = gestures.motion(1, (x, y), tuning, &mut down, at);
            changes.extend(gestures.tick(tuning, &mut down, at));
            for change in changes {
                if let Change::Event(Event::Press(control)) = change {
                    pressed.push(control);
                }
            }
        }
        assert!(down.is_empty(), "nothing left held after rest: {down:?}");
        pressed
    }
    /// A full-push sweep from `from` through `degrees` (negative: counterclockwise) over
    /// `ms`, sampled every 20 ms, then back to rest.
    fn sweep(from: f32, degrees: f32, ms: u64) -> Vec<(u64, f32, f32)> {
        let steps = ms / 20;
        let mut points = (0..=steps)
            .map(|i| {
                let at = (from + degrees * i as f32 / steps as f32).rem_euclid(360.0);
                (i * 20, at, 1.0)
            })
            .collect::<Vec<_>>();
        points.push((ms + 20, 0.0, 0.0));
        points
    }
    fn right_stick_gestures(gestures: &[Gesture]) -> (Gestures, crate::mappings::Tuning) {
        let mut tuning = crate::mappings::Tuning::default();
        tuning.sticks[1] = crate::mappings::StickMode::sectors();
        let mappings = gestures
            .iter()
            .map(|g| {
                let mut m = Mapping::new(g.control(1), "F1", "hold").unwrap();
                m.context = GAMEPAD_CONTEXT;
                m
            })
            .collect::<Vec<_>>();
        let mut detector = Gestures::default();
        let mut analog = Analog::default();
        detector.set(&mappings, &mut analog);
        assert_eq!(analog.gestures, [false, true]);
        (detector, tuning)
    }

    #[test]
    fn gesture_controls_are_named_and_valid_xbox_controls() {
        for side in 0..2 {
            for gesture in [Gesture::Turn(5), Gesture::Flick(0), Gesture::Hold(15)] {
                let control = gesture.control(side);
                assert_eq!(Gesture::of(control.id), Some((side, gesture)));
                assert!(control.validate().is_ok(), "{control:?}");
                assert_eq!(stick_of(control), Some(side));
            }
        }
        assert_eq!(
            Gesture::Turn(2).control(1).label(),
            "Xbox Right stick half circle clockwise"
        );
        assert_eq!(
            Gesture::Flick(2).control(0).label(),
            "Xbox Left stick flick into range 3"
        );
        assert_eq!(
            Gesture::Hold(0).control(1).label(),
            "Xbox Right stick hold in range 1"
        );
        assert_eq!(Gesture::Hold(15).control(1).id as usize, CONTROLS - 1);
        assert!(Control::button(CONTROLS as u8).validate().is_err());
        // Angle ranges keep their ids and names.
        assert_eq!(button_name(ANGLE_BASE + 17), "Right stick angle range 2");
    }

    #[test]
    fn stick_gestures_fire_once_when_completed_and_never_the_ranges_they_pass() {
        let turns = [0, 1, 2, 3, 4, 5].map(Gesture::Turn);
        let (mut g, tuning) = right_stick_gestures(&turns);
        // 180° clockwise: the half circle, once; not the quarter it passed, not a range.
        assert_eq!(
            stroke(&mut g, &tuning, &sweep(0.0, 180.0, 400)),
            [Gesture::Turn(2).control(1)]
        );
        // 360° counterclockwise from the right: the full circle.
        assert_eq!(
            stroke(&mut g, &tuning, &sweep(90.0, -360.0, 800)),
            [Gesture::Turn(5).control(1)]
        );
        // A 90° turn.
        assert_eq!(
            stroke(&mut g, &tuning, &sweep(270.0, 90.0, 250)),
            [Gesture::Turn(0).control(1)]
        );

        // A flick into range 3 (90°–135°) and back, and a rest in range 5 for the hold time.
        let (mut g, tuning) = right_stick_gestures(&[Gesture::Flick(2), Gesture::Hold(4)]);
        let flick = [
            (0, 100.0, 1.0),
            (60, 100.0, 1.0),
            (150, 100.0, 1.0),
            (200, 0.0, 0.0),
        ];
        assert_eq!(
            stroke(&mut g, &tuning, &flick),
            [Gesture::Flick(2).control(1)]
        );
        let mut hold = (0..=45).map(|i| (i * 20, 190.0, 1.0)).collect::<Vec<_>>();
        hold.push((920, 0.0, 0.0));
        assert_eq!(
            stroke(&mut g, &tuning, &hold),
            [Gesture::Hold(4).control(1)],
            "once, not again on release"
        );
    }

    #[test]
    fn slow_short_or_interrupted_sweeps_fire_no_gesture() {
        let turns = [0, 1, 2, 3, 4, 5].map(Gesture::Turn);
        let (mut g, tuning) = right_stick_gestures(&turns);
        let gesture = |c: &Control| Gesture::of(c.id).is_some();
        // Too slow: three seconds for a half circle is pointing, not a turn.
        let slow = stroke(&mut g, &tuning, &sweep(0.0, 180.0, 3000));
        assert!(!slow.iter().any(gesture), "{slow:?}");
        // Stops short of the only turn mapped: nothing at all, not even a range.
        let (mut g, tuning) = right_stick_gestures(&[Gesture::Turn(2)]);
        assert_eq!(stroke(&mut g, &tuning, &sweep(0.0, 120.0, 300)), []);
        // Cut by a transition half-way: nothing, now or when it comes back to rest.
        let path = sweep(0.0, 180.0, 400);
        let (first, rest) = path.split_at(path.len() / 2);
        assert_eq!(stroke(&mut g, &tuning, first), []);
        g.cancel();
        assert_eq!(stroke(&mut g, &tuning, rest), []);
        // The next stroke works again.
        assert_eq!(
            stroke(&mut g, &tuning, &sweep(0.0, 180.0, 400)),
            [Gesture::Turn(2).control(1)]
        );
    }

    #[test]
    fn a_stick_with_gestures_still_reaches_its_ranges() {
        let (mut g, tuning) = right_stick_gestures(&[Gesture::Turn(2)]);
        let range = |r: u8| Control::button(ANGLE_BASE + MAX_RANGES + r);
        // A quick push that isn't a gesture taps its range once.
        let quick = [(0, 100.0, 1.0), (100, 100.0, 1.0), (140, 0.0, 0.0)];
        assert_eq!(stroke(&mut g, &tuning, &quick), [range(2)]);
        // Resting holds the range; moving on (no turn) presses the next, like plain angle mode.
        let mut rest = (0..=20).map(|i| (i * 20, 100.0, 1.0)).collect::<Vec<_>>();
        rest.extend([(420, 150.0, 1.0), (440, 150.0, 1.0), (460, 0.0, 0.0)]);
        assert_eq!(stroke(&mut g, &tuning, &rest), [range(2), range(3)]);
        // Inside the dead zone nothing starts.
        assert_eq!(
            stroke(
                &mut g,
                &tuning,
                &[(0, 90.0, 0.1), (300, 90.0, 0.1), (320, 0.0, 0.0)]
            ),
            []
        );
    }

    /// v7 ticket 115: View or Menu held + LB/LT/RB/RT picks an Xbox program and fires
    /// nothing; View or Menu tapped alone fires its own mapping on release.
    #[test]
    fn view_and_menu_pick_xbox_programs_and_a_tap_fires_on_release() {
        let b = |button| control(button).unwrap();
        let (view, menu) = (b(gilrs::Button::Select), b(gilrs::Button::Start));
        let (lb, rt) = (
            b(gilrs::Button::LeftTrigger),
            b(gilrs::Button::RightTrigger2),
        );
        let up = |c| Event::Release(c, ReleaseType::ButtonUp);
        let mut keys = ProgramKeys::default();
        assert_eq!(keys.event(&Event::Press(view)), ProgramStep::Swallow);
        assert_eq!(keys.event(&Event::Press(rt)), ProgramStep::Select(4));
        assert_eq!(
            keys.event(&up(rt)),
            ProgramStep::Pass,
            "the keyboard never saw its press"
        );
        assert_eq!(
            keys.event(&up(view)),
            ProgramStep::Swallow,
            "a combo fires nothing"
        );
        assert_eq!(keys.event(&Event::Press(menu)), ProgramStep::Swallow);
        assert_eq!(keys.event(&Event::Press(lb)), ProgramStep::Select(5));
        assert_eq!(keys.event(&up(menu)), ProgramStep::Swallow);
        // A tap of View alone fires View, on release.
        assert_eq!(keys.event(&Event::Press(view)), ProgramStep::Swallow);
        assert_eq!(keys.event(&up(view)), ProgramStep::Tap(view));
        // LB alone is LB (the layer key), and a View hold cut by a transition fires nothing.
        assert_eq!(keys.event(&Event::Press(lb)), ProgramStep::Pass);
        keys.event(&Event::Press(view));
        keys.cancel();
        assert_eq!(
            keys.event(&Event::UnmatchedRelease(view)),
            ProgramStep::Swallow
        );
        // Program 1 keeps the contexts it always had; programs 2–8 have their own.
        use crate::mappings::{GAMEPAD_CONTEXT, GAMEPAD_LAYER_2, xbox_context, xbox_program};
        assert_eq!(
            (xbox_context(1, false), xbox_context(1, true)),
            (GAMEPAD_CONTEXT, GAMEPAD_LAYER_2)
        );
        for program in 1..=8 {
            for layer2 in [false, true] {
                assert_eq!(
                    xbox_program(xbox_context(program, layer2)),
                    Some((program, layer2))
                );
            }
        }
        assert_eq!(xbox_program(5), None);
    }

    /// v7 ticket 115: the built-in defaults apply in every Xbox program; a saved button
    /// replaces its default only in its own program.
    #[test]
    fn each_xbox_program_has_its_own_layout() {
        let a = control(gilrs::Button::South).unwrap();
        let mut f9 = Mapping::new(a, "F9", "hold").unwrap();
        f9.context = crate::mappings::xbox_context(3, false);
        let saved = [f9];
        let keys = |program| {
            layout_in(&saved, program)
                .into_iter()
                .find(|m| m.control == a)
                .unwrap()
                .keys
        };
        assert_eq!(keys(1), ["Enter"]);
        assert_eq!(keys(3), ["F9"]);
        assert_eq!(keys(4), ["Enter"]);
        assert!(
            layout_in(&saved, 2)
                .iter()
                .all(|m| m.context == crate::mappings::xbox_context(2, false))
        );
    }
}
