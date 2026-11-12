use crate::feedback::{Feedback, Octave, Settings};
use crate::{Control, DEVICE, Result};
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const NAMED_KEYS: &[&str] = &[
    "Ctrl",
    "Shift",
    "Alt",
    "Meta",
    "RightCtrl",
    "RightShift",
    "RightAlt",
    "RightMeta",
    "Escape",
    "Enter",
    "Tab",
    "Space",
    "Backspace",
    "Delete",
    "Insert",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Up",
    "Down",
    "Left",
    "Right",
    "CapsLock",
    "Minus",
    "Equal",
    "LeftBracket",
    "RightBracket",
    "Backslash",
    "Semicolon",
    "Apostrophe",
    "Grave",
    "Comma",
    "Period",
    "Slash",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Behavior {
    Hold,
    Toggle,
    Pulse,
    Trigger,
    /// v7 ticket 96 (config v21): each press sends the shortcut once (keys down, then up),
    /// however long it's held; nothing on release.
    Tap,
}

impl fmt::Display for Behavior {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Hold => "hold",
            Self::Toggle => "toggle",
            Self::Pulse => "pulse",
            Self::Trigger => "trigger",
            Self::Tap => "tap",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Input {
    #[default]
    Pad,
    Piano,
    Knob {
        step: u8,
    },
    RelativeKnob {
        step: u8,
    },
    Joystick {
        center: u16,
        min: u16,
        max: u16,
        deadzone: u16,
        hysteresis: u16,
    },
}
impl Input {
    pub fn description(self) -> String {
        match self {
            Self::Pad => "Pad Note press/release".into(),
            Self::Piano => "Piano emitted note".into(),
            Self::Knob { step } => format!("Absolute knob, step {step}"),
            Self::RelativeKnob { step } => format!("Relative knob, step {step}"),
            Self::Joystick {
                center,
                deadzone,
                hysteresis,
                ..
            } => format!("Joystick neutral {center}, region {deadzone}, hysteresis {hysteresis}"),
        }
    }
}
impl fmt::Display for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pad => f.write_str("pad"),
            Self::Piano => f.write_str("piano"),
            Self::Knob { step } => write!(f, "knob:{step}"),
            Self::RelativeKnob { step } => write!(f, "relative-knob:{step}"),
            Self::Joystick {
                center,
                min,
                max,
                deadzone,
                hysteresis,
            } => write!(f, "joystick:{center}:{min}:{max}:{deadzone}:{hysteresis}"),
        }
    }
}
impl std::str::FromStr for Input {
    type Err = Box<dyn std::error::Error>;
    fn from_str(value: &str) -> Result<Self> {
        let fields = value.split(':').collect::<Vec<_>>();
        Ok(match fields.as_slice() {
            ["pad"] => Self::Pad,
            ["piano"] => Self::Piano,
            ["knob", step] => Self::Knob {
                step: step.parse()?,
            },
            ["relative-knob", step] => Self::RelativeKnob {
                step: step.parse()?,
            },
            ["joystick", center, min, max, deadzone, hysteresis] => Self::Joystick {
                center: center.parse()?,
                min: min.parse()?,
                max: max.parse()?,
                deadzone: deadzone.parse()?,
                hysteresis: hysteresis.parse()?,
            },
            _ => return Err("invalid input profile".into()),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mapping {
    pub control: Control,
    pub input: Input,
    pub context: u8,
    pub label: String,
    pub action: crate::actions::Action,
    pub keys: Vec<String>,
    pub behavior: Behavior,
    pub feedback: Feedback,
    /// Fires only after the control is held for `keyboard::ARM`.
    pub risky: bool,
    /// Show sent/running/stopped/failed in the live pane (observable actions only).
    pub status: bool,
}

/// Other MIDI devices (v7) use contexts from here up, one per listed device, so their
/// mappings never mix with the Akai's Programs 1–8 or legacy contexts.
pub const DEVICE_CONTEXT: u8 = 240;
/// Xbox controller mappings (v7 ticket 07): one set, active beside whichever MIDI context.
pub const GAMEPAD_CONTEXT: u8 = 239;
/// Xbox layer 2 (v7 ticket 09): what each button does while LB is held.
pub const GAMEPAD_LAYER_2: u8 = 238;
pub fn gamepad_context(context: u8) -> bool {
    matches!(context, GAMEPAD_CONTEXT | GAMEPAD_LAYER_2)
}
pub fn program_label(context: u8) -> String {
    if context == GAMEPAD_CONTEXT {
        "Xbox controller".into()
    } else if context == GAMEPAD_LAYER_2 {
        "Xbox controller, layer 2 (LB held)".into()
    } else if context >= DEVICE_CONTEXT {
        format!("Other MIDI device {}", context - DEVICE_CONTEXT + 1)
    } else if context < 8 {
        format!("Program {}", context + 1)
    } else {
        format!("Legacy context {context} (not one of Programs 1–8)")
    }
}

/// Pads 1–8 of Bank A then Bank B, from a stored program payload (verified 2026-10-07 captures).
pub fn pad_controls(payload: &crate::feedback::Payload) -> Vec<Option<Control>> {
    (0..16)
        .map(|k| Control::note(payload[0x10] + 1, payload[0x24 + 3 * k]))
        .collect()
}

/// Physical position of a control in the verified program, or its raw identity with the reason.
/// Display only: saved identities are unchanged. `program` is the verified (context, payload).
pub fn physical_name(
    control: Control,
    input: Option<Input>,
    context: u8,
    program: Option<&(u8, crate::feedback::Payload)>,
) -> std::result::Result<String, String> {
    let side = match control.direction {
        1 => " increase",
        -1 => " decrease",
        _ => "",
    };
    let raw = match control.message {
        crate::Message::Note => format!("Note ch{} #{}", control.channel, control.id),
        crate::Message::Cc => format!("CC ch{} #{}{side}", control.channel, control.id),
        crate::Message::Bend => return Ok(format!("Joystick X{side}")),
        crate::Message::Button => return Ok(control.label()),
    };
    if input == Some(Input::Piano) {
        const NOTES: [&str; 12] = [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ];
        let (id, octave) = (control.id as usize, control.id as i32 / 12 - 1);
        return Ok(format!("Piano {}{octave} (note {id})", NOTES[id % 12]));
    }
    let Some((_, payload)) = program.filter(|(slot, _)| *slot == context) else {
        return Err(format!("{raw} ({} unverified)", program_label(context)));
    };
    let program = program_label(context);
    let (hits, kind): (Vec<usize>, _) = if control.message == crate::Message::Note {
        let pads = pad_controls(payload);
        let hits = (0..16).filter(|k| pads[*k] == Some(control)).collect();
        (hits, "pad")
    } else {
        // ponytail: knob entries hold no channel, so match the CC on any channel; add the channel once its payload byte is verified.
        let hits = (0..8)
            .filter(|k| payload[0x55 + 20 * k] == control.id)
            .collect();
        (hits, "knob")
    };
    match hits[..] {
        [k] if kind == "pad" => Ok(format!(
            "Bank {} Pad {}",
            if k < 8 { 'A' } else { 'B' },
            k % 8 + 1
        )),
        [k] => Ok(format!("Knob {}{side}", k + 1)),
        [] => Err(format!("{raw} (not on a {kind} in {program})")),
        _ => Err(format!("{raw} (on several {kind}s in {program})")),
    }
}

pub fn control_name(
    control: Control,
    input: Option<Input>,
    context: u8,
    program: Option<&(u8, crate::feedback::Payload)>,
) -> String {
    physical_name(control, input, context, program).unwrap_or_else(|raw| raw)
}

impl Mapping {
    /// Joysticks keep their observed axis/side label; other controls are named by position.
    pub fn name(&self, program: Option<&(u8, crate::feedback::Payload)>) -> String {
        if matches!(self.input, Input::Joystick { .. }) {
            return self.label.clone();
        }
        control_name(self.control, Some(self.input), self.context, program)
    }

    pub fn new(pad: Control, shortcut: &str, behavior: &str) -> Result<Self> {
        pad.validate()?;
        let behavior = match behavior.trim().to_ascii_lowercase().as_str() {
            "hold" => Behavior::Hold,
            "toggle" => Behavior::Toggle,
            "pulse" => Behavior::Pulse,
            "tap" => Behavior::Tap,
            _ => return Err("behavior must be hold, toggle, tap, or knob pulse".into()),
        };
        Ok(Self {
            control: pad,
            input: Input::Pad,
            // v7 ticket 42: an Xbox button starts in the Xbox context (layer 1) so it validates
            // like one; the caller may move it to layer 2. MIDI controls start in context 0.
            context: if pad.message == crate::Message::Button {
                GAMEPAD_CONTEXT
            } else {
                0
            },
            label: pad.label(),
            action: crate::actions::Action::Shortcut,
            keys: parse_shortcut(shortcut)?,
            behavior,
            feedback: Feedback::default(),
            risky: false,
            status: false,
        })
    }
    pub fn new_action(
        control: Control,
        input: Input,
        action: crate::actions::Action,
    ) -> Result<Self> {
        let mut mapping = Self::new(control, "Shift", "hold")?;
        mapping.input = input;
        mapping.action = action;
        mapping.keys.clear();
        mapping.behavior = Behavior::Trigger;
        mapping.validate()?;
        Ok(mapping)
    }
    pub fn action_label(&self) -> String {
        if self.action == crate::actions::Action::Shortcut {
            self.keys.join("+")
        } else {
            self.action.description()
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.control.validate()?;
        self.action.validate()?;
        if self.action == crate::actions::Action::Shortcut {
            if self.behavior == Behavior::Trigger
                || parse_shortcut(&self.keys.join("+"))? != self.keys
            {
                return Err("invalid keyboard shortcut behavior/keys".into());
            }
        } else if self.behavior != Behavior::Trigger
            || !self.keys.is_empty()
            || self.feedback.enabled()
        {
            return Err(
                "application/system actions require Trigger, empty keys, and no feedback".into(),
            );
        }
        if (self.context > 16 && self.context < GAMEPAD_LAYER_2)
            || gamepad_context(self.context) != (self.control.message == crate::Message::Button)
            || (gamepad_context(self.context) && crate::gamepad::is_reserved(self.control))
            || self.label.is_empty()
            || self.label.len() > 80
            || self.label.chars().any(char::is_control)
        {
            return Err("invalid mapping context or label".into());
        }
        use crate::Message;
        let valid = match self.input {
            Input::Pad => {
                matches!(self.control.message, Message::Note | Message::Button)
                    && self.behavior != Behavior::Pulse
            }
            Input::Piano => {
                self.control.message == Message::Note && self.behavior != Behavior::Pulse
            }
            Input::Knob { step } | Input::RelativeKnob { step } => {
                self.control.message == Message::Cc
                    && (1..=if matches!(self.input, Input::RelativeKnob { .. }) {
                        8
                    } else {
                        127
                    })
                        .contains(&step)
                    && (self.behavior == Behavior::Pulse
                        || (self.behavior == Behavior::Trigger
                            && (self.action.audio()
                                || (self.action.dial()
                                    && matches!(self.input, Input::Knob { .. })))))
                    && !self.feedback.enabled()
            }
            Input::Joystick {
                center,
                min,
                max,
                deadzone,
                hysteresis,
            } => {
                let limit = if self.control.message == Message::Bend {
                    16383
                } else {
                    127
                };
                let reach = if self.control.direction > 0 {
                    max.saturating_sub(center)
                } else {
                    center.saturating_sub(min)
                };
                self.control.message != Message::Note
                    && self.behavior == Behavior::Hold
                    && !self.feedback.enabled()
                    && min <= center
                    && center <= max
                    && max <= limit
                    && hysteresis < deadzone
                    && deadzone
                        .checked_add(hysteresis)
                        .is_some_and(|threshold| threshold < reach)
            }
        };
        if self.action.dial() && !matches!(self.input, Input::Knob { .. }) {
            return Err("choice and value actions need an absolute knob".into());
        }
        if self.status && !self.action.observable() {
            return Err("status feedback needs an action with observable state".into());
        }
        if self.risky && !matches!(self.input, Input::Pad | Input::Piano) {
            return Err("only pad and piano presses can be risky".into());
        }
        if !valid || ((self.input != Input::Pad || self.context > 7) && self.feedback.enabled()) {
            return Err("unsupported control/profile/behavior/feedback combination".into());
        }
        self.feedback.validate()
    }
}

impl fmt::Display for Mapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} | {} | USB {DEVICE} | {} channel {} id {} direction {} | {} | {} | {} | Feedback: {}",
            program_label(self.context),
            self.label,
            self.control.message.text(),
            self.control.channel,
            self.control.id,
            self.control.direction,
            self.action_label(),
            self.behavior,
            self.input.description(),
            self.feedback
        )
    }
}

pub fn parse_shortcut(shortcut: &str) -> Result<Vec<String>> {
    let mut keys = Vec::new();
    for name in shortcut.split('+') {
        let name = name.trim().to_ascii_lowercase();
        // ponytail: named keys only; extend this subset when another physical key is needed.
        let alias = match name.as_str() {
            "control" | "leftctrl" => "Ctrl",
            "leftshift" => "Shift",
            "leftalt" => "Alt",
            "super" | "win" | "leftmeta" => "Meta",
            "altgr" => "RightAlt",
            "esc" => "Escape",
            "return" => "Enter",
            "del" => "Delete",
            "ins" => "Insert",
            _ => &name,
        };
        let key = if let Some(key) = NAMED_KEYS
            .iter()
            .find(|key| key.eq_ignore_ascii_case(alias))
        {
            (*key).to_owned()
        } else if (name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric())
            || (1..=12).any(|number| name == format!("f{number}"))
        {
            name.to_ascii_uppercase()
        } else {
            return Err(format!(
                "unknown or empty key name: {name:?}; see README.md for supported keys"
            )
            .into());
        };
        crate::keyboard::key_code(&key)?;
        if keys.contains(&key) {
            return Err(format!("duplicate key in shortcut: {key}").into());
        }
        keys.push(key);
    }
    keys.sort_by_key(|key| {
        !matches!(
            key.as_str(),
            "Ctrl"
                | "Shift"
                | "Alt"
                | "Meta"
                | "RightCtrl"
                | "RightShift"
                | "RightAlt"
                | "RightMeta"
        )
    });
    Ok(keys)
}

pub fn key_groups() -> Vec<(&'static str, Vec<String>)> {
    vec![
        (
            "Modifiers",
            NAMED_KEYS[..8].iter().map(|s| s.to_string()).collect(),
        ),
        ("Letters", ('A'..='Z').map(|c| c.to_string()).collect()),
        ("Numbers", (0..=9).map(|n| n.to_string()).collect()),
        ("Function keys", (1..=12).map(|n| format!("F{n}")).collect()),
        (
            "Editing",
            NAMED_KEYS[8..15]
                .iter()
                .chain(NAMED_KEYS[23..24].iter())
                .map(|s| s.to_string())
                .collect(),
        ),
        (
            "Navigation",
            NAMED_KEYS[15..23].iter().map(|s| s.to_string()).collect(),
        ),
        (
            "Punctuation",
            NAMED_KEYS[24..].iter().map(|s| s.to_string()).collect(),
        ),
    ]
}

pub fn config_path() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => {
            PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unset; set XDG_CONFIG_HOME")?)
                .join(".config")
        }
    };
    if !base.is_absolute() {
        return Err("configuration directory must be absolute (XDG_CONFIG_HOME or HOME)".into());
    }
    let path = base.join("vibeconsole/mappings.tsv");
    // Copy a pre-rename KeyAI configuration once; the old file stays as a backup.
    let old = base.join("keyai/mappings.tsv");
    if !path.exists() && old.exists() {
        std::fs::create_dir_all(base.join("vibeconsole"))?;
        std::fs::copy(&old, &path)?;
    }
    Ok(path)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub mappings: Vec<Mapping>,
    pub agents: Vec<Agent>,
    pub idle: Settings,
    pub program_idle: std::collections::BTreeMap<u8, Settings>,
    pub start_flow: bool,
    /// Default working directory for agents (Settings); `${NAME}` allowed.
    pub agent_dir: Option<String>,
    /// Startup variables: exported into every tmux session and command VibeConsole starts.
    pub env: Vec<(String, String)>,
    /// User variables: expanded as `${NAME}` in command and agent directories, and exported
    /// (with the startup variables) into commands, whose shell expands them.
    pub vars: Vec<(String, String)>,
    /// Status lights on the controller follow agent status (ticket 14); default On.
    pub status_lights: bool,
    /// Ticket 16: sessions VibeConsole did not start light by folder (absolute, most specific
    /// wins); others use `light_other` when `light_other_on`.
    pub light_folders: Vec<(String, i8)>,
    pub light_other: i8,
    pub light_other_on: bool,
    /// Ticket 17: what each busy state shows on the tempo and octave lights.
    pub light_patterns: crate::status::Patterns,
    /// v7: gamepad stick/trigger tuning, mouse speed and rumble (config v16).
    pub tuning: Tuning,
    /// v7 ticket 86 (config v20): each device's status-light switch and patterns.
    pub device_lights: crate::status::DevicePatterns,
    /// v7: other MIDI devices by device name; the n-th uses context `DEVICE_CONTEXT + n`.
    pub midi_devices: Vec<String>,
    /// The MIDI device the session uses: None is the Akai.
    pub midi_active: Option<String>,
    /// v7 ticket 12: per-app overrides; an app with no profile uses Default.
    pub profiles: Vec<Profile>,
    /// v7 ticket 13: a pinned profile (or Default) that ignores focus changes.
    pub lock: Option<Lock>,
    /// v7 ticket 24: favourite mappings and profiles (`favourite_key`), listed first.
    pub favourites: Vec<String>,
}
/// A mapping's favourite key: its context and control, which stay the same when its
/// action is edited.
pub fn favourite_key(mapping: &Mapping) -> String {
    let c = mapping.control;
    format!(
        "{}:{}:{}:{}:{}",
        mapping.context,
        c.message.text(),
        c.channel,
        c.id,
        c.direction
    )
}
pub fn profile_favourite_key(name: &str) -> String {
    format!("profile:{name}")
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Lock {
    Default,
    Profile(String),
}
/// An app profile (v7 ticket 12): while one of its apps is focused, its mappings replace
/// the Default ones for the same controls; every other control keeps its Default meaning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Profile {
    pub name: String,
    /// Desktop IDs, e.g. `firefox.desktop`: identifiers, never commands.
    pub apps: Vec<String>,
    /// Overrides only: pads and Xbox buttons, no feedback.
    pub mappings: Vec<Mapping>,
}
/// A profile name: one printable line, 1–32 characters.
pub fn valid_profile_name(name: &str) -> bool {
    !name.trim().is_empty()
        && !name.trim().eq_ignore_ascii_case("default")
        && name.chars().count() <= 32
        && name != "-"
        && !name.chars().any(char::is_control)
}
/// A desktop ID (`firefox.desktop`, `com.t3tools.T3Code.desktop`): letters, digits, `.`,
/// `-` and `_` only, so it can never be a path or a command.
pub fn valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with(['.', '-'])
        && !id.contains("..")
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}
fn validate_profiles(profiles: &[Profile]) -> Result<()> {
    if profiles.len() > 32 {
        return Err("too many app profiles (32 at most)".into());
    }
    let mut apps = HashSet::new();
    for (i, profile) in profiles.iter().enumerate() {
        if !valid_profile_name(&profile.name) {
            return Err(format!("invalid profile name {:?}", profile.name).into());
        }
        if profiles[..i].iter().any(|p| p.name == profile.name) {
            return Err(format!("profile {} exists twice", profile.name).into());
        }
        for app in &profile.apps {
            if !valid_app_id(app) {
                return Err(format!("invalid app ID {app:?} in profile {}", profile.name).into());
            }
            if !apps.insert(app.as_str()) {
                return Err(format!("{app} is in two profiles").into());
            }
        }
        let mut seen = HashSet::new();
        for mapping in &profile.mappings {
            mapping.validate()?;
            if mapping.input != Input::Pad || mapping.feedback.enabled() {
                return Err(format!(
                    "profile {}: overrides are pads and Xbox buttons without feedback",
                    profile.name
                )
                .into());
            }
            if !seen.insert((mapping.context, mapping.control)) {
                return Err(format!("profile {}: a control is set twice", profile.name).into());
            }
        }
    }
    Ok(())
}
/// A profile file (v7 ticket 14): this header, the profile line, then its overrides.
pub const PROFILE_HEADER: &str = "vibeconsole-profile-v1";
pub fn export_profile(profile: &Profile) -> Result<String> {
    validate_profiles(std::slice::from_ref(profile))?;
    let mut text = format!(
        "{PROFILE_HEADER}\nprofile\t{}\t{}\t{}\n",
        profile.name,
        if profile.apps.is_empty() {
            "-".to_owned()
        } else {
            profile.apps.join(",")
        },
        profile.mappings.len()
    );
    for mapping in &profile.mappings {
        text.push_str(&mapping_line(mapping)?);
    }
    Ok(text)
}
/// Read a profile file, checked in full against `config` (names, app IDs, every override,
/// its devices, and conflicts with existing profiles) before anything can be saved.
pub fn import_profile(text: &str, config: &Config) -> Result<Profile> {
    let mut lines = text.lines();
    if lines.next() != Some(PROFILE_HEADER) {
        return Err("not a VibeConsole profile file".into());
    }
    let line = lines.next().ok_or("missing profile line")?;
    let ["profile", name, apps, count] = line.split('\t').collect::<Vec<_>>()[..] else {
        return Err("invalid profile line".into());
    };
    let count: usize = count.parse()?;
    if count > 512 {
        return Err("too many overrides".into());
    }
    let mut profile = Profile {
        name: name.to_owned(),
        apps: if apps == "-" {
            Vec::new()
        } else {
            apps.split(',').map(str::to_owned).collect()
        },
        mappings: Vec::new(),
    };
    for n in 0..count {
        let line = lines
            .next()
            .ok_or("fewer overrides than the profile line says")?;
        profile.mappings.push(
            parse_mapping_line(line, 16, &config.midi_devices)
                .map_err(|e| format!("override {}: {e}", n + 1))?,
        );
    }
    if lines.next().is_some() {
        return Err("more lines than the profile line says".into());
    }
    if config.profile(&profile.name).is_some() {
        return Err(format!("a profile named {} exists; delete it first", profile.name).into());
    }
    let mut all = config.profiles.clone();
    all.push(profile.clone());
    validate_profiles(&all)?;
    Ok(profile)
}
/// v7 ticket 26: the profile file inside Claude's reply (prose and code fences around it
/// are dropped), or None when there is none. Whatever comes back still goes through
/// `import_profile`, so nothing unchecked is ever saved.
pub fn profile_text_from_reply(reply: &str) -> Option<String> {
    let lines = reply.lines().collect::<Vec<_>>();
    let start = lines.iter().position(|l| l.trim() == PROFILE_HEADER)?;
    let mut text = String::new();
    for line in &lines[start..] {
        if line.trim_start().starts_with("```") {
            break;
        }
        text.push_str(line.trim_end_matches('\r'));
        text.push('\n');
    }
    Some(text.trim_end().to_owned() + "\n")
}
/// The request sent to Claude for a profile: the format by example and the controls the
/// user has, so its overrides land on real pads.
pub fn profile_request(config: &Config, app: &str, wish: &str) -> Result<String> {
    // v7 ticket 104: the example uses mappings a profile may hold (pads and Xbox buttons
    // without feedback); a setup whose first mappings are knobs or lit pads would otherwise
    // fail before Claude is asked. With none, a stand-in pad shows the format.
    let mut overrides = config
        .mappings
        .iter()
        .filter(|m| m.input == Input::Pad && !m.feedback.enabled())
        .take(2)
        .cloned()
        .collect::<Vec<_>>();
    if overrides.is_empty() {
        let pad = Control::from_note(10, 36).ok_or("example pad")?;
        overrides.push(Mapping::new(pad, "Ctrl+T", "hold")?);
    }
    let example = Profile {
        name: "Example".into(),
        apps: vec![app.to_owned()],
        mappings: overrides,
    };
    let controls = config
        .mappings
        .iter()
        .map(|m| format!("- {} (now: {})", m.label, m.action_label()))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "Write one VibeConsole app profile for the app `{app}`. {wish}\n\n         Reply with only the profile file, in exactly this tab-separated format (the first          line is the header, then one profile line whose last field is the number of override          lines, then the override lines, each with exactly the same 16 fields as this example):\n\n         {}\n         Rules: a profile name of 1–32 characters (not Default); overrides only for pads or Xbox          buttons, behaviour hold or toggle, keyboard shortcuts as Key+Key names, no feedback.          The controls this setup has:\n{controls}\n",
        export_profile(&example)?
    ))
}

/// A profile's export file name: letters, digits, `-` and `_` only, so it stays in its
/// folder whatever the profile is called.
pub fn profile_file_name(name: &str) -> String {
    safe_file_name(name, "profile")
}
/// A preset's file name (v7 ticket 25), kept inside its folder like a profile's.
pub fn preset_file_name(name: &str) -> String {
    safe_file_name(name, "preset")
}
/// A program preset's file name (v7 ticket 47): one program's mappings only.
pub fn program_preset_file_name(name: &str) -> String {
    safe_file_name(name, "program-preset")
}
/// v7 ticket 47: a program preset is the mappings of one program (`context`), both banks;
/// nothing else of the setup.
pub fn program_preset(config: &Config, context: u8) -> Config {
    Config {
        mappings: config
            .mappings
            .iter()
            .filter(|m| m.context == context)
            .cloned()
            .collect(),
        ..Config::default()
    }
}
/// v7 ticket 47: load a program preset into the program `context`: that program's mappings
/// are replaced and nothing else changes. A file holding more than one program, Xbox
/// mappings or profiles isn't a program preset and is refused whole.
pub fn apply_program_preset(config: &Config, preset: &Config, context: u8) -> Result<Config> {
    let mut contexts = preset.mappings.iter().map(|m| m.context);
    let first = contexts.next();
    if contexts.any(|c| Some(c) != first)
        || first.is_some_and(gamepad_context)
        || !preset.profiles.is_empty()
    {
        return Err("not a program preset: it holds more than one program's mappings".into());
    }
    let mut next = config.clone();
    next.mappings.retain(|m| m.context != context);
    for mapping in &preset.mappings {
        let mut mapping = mapping.clone();
        mapping.context = context;
        mapping.validate()?;
        next.mappings.push(mapping);
    }
    next.mappings.sort_by_key(|m| (m.context, m.control));
    Ok(next)
}
fn safe_file_name(name: &str, extension: &str) -> String {
    let mut file = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            file.push(c);
        } else if !file.is_empty() && !file.ends_with('-') {
            file.push('-');
        }
    }
    let file = file.trim_end_matches('-');
    format!(
        "{}.{extension}",
        if file.is_empty() { extension } else { file }
    )
}

/// v7 ticket 25: load a saved preset's setup (mappings, app profiles and the MIDI devices
/// they use) into `config`; settings, agents and tuning stay. A lock or active device the
/// preset doesn't have is dropped.
pub fn apply_preset(config: &Config, preset: &Config) -> Config {
    let mut next = config.clone();
    next.mappings = preset.mappings.clone();
    next.profiles = preset.profiles.clone();
    next.midi_devices = preset.midi_devices.clone();
    if next
        .midi_active
        .as_ref()
        .is_some_and(|a| !next.midi_devices.contains(a))
    {
        next.midi_active = None;
    }
    if let Some(Lock::Profile(name)) = &next.lock {
        if next.profile(name).is_none() {
            next.lock = None;
        }
    }
    next
}

/// One context's mappings with a profile's overrides on top (v7 ticket 12).
pub fn overlay(all: &[Mapping], profile: Option<&Profile>, context: u8) -> Vec<Mapping> {
    let overrides = profile
        .map(|p| {
            p.mappings
                .iter()
                .filter(|m| m.context == context)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut set = all
        .iter()
        .filter(|m| m.context == context && !overrides.iter().any(|o| o.control == m.control))
        .cloned()
        .collect::<Vec<_>>();
    set.extend(overrides.into_iter().cloned());
    set.sort_by_key(|m| (m.context, m.control));
    set
}
/// Gamepad tuning (v7 config v16): stick dead zone and trigger fire point in percent of
/// travel, mouse speed 1–50, rumble on/off. Defaults are safe: rumble off.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tuning {
    pub dead_zone: u8,
    pub trigger: u8,
    pub mouse_speed: u8,
    pub rumble: bool,
    /// v7 ticket 22: the stick that moves the pointer, if any.
    pub mouse: MouseStick,
    /// v7 ticket 75 (config v18): left then right stick, each Linear or Angle.
    pub sticks: [StickMode; 2],
}
/// How a stick fires (v7 ticket 75). Linear: up/down/left/right, each on its own axis.
/// Angle: the stick's angle (0° = up, clockwise) picks one of its ranges, each a control.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum StickMode {
    #[default]
    Linear,
    /// `[start, end)` in whole degrees; `start > end` wraps through 0°. At most
    /// [`MAX_RANGES`], never overlapping.
    Angle(Vec<(u16, u16)>),
}
/// Ranges one stick can have in angle mode; each is its own control id.
pub const MAX_RANGES: usize = 16;
impl StickMode {
    /// The default angle mode: 8 sectors of 45°, from straight up clockwise.
    pub fn sectors() -> Self {
        Self::Angle((0..8).map(|k| (k * 45, k * 45 + 45)).collect())
    }
    /// `linear`, or `angle:0-45,45-90,…`.
    pub fn text(&self) -> String {
        match self {
            Self::Linear => "linear".into(),
            Self::Angle(ranges) => {
                let ranges = ranges.iter().map(|(a, b)| format!("{a}-{}", b % 360));
                format!("angle:{}", ranges.collect::<Vec<_>>().join(","))
            }
        }
    }
    pub fn parse(text: &str) -> Result<Self> {
        if text == "linear" {
            return Ok(Self::Linear);
        }
        let ranges = text
            .strip_prefix("angle:")
            .ok_or("stick mode must be linear or angle")?;
        let range = |r: &str| -> Result<(u16, u16)> {
            let (a, b) = r
                .split_once('-')
                .ok_or("a stick range is start-end in degrees")?;
            let end: u16 = b.trim().parse()?;
            Ok((a.trim().parse()?, if end == 0 { 360 } else { end }))
        };
        Ok(Self::Angle(
            ranges.split(',').map(range).collect::<Result<_>>()?,
        ))
    }
    /// The range holding `degrees` (0 ≤ degrees < 360), if any. Core's one lookup: firing,
    /// the hints and Detect all use it.
    pub fn range_at(&self, degrees: f32) -> Option<usize> {
        let Self::Angle(ranges) = self else {
            return None;
        };
        ranges.iter().position(|&(a, b)| {
            let (a, b) = (f32::from(a), f32::from(b));
            if a < b {
                (a..b).contains(&degrees)
            } else {
                degrees >= a || degrees < b
            }
        })
    }
    fn validate(&self, side: &str) -> Result<()> {
        let Self::Angle(ranges) = self else {
            return Ok(());
        };
        if ranges.is_empty() || ranges.len() > MAX_RANGES {
            return Err(format!("tuning: {side} stick needs 1–{MAX_RANGES} angle ranges").into());
        }
        for &(a, b) in ranges {
            if a >= 360 || !(1..=360).contains(&b) || a == b % 360 {
                return Err(format!("tuning: {side} stick range {a}°–{b}° is not 0–360°").into());
            }
        }
        for degree in 0..360u16 {
            let holding = ranges.iter().filter(|&&(a, b)| {
                if a < b {
                    (a..b).contains(&degree)
                } else {
                    degree >= a || degree < b
                }
            });
            if holding.count() > 1 {
                return Err(format!("tuning: {side} stick ranges overlap at {degree}°").into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MouseStick {
    #[default]
    Off,
    Left,
    Right,
}
impl MouseStick {
    pub fn text(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}
impl Default for Tuning {
    fn default() -> Self {
        Self {
            dead_zone: 15,
            trigger: 30,
            mouse_speed: 10,
            rumble: false,
            mouse: MouseStick::Off,
            sticks: Default::default(),
        }
    }
}
impl Tuning {
    pub fn validate(self) -> Result<Self> {
        for (stick, side, mouse) in [
            (0, "left", MouseStick::Left),
            (1, "right", MouseStick::Right),
        ] {
            self.sticks[stick].validate(side)?;
            if self.mouse == mouse && self.sticks[stick] != StickMode::Linear {
                return Err(format!(
                    "tuning: the {side} stick moves the pointer, so it can't be in angle mode"
                )
                .into());
            }
        }
        if self.dead_zone > 90 {
            return Err("tuning: dead zone must be 0–90%".into());
        }
        if !(1..=100).contains(&self.trigger) {
            return Err("tuning: trigger fire point must be 1–100%".into());
        }
        if !(1..=50).contains(&self.mouse_speed) {
            return Err("tuning: mouse speed must be 1–50".into());
        }
        Ok(self)
    }
}
impl Default for Config {
    fn default() -> Self {
        Self {
            mappings: Vec::new(),
            agents: vec![
                Agent::new("claude", "claude", "claude").unwrap(),
                Agent::new("codex", "codex", "codex").unwrap(),
            ],
            idle: Settings::default(),
            program_idle: std::collections::BTreeMap::new(),
            start_flow: true,
            agent_dir: None,
            env: Vec::new(),
            vars: Vec::new(),
            status_lights: true,
            light_folders: Vec::new(),
            light_other: 2,
            light_other_on: false,
            light_patterns: crate::status::Patterns::default(),
            tuning: Tuning::default(),
            device_lights: Default::default(),
            midi_devices: Vec::new(),
            midi_active: None,
            profiles: Vec::new(),
            lock: None,
            favourites: Vec::new(),
        }
    }
}
/// The device column of a mapping line: the Akai's USB id, `midi` or `xbox`.
fn device_field(context: u8) -> &'static str {
    match context {
        GAMEPAD_CONTEXT | GAMEPAD_LAYER_2 => "xbox",
        c if c >= DEVICE_CONTEXT => "midi",
        _ => DEVICE,
    }
}
impl Config {
    pub fn profile_for(&self, app: &str) -> Option<&Profile> {
        self.profiles
            .iter()
            .find(|p| p.apps.iter().any(|a| a == app))
    }
    /// The profile that applies with `focused` in front: the lock, else the app's own.
    pub fn choice(&self, focused: Option<&str>) -> Option<String> {
        match &self.lock {
            Some(Lock::Default) => None,
            Some(Lock::Profile(name)) => Some(name.clone()),
            None => focused
                .and_then(|app| self.profile_for(app))
                .map(|p| p.name.clone()),
        }
    }
    /// Default, then each profile in order, then Default again (or backwards).
    pub fn cycle(&self, current: Option<&str>, forward: bool) -> Option<String> {
        let mut order = vec![None];
        order.extend(self.profiles.iter().map(|p| Some(p.name.as_str())));
        let at = order.iter().position(|o| *o == current).unwrap_or(0);
        let next = if forward {
            (at + 1) % order.len()
        } else {
            (at + order.len() - 1) % order.len()
        };
        order[next].map(str::to_owned)
    }
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }
    pub fn device_context(&self, name: &str) -> Option<u8> {
        let index = self.midi_devices.iter().position(|d| d == name)?;
        Some(DEVICE_CONTEXT + u8::try_from(index).ok()?)
    }
}
/// A MIDI device name: one printable line, at most 64 characters, never `-`.
pub fn valid_device_name(name: &str) -> bool {
    !name.is_empty()
        && name != "-"
        && name.chars().count() <= 64
        && !name.chars().any(char::is_control)
}
fn validate_devices(devices: &[String], active: Option<&String>) -> Result<()> {
    if devices.len() > usize::from(u8::MAX - DEVICE_CONTEXT) + 1 {
        return Err("too many MIDI devices (16 at most)".into());
    }
    for (i, name) in devices.iter().enumerate() {
        if !valid_device_name(name) {
            return Err(format!("invalid MIDI device name {name:?}").into());
        }
        if devices[..i].contains(name) {
            return Err(format!("MIDI device {name} listed twice").into());
        }
    }
    if active.is_some_and(|a| !devices.contains(a)) {
        return Err("the active MIDI device isn't listed".into());
    }
    Ok(())
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Agent {
    pub name: String,
    pub launch: String,
    pub window: String,
    /// Working directory overriding the Settings default; `${NAME}` allowed.
    pub dir: Option<String>,
    /// Status lights: this agent's octave light (±1..±4), which says which agent it is.
    pub octave: i8,
    /// Where the agent lives (v7 ticket 49). Orca: `launch` is the Orca agent kind (e.g.
    /// `claude`) and the folder is its workspace. herdr: `name` is herdr's agent name.
    pub host: Host,
}
/// Where an agent lives (v7 ticket 49). T3 Code has no supported way to send a prompt, so
/// it can't host one.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Host {
    #[default]
    Tmux,
    Orca,
    Herdr,
}
impl Host {
    pub fn text(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Orca => "orca",
            Self::Herdr => "herdr",
        }
    }
    fn parse(text: &str) -> Result<Self> {
        [Self::Tmux, Self::Orca, Self::Herdr]
            .into_iter()
            .find(|h| h.text() == text)
            .ok_or_else(|| {
                format!("unknown agent host {text:?}; expected tmux, orca or herdr").into()
            })
    }
}
/// herdr's own rule for agent names: `[a-z][a-z0-9_-]{0,31}`.
pub fn herdr_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
impl Agent {
    pub fn new(name: &str, launch: &str, window: &str) -> Result<Self> {
        if !simple_name(name)
            || !simple_name(window)
            || launch.trim().is_empty()
            || launch.len() > 4096
            || launch.chars().any(char::is_control)
        {
            return Err(
                "agent name/window must be simple names; launch must be one nonempty line".into(),
            );
        }
        Ok(Self {
            name: name.into(),
            launch: launch.into(),
            window: window.into(),
            dir: None,
            // Spec defaults: Claude +1, Codex −1.
            octave: if name == "codex" { -1 } else { 1 },
            host: Host::Tmux,
        })
    }
}
pub fn simple_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}
/// Environment-variable name rules: a letter or `_`, then letters, digits or `_`.
pub fn variable_name(name: &str) -> bool {
    name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
fn validate_variables(list: &[(String, String)], kind: &str) -> Result<()> {
    if list.len() > 64 {
        return Err(format!("too many {kind} variables").into());
    }
    let mut names = HashSet::new();
    for (name, value) in list {
        if !variable_name(name) {
            return Err(format!("{kind} variable name {name:?} must be letters, digits and _, not starting with a digit").into());
        }
        if value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(format!(
                "{kind} variable {name} must be one line without control characters"
            )
            .into());
        }
        if !names.insert(name) {
            return Err(format!("duplicate {kind} variable {name}").into());
        }
    }
    Ok(())
}
/// A directory field before expansion: one nonempty line.
fn directory_syntax(dir: &str) -> Result<()> {
    if dir.trim().is_empty() || dir.len() > 4096 || dir.chars().any(char::is_control) {
        return Err("directory must be one nonempty line without control characters".into());
    }
    Ok(())
}
/// Replace each `${NAME}` in a directory field with the user variable, else the startup
/// variable. Unknown names are an error (the same answer in every process: no environment
/// fallback); `$X` and `${…}` that is not a plain name stay as written.
pub fn expand(text: &str, config: &Config) -> Result<String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}').map(|end| &after[..end]) {
            Some(name) if variable_name(name) => {
                let value = config
                    .vars
                    .iter()
                    .chain(&config.env)
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.clone())
                    .ok_or_else(|| format!("unknown variable ${{{name}}}; add it in Settings"))?;
                out.push_str(&value);
                rest = &after[name.len() + 1..];
            }
            _ => {
                out.push_str("${");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}
/// Expanded directory field; must be an absolute path (existence is checked where it is used).
pub fn absolute_directory(dir: &str, config: &Config) -> Result<String> {
    directory_syntax(dir)?;
    let expanded = expand(dir, config)?;
    if !Path::new(&expanded).is_absolute()
        || expanded.len() > 4096
        || expanded.chars().any(char::is_control)
    {
        return Err(format!("directory {dir:?} must be an absolute path").into());
    }
    Ok(expanded)
}
fn validate_agents(agents: &[Agent]) -> Result<()> {
    if agents.len() > 64 {
        return Err("too many agents".into());
    }
    let mut names = HashSet::new();
    let mut windows = HashSet::new();
    for agent in agents {
        Agent::new(&agent.name, &agent.launch, &agent.window)?;
        agent.dir.as_deref().map(directory_syntax).transpose()?;
        crate::status::valid_octave(agent.octave)?;
        match agent.host {
            Host::Orca if !simple_name(&agent.launch) => {
                return Err(format!(
                    "Orca agent {}: its kind must be one simple name (e.g. claude), not a command",
                    agent.name
                )
                .into());
            }
            Host::Herdr if !herdr_name(&agent.name) => {
                return Err(format!(
                    "herdr agent {}: herdr names are lowercase letters, digits, _ and -",
                    agent.name
                )
                .into());
            }
            _ => {}
        }
        if !names.insert(&agent.name) || !windows.insert(&agent.window) {
            return Err("agent names and tmux windows must be unique".into());
        }
    }
    Ok(())
}
impl Config {
    /// The agent's working directory: its override, else the Settings default, expanded and
    /// absolute. None keeps the earlier behavior (tmux's own default).
    pub fn agent_directory(&self, agent: &Agent) -> Result<Option<String>> {
        agent
            .dir
            .as_ref()
            .or(self.agent_dir.as_ref())
            .map(|dir| absolute_directory(dir, self))
            .transpose()
    }
    /// Every directory field must expand to an absolute path. Command lines are shell lines:
    /// their `${NAME}` is the shell's (user variables are exported to it), so not checked here.
    fn validate_references(&self) -> Result<()> {
        for dir in self
            .agent_dir
            .iter()
            .chain(self.agents.iter().filter_map(|a| a.dir.as_ref()))
        {
            absolute_directory(dir, self)?;
        }
        for mapping in &self.mappings {
            for (_, dir) in mapping.action.commands() {
                absolute_directory(dir, self)?;
            }
        }
        Ok(())
    }
    pub fn idle_for(&self, context: u8) -> Settings {
        if context == 0 {
            self.idle
        } else {
            self.program_idle.get(&context).copied().unwrap_or_default()
        }
    }
    pub fn set_idle(&mut self, context: u8, settings: Settings) -> Result<()> {
        if context > 7 {
            return Err("idle feedback requires Program 1–8".into());
        }
        settings.validate()?;
        if context == 0 {
            self.idle = settings;
        } else if settings.enabled() {
            self.program_idle.insert(context, settings);
        } else {
            self.program_idle.remove(&context);
        }
        Ok(())
    }
}

fn optional<T: std::str::FromStr>(value: &str) -> Result<Option<T>>
where
    T::Err: std::error::Error + 'static,
{
    if value == "-" {
        Ok(None)
    } else {
        Ok(Some(value.parse()?))
    }
}
fn arp(value: &str) -> Result<Option<bool>> {
    match value {
        "-" => Ok(None),
        "on" => Ok(Some(true)),
        "off" => Ok(Some(false)),
        _ => Err("arpeggiator must be on, off, or -".into()),
    }
}
fn arp_text(value: Option<bool>) -> &'static str {
    match value {
        None => "-",
        Some(true) => "on",
        Some(false) => "off",
    }
}
fn opt_text(value: Option<impl fmt::Display>) -> String {
    value.map(|n| n.to_string()).unwrap_or_else(|| "-".into())
}

/// One mapping line of a `version` config (also a profile file's override lines).
/// Orca's "needs me" as the Orca family builds it: the jump palette, a short wait, its first
/// row (v7 tickets 15 and 98).
pub fn orca_needs_me_steps() -> Vec<crate::actions::Step> {
    use crate::actions::Step;
    vec![
        Step::Keys(vec!["Ctrl".into(), "J".into()]),
        Step::Wait(150),
        Step::Keys(vec!["Ctrl".into(), "1".into()]),
    ]
}
fn is_orca_needs_me(steps: &[crate::actions::Step]) -> bool {
    steps == orca_needs_me_steps().as_slice()
}
fn parse_mapping_line(line: &str, version: u32, midi_devices: &[String]) -> Result<Mapping> {
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len()
        != match version {
            1 => 6,
            2 => 9,
            3 => 13,
            4..=8 => 14,
            9 | 10 => 15,
            _ => 16,
        }
    {
        return Err("wrong mapping field count".into());
    }
    let (context, label, fields) = if version >= 3 {
        (fields[0].parse::<u8>()?, Some(fields[1]), &fields[3..])
    } else {
        (0, None, fields.as_slice())
    };
    let offset = usize::from(version >= 3);
    let (behavior, shortcut) = (fields[4 + offset], fields[5 + offset]);
    let [device, message, channel, note] = &fields[..4] else {
        unreachable!()
    };
    let other = context >= DEVICE_CONTEXT;
    if other && usize::from(context - DEVICE_CONTEXT) >= midi_devices.len() {
        return Err("mapping for an unlisted MIDI device".into());
    }
    if *device != device_field(context) || (version < 3 && *message != "note") {
        return Err("unsupported device or MIDI message type".into());
    }
    let pad = if version >= 3 {
        let message = match *message {
            "note" => crate::Message::Note,
            "cc" => crate::Message::Cc,
            "bend" => crate::Message::Bend,
            "button" if version >= 16 => crate::Message::Button,
            _ => return Err("unsupported MIDI message type".into()),
        };
        let control = Control {
            channel: channel.parse()?,
            id: note.parse()?,
            message,
            direction: fields[4].parse()?,
        };
        control.validate()?;
        control
    } else {
        Control::from_note(channel.parse()?, note.parse()?)
            .ok_or("unsupported pad channel or note")?
    };
    let action = if version >= 4 {
        if version < 22 && fields[10].starts_with("orca-keys:") {
            return Err("Orca keys need a v22 configuration".into());
        }
        crate::actions::Action::parse(fields[10])?
    } else {
        crate::actions::Action::Shortcut
    };
    // v22 (v7 ticket 98): Orca's "needs me" saved by an older build becomes Orca keys, so
    // it never fires outside Orca.
    let action = match action {
        crate::actions::Action::Sequence(steps) if version < 22 && is_orca_needs_me(&steps) => {
            crate::actions::Action::OrcaKeys(steps)
        }
        action => action,
    };
    let mut mapping = if action == crate::actions::Action::Shortcut {
        if behavior == "tap" && version < 21 {
            return Err("the tap behavior needs a v21 configuration".into());
        }
        Mapping::new(pad, shortcut, behavior)?
    } else {
        if behavior != "trigger" || shortcut != "-" {
            return Err("invalid action shortcut/behavior fields".into());
        }
        Mapping::new_action(pad, line.split('\t').nth(2).unwrap().parse()?, action)?
    };
    mapping.context = context;
    mapping.risky = match fields.get(11).filter(|_| version >= 9) {
        None | Some(&"-") => false,
        Some(&"risky") => true,
        Some(_) => return Err("expected risky or -".into()),
    };
    mapping.status = match fields.get(12).filter(|_| version >= 11) {
        None | Some(&"-") => false,
        Some(&"status") => true,
        Some(_) => return Err("expected status or -".into()),
    };
    if version >= 3 {
        mapping.input = line.split('\t').nth(2).unwrap().parse()?;
    }
    if let Some(label) = label {
        mapping.label = label.to_owned();
    }
    if version >= 2 {
        let octave = match fields[6 + offset] {
            "-" => None,
            value => {
                let (kind, n) = value.split_once(':').ok_or("invalid octave setting")?;
                Some(match kind {
                    "offset" => Octave::Offset(n.parse()?),
                    "absolute" => Octave::Absolute(n.parse()?),
                    _ => return Err("invalid octave mode".into()),
                })
            }
        };
        mapping.feedback = Feedback {
            octave,
            tempo: optional(fields[7 + offset])?,
            arp: arp(fields[8 + offset])?,
        };
        mapping.validate()?;
    }
    mapping.validate()?;
    Ok(mapping)
}

/// v7 ticket 86: `device-lights` (MPK, Xbox, DualSense switches), then an `xbox-pattern`
/// and a `ps5-pattern` per busy state, in `status::BUSY` order.
fn device_lights_text(p: &crate::status::DevicePatterns) -> String {
    use crate::status::{BUSY, LIGHTS, RUMBLES, word};
    let light = |l| word(&LIGHTS, l);
    let rumble = |r| word(&RUMBLES, r);
    let on = |b: bool| if b { "on" } else { "off" };
    let mut text = format!(
        "device-lights\t{}\t{}\t{}\n",
        on(p.on[0]),
        on(p.on[1]),
        on(p.on[2])
    );
    for (state, (l, r)) in BUSY.iter().zip(p.xbox) {
        text.push_str(&format!(
            "xbox-pattern\t{state}\t{}\t{}\n",
            light(l),
            rumble(r)
        ));
    }
    for (state, (c, l, r)) in BUSY.iter().zip(p.ps5) {
        text.push_str(&format!(
            "ps5-pattern\t{state}\t{c}\t{}\t{}\n",
            light(l),
            rumble(r)
        ));
    }
    text
}
fn parse_device_lights(
    lines: &mut std::iter::Peekable<std::str::Lines>,
) -> Result<crate::status::DevicePatterns> {
    use crate::status::{BUSY, COLOURS, LIGHTS, RUMBLES};
    let mut next = |record: &str| -> Result<Vec<String>> {
        let line = lines.next().ok_or(format!("missing {record}"))?;
        let fields = line.split('\t').map(str::to_owned).collect::<Vec<_>>();
        if fields[0] != record {
            return Err(format!("expected {record}, found {}", fields[0]).into());
        }
        Ok(fields[1..].to_vec())
    };
    let word = |w: &str| match w {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(format!("device-lights must be on or off, not {w}")),
    };
    let light = |w: &str| {
        LIGHTS
            .iter()
            .find(|(x, _)| *x == w)
            .map(|(_, l)| *l)
            .ok_or_else(|| {
                format!("light must be off, steady, slow-pulse, fast-blink or blink, not {w}")
            })
    };
    let rumble = |w: &str| {
        RUMBLES
            .iter()
            .find(|(x, _)| *x == w)
            .map(|(_, r)| *r)
            .ok_or_else(|| format!("rumble must be off, short or long, not {w}"))
    };
    let mut p = crate::status::DevicePatterns::default();
    let [mpk, xbox, ps5] = &next("device-lights")?[..] else {
        return Err("expected device-lights MPK/Xbox/DualSense switches".into());
    };
    p.on = [word(mpk)?, word(xbox)?, word(ps5)?];
    for (i, state) in BUSY.iter().enumerate() {
        let [name, l, r] = &next("xbox-pattern")?[..] else {
            return Err("expected xbox-pattern state/light/rumble".into());
        };
        if name != state {
            return Err(format!("xbox-pattern {state} expected, found {name}").into());
        }
        p.xbox[i] = (light(l)?, rumble(r)?);
    }
    for (i, state) in BUSY.iter().enumerate() {
        let [name, c, l, r] = &next("ps5-pattern")?[..] else {
            return Err("expected ps5-pattern state/colour/light/rumble".into());
        };
        if name != state {
            return Err(format!("ps5-pattern {state} expected, found {name}").into());
        }
        let colour = COLOURS.iter().find(|x| **x == c).ok_or(format!(
            "light-bar colour must be one of {}, not {c}",
            COLOURS.join(", ")
        ))?;
        p.ps5[i] = (colour, light(l)?, rumble(r)?);
    }
    Ok(p)
}

fn parse_config(contents: &str) -> Result<Config> {
    let mut lines = contents.lines().peekable();
    // Files saved before the KeyAI → VibeConsole rename keep their old header.
    let version = lines.next().and_then(|header| {
        header
            .strip_prefix("vibeconsole-mappings-v")
            .or_else(|| header.strip_prefix("keyai-mappings-v"))
    });
    let version: u32 = match version {
        Some("1") => 1,
        Some("2") => 2,
        Some("3") => 3,
        Some("4") => 4,
        Some("5") => 5,
        Some("6") => 6,
        Some("7") => 7,
        Some("8") => 8,
        Some("9") => 9,
        Some("10") => 10,
        Some("11") => 11,
        Some("12") => 12,
        Some("13") => 13,
        Some("14") => 14,
        Some("15") => 15,
        Some("16") => 16,
        Some("17") => 17,
        Some("18") => 18,
        // v19 (v7 ticket 81): a prompt-sequence step may carry its own text (`prompt-own:`).
        Some("19") => 19,
        // v20 (v7 ticket 86): status-light switches and patterns per device.
        Some("20") => 20,
        // v21 (v7 ticket 96): the one-tap behavior (`tap`) for pads, keys and buttons.
        Some("21") => 21,
        // v22 (v7 ticket 98): Orca's keys sent only while Orca is focused (`orca-keys:`).
        Some("22") => 22,
        _ => {
            return Err(
                "invalid configuration header; expected vibeconsole-mappings-v1..v22".into(),
            );
        }
    };
    let idle = if version >= 2 {
        let fields = lines
            .next()
            .ok_or("missing idle configuration")?
            .split('\t')
            .collect::<Vec<_>>();
        let ["idle", octave, tempo, arpeggiator] = fields.as_slice() else {
            return Err("expected idle octave/tempo/arpeggiator fields".into());
        };
        Settings {
            octave: optional(octave)?,
            tempo: optional(tempo)?,
            arp: arp(arpeggiator)?,
        }
        .validate()?
    } else {
        Settings::default()
    };
    let mut program_idle = std::collections::BTreeMap::new();
    if version >= 5 {
        for context in 1..8u8 {
            let fields = lines
                .next()
                .ok_or("missing program idle configuration")?
                .split('\t')
                .collect::<Vec<_>>();
            let ["program-idle", id, octave, tempo, arpeggiator] = fields.as_slice() else {
                return Err("expected program-idle context/octave/tempo/arpeggiator".into());
            };
            if id.parse::<u8>()? != context {
                return Err("program idle contexts must appear exactly once in order 1–7".into());
            }
            let settings = Settings {
                octave: optional(octave)?,
                tempo: optional(tempo)?,
                arp: arp(arpeggiator)?,
            }
            .validate()?;
            if settings.enabled() {
                program_idle.insert(context, settings);
            }
        }
    }
    let start_flow = if version >= 6 {
        match lines.next().ok_or("missing Flow startup setting")? {
            "start-flow\ton" => true,
            "start-flow\toff" => false,
            _ => return Err("expected start-flow on/off setting".into()),
        }
    } else {
        true
    };
    let mut agent_dir = None;
    let mut env = Vec::new();
    let mut vars = Vec::new();
    if version >= 12 {
        agent_dir = match lines
            .next()
            .ok_or("missing agent-dir setting")?
            .split_once('\t')
        {
            Some(("agent-dir", "-")) => None,
            Some(("agent-dir", dir)) => Some(dir.to_owned()),
            _ => return Err("expected agent-dir setting".into()),
        };
        for (list, header, record) in [
            (&mut env, "env", "env-var"),
            (&mut vars, "vars", "user-var"),
        ] {
            let count: usize = lines
                .next()
                .and_then(|l| l.strip_prefix(&format!("{header}\t")))
                .ok_or(format!("expected {header} count"))?
                .parse()?;
            for _ in 0..count.min(65) {
                let fields: Vec<_> = lines
                    .next()
                    .ok_or("missing variable")?
                    .split('\t')
                    .collect();
                let [kind, name, value] = fields.as_slice() else {
                    return Err("invalid variable record".into());
                };
                if *kind != record {
                    return Err(format!("expected {record} record").into());
                }
                list.push((name.to_string(), value.to_string()));
            }
        }
        validate_variables(&env, "startup")?;
        validate_variables(&vars, "user")?;
        agent_dir.as_deref().map(directory_syntax).transpose()?;
    }
    let status_lights = if version >= 13 {
        match lines.next().ok_or("missing status-lights setting")? {
            "status-lights\ton" => true,
            "status-lights\toff" => false,
            _ => return Err("expected status-lights on/off setting".into()),
        }
    } else {
        true
    };
    let defaults = Config::default();
    let (mut light_other, mut light_other_on, mut light_folders) =
        (defaults.light_other, false, Vec::new());
    let mut light_patterns = defaults.light_patterns;
    if version >= 15 {
        let fields: Vec<_> = lines
            .next()
            .ok_or("missing light-tempos")?
            .split('\t')
            .collect();
        let ["light-tempos", slow, fast] = fields.as_slice() else {
            return Err("expected light-tempos slow/fast".into());
        };
        (light_patterns.slow, light_patterns.fast) = (slow.parse()?, fast.parse()?);
        for (index, state) in crate::status::BUSY.iter().enumerate() {
            let fields: Vec<_> = lines
                .next()
                .ok_or("missing light-pattern")?
                .split('\t')
                .collect();
            let ["light-pattern", name, tempo, octave] = fields.as_slice() else {
                return Err("expected light-pattern state/tempo/octave".into());
            };
            if name != state {
                return Err(format!("light-pattern {state} expected, found {name}").into());
            }
            let tempo = crate::status::TEMPOS
                .iter()
                .find(|(t, _)| t == tempo)
                .ok_or("light-pattern tempo must be off, slow or fast")?
                .1;
            let octave = crate::status::OCTAVES
                .iter()
                .find(|(o, _)| o == octave)
                .ok_or("light-pattern octave must be off, steady, blink or alternate")?
                .1;
            light_patterns.states[index] = (tempo, octave);
        }
        light_patterns.validate()?;
    }
    if version >= 14 {
        // v14 stored tempos per source; v15 keeps only the octave (tempo = state).
        let fields: Vec<_> = lines
            .next()
            .ok_or("missing light-other setting")?
            .split('\t')
            .collect();
        let (on, octave) = match fields.as_slice() {
            ["light-other", on, octave, _, _] if version == 14 => (on, octave),
            ["light-other", on, octave] if version >= 15 => (on, octave),
            _ => return Err("expected light-other on/off/octave".into()),
        };
        light_other_on = match *on {
            "on" => true,
            "off" => false,
            _ => return Err("light-other must be on or off".into()),
        };
        light_other = crate::status::valid_octave(octave.parse()?)?;
        let count: usize = lines
            .next()
            .and_then(|l| l.strip_prefix("light-folders\t"))
            .ok_or("expected light-folders count")?
            .parse()?;
        for _ in 0..count.min(64) {
            let fields: Vec<_> = lines
                .next()
                .ok_or("missing light folder")?
                .split('\t')
                .collect();
            let (folder, octave) = match fields.as_slice() {
                ["light-folder", folder, octave, _, _] if version == 14 => (folder, octave),
                ["light-folder", folder, octave] if version >= 15 => (folder, octave),
                _ => return Err("invalid light-folder record".into()),
            };
            light_folders.push((folder.to_string(), octave.parse()?));
        }
        validate_light_folders(&light_folders)?;
    }
    let agents = if version >= 8 {
        let count: usize = lines
            .next()
            .ok_or("missing agent count")?
            .strip_prefix("agents\t")
            .ok_or("expected agents count")?
            .parse()?;
        if count > 64 {
            return Err("too many agents".into());
        }
        let mut agents = Vec::new();
        for _ in 0..count {
            let fields: Vec<_> = lines.next().ok_or("missing agent")?.split('\t').collect();
            let mut host = Host::Tmux;
            let (name, launch, window, dir, lights) = match fields.as_slice() {
                ["agent", name, launch, window] if version < 12 => {
                    (name, launch, window, None, None)
                }
                ["agent", name, launch, window, dir] if version == 12 => {
                    (name, launch, window, Some(dir), None)
                }
                ["agent", name, launch, window, dir, octave, hosted] if version >= 17 => {
                    host = Host::parse(hosted)?;
                    (name, launch, window, Some(dir), Some(octave))
                }
                ["agent", name, launch, window, dir, octave, thinking, input]
                    if (13..15).contains(&version) =>
                {
                    // Per-agent tempos became the shared slow/fast tempos (first agent's).
                    if agents.is_empty() {
                        (light_patterns.slow, light_patterns.fast) =
                            (thinking.parse()?, input.parse()?);
                    }
                    (name, launch, window, Some(dir), Some(octave))
                }
                ["agent", name, launch, window, dir, octave] if (15..17).contains(&version) => {
                    (name, launch, window, Some(dir), Some(octave))
                }
                _ => return Err("invalid agent record".into()),
            };
            let mut agent = Agent::new(name, launch, window)?;
            agent.host = host;
            agent.dir = dir.filter(|dir| **dir != "-").map(|dir| dir.to_string());
            if let Some(octave) = lights {
                agent.octave = octave.parse()?;
            }
            agents.push(agent);
        }
        agents
    } else {
        Config::default().agents
    };
    validate_agents(&agents)?;
    let tuning = if version >= 16 {
        let line = lines.next().ok_or("missing tuning")?;
        let ["tuning", dead_zone, trigger, speed, rumble, mouse] =
            line.split('\t').collect::<Vec<_>>()[..]
        else {
            return Err("expected tuning dead-zone/trigger/mouse-speed/rumble/mouse fields".into());
        };
        Tuning {
            dead_zone: dead_zone.parse()?,
            trigger: trigger.parse()?,
            mouse_speed: speed.parse()?,
            rumble: match rumble {
                "on" => true,
                "off" => false,
                _ => return Err("tuning rumble must be on or off".into()),
            },
            mouse: match mouse {
                "off" => MouseStick::Off,
                "left" => MouseStick::Left,
                "right" => MouseStick::Right,
                _ => return Err("tuning mouse must be off, left or right".into()),
            },
            sticks: if version >= 18 {
                let line = lines.next().ok_or("missing sticks")?;
                let ["sticks", left, right] = line.split('\t').collect::<Vec<_>>()[..] else {
                    return Err("expected sticks left/right fields".into());
                };
                [StickMode::parse(left)?, StickMode::parse(right)?]
            } else {
                Default::default()
            },
        }
        .validate()?
    } else {
        Tuning::default()
    };
    let device_lights = if version >= 20 {
        parse_device_lights(&mut lines)?
    } else {
        Default::default()
    };
    // v16 grew record by record during v7 (devices, profiles, lock, favourites): a v16 file
    // written before one existed loads it with its safe default instead of failing.
    let has = |lines: &mut std::iter::Peekable<std::str::Lines>, record: &str| {
        version >= 16 && lines.peek().is_some_and(|l| l.starts_with(record))
    };
    let (mut midi_devices, mut midi_active) = (Vec::new(), None);
    if has(&mut lines, "midi-devices\t") {
        let count: usize = lines
            .next()
            .and_then(|l| l.strip_prefix("midi-devices\t"))
            .ok_or("expected midi-devices count")?
            .parse()?;
        if count > 16 {
            return Err("too many MIDI devices (16 at most)".into());
        }
        for _ in 0..count {
            let name = lines
                .next()
                .and_then(|l| l.strip_prefix("midi-device\t"))
                .ok_or("expected midi-device record")?;
            midi_devices.push(name.to_owned());
        }
        let active = lines
            .next()
            .and_then(|l| l.strip_prefix("midi-active\t"))
            .ok_or("expected midi-active")?;
        midi_active = (active != "-").then(|| active.to_owned());
        validate_devices(&midi_devices, midi_active.as_ref())?;
    }
    let parse_mapping = |line: &str| parse_mapping_line(line, version, &midi_devices);
    let mut profiles = Vec::new();
    if has(&mut lines, "profiles\t") {
        let count: usize = lines
            .next()
            .and_then(|l| l.strip_prefix("profiles\t"))
            .ok_or("expected profiles count")?
            .parse()?;
        if count > 32 {
            return Err("too many app profiles (32 at most)".into());
        }
        for _ in 0..count {
            let line = lines.next().ok_or("missing profile")?;
            let ["profile", name, apps, overrides] = line.split('\t').collect::<Vec<_>>()[..]
            else {
                return Err("invalid profile record".into());
            };
            let overrides: usize = overrides.parse()?;
            if overrides > 512 {
                return Err(format!("profile {name}: too many overrides").into());
            }
            let mut profile = Profile {
                name: name.to_owned(),
                apps: if apps == "-" {
                    Vec::new()
                } else {
                    apps.split(',').map(str::to_owned).collect()
                },
                mappings: Vec::new(),
            };
            for _ in 0..overrides {
                let line = lines.next().ok_or("missing profile override")?;
                profile
                    .mappings
                    .push(parse_mapping(line).map_err(|e| format!("profile {name}: {e}"))?);
            }
            profiles.push(profile);
        }
        validate_profiles(&profiles)?;
    }
    let lock = if has(&mut lines, "profile-lock\t") {
        match lines
            .next()
            .and_then(|l| l.strip_prefix("profile-lock\t"))
            .ok_or("expected profile-lock")?
        {
            "-" => None,
            "Default" => Some(Lock::Default),
            name if profiles.iter().any(|p| p.name == name) => Some(Lock::Profile(name.into())),
            _ => return Err("the locked profile doesn't exist".into()),
        }
    } else {
        None
    };
    let mut favourites = Vec::new();
    if has(&mut lines, "favourites\t") {
        let count: usize = lines
            .next()
            .and_then(|l| l.strip_prefix("favourites\t"))
            .ok_or("expected favourites count")?
            .parse()?;
        if count > 1024 {
            return Err("too many favourites".into());
        }
        for _ in 0..count {
            let key = lines
                .next()
                .and_then(|l| l.strip_prefix("favourite\t"))
                .ok_or("expected favourite record")?;
            favourites.push(key.to_owned());
        }
        validate_favourites(&favourites)?;
    }
    let mut mappings = Vec::new();
    let mut seen = HashSet::new();
    let mut labels = HashSet::new();
    for (index, line) in lines.enumerate() {
        let result = parse_mapping(line).and_then(|mapping| {
            if !seen.insert((mapping.context, mapping.control)) {
                return Err("duplicate control assignment in context".into());
            }
            if !labels.insert((mapping.context, mapping.label.clone())) {
                return Err("duplicate physical label in context".into());
            }
            Ok(mapping)
        });
        mappings
            .push(result.map_err(|error| format!("configuration line {}: {error}", index + 2))?);
    }
    for (i, mapping) in mappings.iter().enumerate() {
        for other in &mappings[..i] {
            if mapping.context == other.context
                && mapping.control.message == other.control.message
                && mapping.control.channel == other.control.channel
                && mapping.control.id == other.control.id
                && mapping.input != other.input
            {
                return Err(
                    "ambiguous source: one emitted control has conflicting input profiles".into(),
                );
            }
        }
        if mapping.input == Input::Piano
            && ((if mapping.context == 0 {
                idle.enabled()
            } else {
                program_idle
                    .get(&mapping.context)
                    .is_some_and(|s| s.enabled())
            }) || mappings
                .iter()
                .any(|m| m.context == mapping.context && m.feedback.enabled()))
        {
            return Err("piano context must leave musical feedback unchanged".into());
        }
    }
    mappings.sort_by_key(|mapping| (mapping.context, mapping.control));
    Ok(Config {
        mappings,
        agents,
        idle,
        program_idle,
        start_flow,
        agent_dir,
        env,
        vars,
        status_lights,
        light_folders,
        light_other,
        light_other_on,
        light_patterns: light_patterns.validate()?,
        tuning,
        device_lights,
        midi_devices,
        midi_active,
        profiles,
        lock,
        favourites,
    })
}

/// Folder sources: absolute, one line, each once, at most 64.
pub fn validate_light_folders(folders: &[(String, i8)]) -> Result<()> {
    let mut seen = HashSet::new();
    if folders.len() > 64 {
        return Err("too many status-light folders".into());
    }
    for (folder, octave) in folders {
        if !folder.starts_with('/') || folder.contains(['\t', '\n']) || folder.len() > 4096 {
            return Err(format!("status-light folder {folder:?} must be one absolute path").into());
        }
        if !seen.insert(folder.trim_end_matches('/')) {
            return Err(format!("status-light folder {folder} is listed twice").into());
        }
        crate::status::valid_octave(*octave)?;
    }
    Ok(())
}

pub fn load_config(path: &Path) -> Result<Config> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            parse_config(&contents).map_err(|error| format!("{}: {error}", path.display()).into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(error.into()),
    }
}

pub fn load(path: &Path) -> Result<Vec<Mapping>> {
    Ok(load_config(path)?.mappings)
}
pub fn save(path: &Path, mappings: &[Mapping]) -> Result<()> {
    let mut config = load_config(path)?;
    config.mappings = mappings.to_vec();
    save_config(path, &config)
}

fn validate_favourites(favourites: &[String]) -> Result<()> {
    if favourites.len() > 1024 {
        return Err("too many favourites".into());
    }
    for (i, key) in favourites.iter().enumerate() {
        if key.is_empty() || key.len() > 200 || key.chars().any(char::is_control) {
            return Err(format!("invalid favourite {key:?}").into());
        }
        if favourites[..i].contains(key) {
            return Err(format!("favourite {key} listed twice").into());
        }
    }
    Ok(())
}

/// One mapping as a config line (Default mappings and profile overrides alike).
fn mapping_line(mapping: &Mapping) -> Result<String> {
    mapping.validate()?;
    let feedback = mapping.feedback;
    let device = device_field(mapping.context);
    let octave = match feedback.octave {
        None => "-".into(),
        Some(Octave::Offset(n)) => format!("offset:{n}"),
        Some(Octave::Absolute(n)) => format!("absolute:{n}"),
    };
    Ok(format!(
        "{}\t{}\t{}\t{device}\t{}\t{}\t{}\t{}\t{}\t{}\t{octave}\t{}\t{}\t{}\t{}\t{}\n",
        mapping.context,
        mapping.label,
        mapping.input,
        mapping.control.message.text(),
        mapping.control.channel,
        mapping.control.id,
        mapping.control.direction,
        mapping.behavior,
        if mapping.keys.is_empty() {
            "-".into()
        } else {
            mapping.keys.join("+")
        },
        opt_text(feedback.tempo),
        arp_text(feedback.arp),
        mapping.action.encode(),
        if mapping.risky { "risky" } else { "-" },
        if mapping.status { "status" } else { "-" }
    ))
}

/// The configuration file's text, checked by parsing it back.
pub fn serialize_config(config: &Config) -> Result<String> {
    config.idle.validate()?;
    validate_agents(&config.agents)?;
    validate_variables(&config.env, "startup")?;
    validate_variables(&config.vars, "user")?;
    config.validate_references()?;
    let mut contents = format!(
        "vibeconsole-mappings-v22\nidle\t{}\t{}\t{}\n",
        opt_text(config.idle.octave),
        opt_text(config.idle.tempo),
        arp_text(config.idle.arp)
    );
    if config.program_idle.keys().any(|id| !(1..=7).contains(id)) {
        return Err("program idle context must be 1–7; original idle belongs to context 0".into());
    }
    for context in 1..8 {
        let idle = config.idle_for(context).validate()?;
        contents.push_str(&format!(
            "program-idle\t{context}\t{}\t{}\t{}\n",
            opt_text(idle.octave),
            opt_text(idle.tempo),
            arp_text(idle.arp)
        ));
    }
    contents.push_str(if config.start_flow {
        "start-flow\ton\n"
    } else {
        "start-flow\toff\n"
    });
    contents.push_str(&format!(
        "agent-dir\t{}\n",
        config.agent_dir.as_deref().unwrap_or("-")
    ));
    for (list, header, record) in [
        (&config.env, "env", "env-var"),
        (&config.vars, "vars", "user-var"),
    ] {
        contents.push_str(&format!("{header}\t{}\n", list.len()));
        for (name, value) in list {
            contents.push_str(&format!("{record}\t{name}\t{value}\n"));
        }
    }
    contents.push_str(if config.status_lights {
        "status-lights\ton\n"
    } else {
        "status-lights\toff\n"
    });
    let patterns = config.light_patterns.validate()?;
    contents.push_str(&format!(
        "light-tempos\t{}\t{}\n",
        patterns.slow, patterns.fast
    ));
    for (state, (tempo, octave)) in crate::status::BUSY.iter().zip(patterns.states) {
        contents.push_str(&format!(
            "light-pattern\t{state}\t{}\t{}\n",
            crate::status::tempo_word(tempo),
            crate::status::octave_word(octave)
        ));
    }
    crate::status::valid_octave(config.light_other)?;
    validate_light_folders(&config.light_folders)?;
    contents.push_str(&format!(
        "light-other\t{}\t{}\nlight-folders\t{}\n",
        if config.light_other_on { "on" } else { "off" },
        config.light_other,
        config.light_folders.len()
    ));
    for (folder, octave) in &config.light_folders {
        contents.push_str(&format!("light-folder\t{folder}\t{octave}\n"));
    }
    contents.push_str(&format!("agents\t{}\n", config.agents.len()));
    for agent in &config.agents {
        contents.push_str(&format!(
            "agent\t{}\t{}\t{}\t{}\t{}\t{}\n",
            agent.name,
            agent.launch,
            agent.window,
            agent.dir.as_deref().unwrap_or("-"),
            agent.octave,
            agent.host.text()
        ));
    }
    let tuning = config.tuning.clone().validate()?;
    contents.push_str(&format!(
        "tuning\t{}\t{}\t{}\t{}\t{}\nsticks\t{}\t{}\n",
        tuning.dead_zone,
        tuning.trigger,
        tuning.mouse_speed,
        if tuning.rumble { "on" } else { "off" },
        tuning.mouse.text(),
        tuning.sticks[0].text(),
        tuning.sticks[1].text()
    ));
    config.device_lights.validate()?;
    contents.push_str(&device_lights_text(&config.device_lights));
    contents.push_str(&format!("midi-devices\t{}\n", config.midi_devices.len()));
    for name in &config.midi_devices {
        contents.push_str(&format!("midi-device\t{name}\n"));
    }
    contents.push_str(&format!(
        "midi-active\t{}\n",
        config.midi_active.as_deref().unwrap_or("-")
    ));
    validate_devices(&config.midi_devices, config.midi_active.as_ref())?;
    validate_profiles(&config.profiles)?;
    contents.push_str(&format!("profiles\t{}\n", config.profiles.len()));
    for profile in &config.profiles {
        contents.push_str(&format!(
            "profile\t{}\t{}\t{}\n",
            profile.name,
            if profile.apps.is_empty() {
                "-".to_owned()
            } else {
                profile.apps.join(",")
            },
            profile.mappings.len()
        ));
        for mapping in &profile.mappings {
            contents.push_str(&mapping_line(mapping)?);
        }
    }
    contents.push_str(&format!(
        "profile-lock\t{}\n",
        match &config.lock {
            None => "-",
            Some(Lock::Default) => "Default",
            Some(Lock::Profile(name)) => name,
        }
    ));
    validate_favourites(&config.favourites)?;
    contents.push_str(&format!("favourites\t{}\n", config.favourites.len()));
    for key in &config.favourites {
        contents.push_str(&format!("favourite\t{key}\n"));
    }
    for mapping in &config.mappings {
        contents.push_str(&mapping_line(mapping)?);
    }
    parse_config(&contents)?;
    Ok(contents)
}

pub fn save_config(path: &Path, config: &Config) -> Result<()> {
    let contents = serialize_config(config)?;
    fs::create_dir_all(path.parent().ok_or("configuration path has no parent")?)?;
    // ponytail: one staging file; concurrent saves fail safely. Remove a stale file after an interrupted save.
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| {
            format!(
                "cannot create {}: {error}; saved mappings unchanged",
                temporary.display()
            )
        })?;
    let result = (|| -> Result<()> {
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn agents_and_prompt_mapping_round_trip_while_old_config_migrates() {
        use super::*;
        let mut config = parse_config("vibeconsole-mappings-v7\nidle\t-\t-\t-\nprogram-idle\t1\t-\t-\t-\nprogram-idle\t2\t-\t-\t-\nprogram-idle\t3\t-\t-\t-\nprogram-idle\t4\t-\t-\t-\nprogram-idle\t5\t-\t-\t-\nprogram-idle\t6\t-\t-\t-\nprogram-idle\t7\t-\t-\t-\nstart-flow\ton\n").unwrap();
        assert_eq!(
            config
                .agents
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            ["claude", "codex"]
        );
        config.agents.push(
            Agent::new(
                "claude-yolo",
                "claude --dangerously-skip-permissions",
                "claude-yolo",
            )
            .unwrap(),
        );
        config.mappings.push(
            Mapping::new_action(
                Control::from_note(10, 36).unwrap(),
                Input::Pad,
                crate::actions::Action::Prompt {
                    file: "review".into(),
                    target: crate::actions::Target::Agent("claude-yolo".into()),
                    enter: true,
                },
            )
            .unwrap(),
        );
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-agents-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        save_config(&path, &config).unwrap();
        assert_eq!(load_config(&path).unwrap(), config);
        config.mappings.push(
            Mapping::new_action(
                Control::from_note(10, 37).unwrap(),
                Input::Pad,
                crate::actions::Action::Sequence(vec![
                    crate::actions::Step::Agent("claude".into()),
                    crate::actions::Step::Wait(3000),
                    crate::actions::Step::Keys(vec!["Enter".into()]),
                ]),
            )
            .unwrap(),
        );
        config.mappings[0].risky = true;
        save_config(&path, &config).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("\trisky\t-\n"));
        assert_eq!(load_config(&path).unwrap(), config);
        let cc = Control {
            channel: 1,
            id: 70,
            message: crate::Message::Cc,
            direction: 1,
        };
        let mut knob = Mapping::new(cc, "Shift", "pulse").unwrap();
        knob.input = Input::Knob { step: 4 };
        knob.validate().unwrap();
        knob.risky = true;
        assert!(knob.validate().is_err());
        config.agents[2].window = "claude".into();
        assert!(save_config(&path, &config).is_err());
        assert_ne!(load_config(&path).unwrap(), config);
        fs::remove_dir_all(dir).unwrap();
    }
    use super::*;

    #[test]
    fn flow_startup_defaults_on_and_round_trips_off() {
        let mut old = "vibeconsole-mappings-v5\nidle\t-\t-\t-\n".to_owned();
        for context in 1..8 {
            old.push_str(&format!("program-idle\t{context}\t-\t-\t-\n"));
        }
        assert!(parse_config(&old).unwrap().start_flow);
        let mut config = Config::default();
        config.start_flow = false;
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-flow-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        save_config(&path, &config).unwrap();
        assert_eq!(load_config(&path).unwrap(), config);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("start-flow\toff\n"));
        assert!(parse_config(&text.replace("start-flow\toff", "start-flow\tmaybe")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn captured_programs_name_pads_knobs_and_fall_back_to_raw_identity() {
        let capture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../testdata/mpk-mini3-programs.hex"
        ));
        // Line 0 is RAM; lines 1–8 are stored Programs 1–8 (contexts 0–7).
        let program = |context: u8| {
            let line = capture.lines().filter(|l| !l.starts_with('#'));
            let payload: crate::feedback::Payload = line
                .clone()
                .nth(context as usize + 1)
                .unwrap()
                .split_whitespace()
                .map(|b| u8::from_str_radix(b, 16).unwrap())
                .collect::<Vec<_>>()
                .try_into()
                .unwrap();
            (context, payload)
        };
        let note = |channel, id| Control::note(channel, id).unwrap();
        let name = |control, context, p: &(u8, _)| control_name(control, None, context, Some(p));
        let cc = |id, direction| Control {
            channel: 1,
            id,
            message: crate::Message::Cc,
            direction,
        };
        // Live cross-checks recorded in archive/tickets/01-physical-control-names.md.
        let (p1, p2, p4) = (program(0), program(1), program(3));
        assert_eq!(name(note(1, 21), 1, &p2), "Bank A Pad 1");
        assert_eq!(name(note(1, 29), 1, &p2), "Bank B Pad 1");
        assert_eq!(name(note(2, 5), 0, &p1), "Bank A Pad 1");
        assert_eq!(name(note(10, 37), 3, &p4), "Bank A Pad 1");
        assert_eq!(name(cc(70, 1), 3, &p4), "Knob 1 increase");
        assert_eq!(name(cc(72, -1), 3, &p4), "Knob 3 decrease");
        // Fallbacks never guess.
        assert_eq!(
            name(note(10, 29), 1, &p2),
            "Note ch10 #29 (not on a pad in Program 2)"
        );
        assert_eq!(
            name(note(1, 21), 0, &p2),
            "Note ch1 #21 (Program 1 unverified)"
        );
        assert_eq!(
            control_name(note(1, 21), None, 1, None),
            "Note ch1 #21 (Program 2 unverified)"
        );
        assert_eq!(
            name(cc(99, 1), 3, &p4),
            "CC ch1 #99 increase (not on a knob in Program 4)"
        );
        let mut twice = p4;
        twice.1[0x24 + 3] = 37;
        assert_eq!(
            name(note(10, 37), 3, &twice),
            "Note ch10 #37 (on several pads in Program 4)"
        );
        // Piano keys and joysticks need no table; joysticks keep their observed side label.
        let piano = control_name(note(1, 60), Some(Input::Piano), 3, None);
        assert_eq!(piano, "Piano C4 (note 60)");
        let mut joystick = Mapping::new(note(10, 36), "Shift", "hold").unwrap();
        joystick.input = Input::Joystick {
            center: 64,
            min: 0,
            max: 127,
            deadzone: 8,
            hysteresis: 2,
        };
        joystick.label = "Joystick Right".into();
        assert_eq!(joystick.name(Some(&p1)), "Joystick Right");
    }

    #[test]
    fn per_program_idle_migrates_preserves_mappings_and_rejects_piano_feedback() {
        let old = "vibeconsole-mappings-v4\nidle\t0\t1\ton\n";
        let mut config = parse_config(old).unwrap();
        assert_eq!(config.idle_for(0).tempo, Some(1));
        for context in 1..8 {
            assert!(!config.idle_for(context).enabled());
        }
        config
            .set_idle(
                1,
                Settings {
                    octave: Some(-1),
                    tempo: Some(120),
                    arp: Some(false),
                },
            )
            .unwrap();
        let mut pad = Mapping::new(Control::note(1, 21).unwrap(), "Shift", "hold").unwrap();
        pad.context = 1;
        pad.feedback.octave = Some(Octave::Offset(1));
        config.mappings.push(pad);
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-program-idle-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        save_config(&path, &config).unwrap();
        assert_eq!(load_config(&path).unwrap(), config);
        save(&path, &config.mappings).unwrap();
        assert_eq!(load_config(&path).unwrap(), config);
        let before = fs::read(&path).unwrap();
        let mut invalid = config.clone();
        invalid.mappings[0].input = Input::Piano;
        invalid.mappings[0].feedback = Feedback::default();
        assert!(save_config(&path, &invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        invalid = config.clone();
        invalid.program_idle.insert(8, Settings::default());
        assert!(save_config(&path, &invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        let text = String::from_utf8(before).unwrap();
        assert!(parse_config(&text.replace("program-idle\t2", "program-idle\t1")).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn action_families_round_trip_and_invalid_save_preserves_shortcuts() {
        use crate::actions::Action;
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-action-config-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let mut maps =
            vec![Mapping::new(Control::from_note(10, 36).unwrap(), "Shift", "toggle").unwrap()];
        maps.push(
            Mapping::new_action(
                Control::from_note(10, 37).unwrap(),
                Input::Pad,
                Action::Application("/missing/selected.desktop".into()),
            )
            .unwrap(),
        );
        maps.push(
            Mapping::new_action(
                Control::note(1, 48).unwrap(),
                Input::Piano,
                Action::Volume { up: false, step: 5 },
            )
            .unwrap(),
        );
        maps.push(
            Mapping::new_action(
                Control::from_note(10, 38).unwrap(),
                Input::Pad,
                Action::Flow,
            )
            .unwrap(),
        );
        maps.sort_by_key(|m| (m.context, m.control));
        save(&path, &maps).unwrap();
        assert_eq!(load(&path).unwrap(), maps);
        let contents = fs::read(&path).unwrap();
        let mut invalid = maps.clone();
        invalid[0].action = Action::Volume { up: true, step: 0 };
        assert!(save(&path, &invalid).is_err());
        invalid = maps.clone();
        invalid
            .iter_mut()
            .find(|m| m.action.flow())
            .unwrap()
            .behavior = Behavior::Toggle;
        assert!(save(&path, &invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), contents);
        assert!(load(&path).unwrap().iter().any(|m| m.keys == ["Shift"]));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn control_profiles_round_trip_and_invalid_or_ambiguous_saves_preserve_data() {
        use crate::Message;
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-profile-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let mut piano = Mapping::new(Control::note(1, 48).unwrap(), "Shift", "hold").unwrap();
        piano.input = Input::Piano;
        piano.label = "Piano note 48".into();
        let mut knob = Mapping::new(
            Control {
                message: Message::Cc,
                channel: 1,
                id: 70,
                direction: 1,
            },
            "Ctrl+K",
            "pulse",
        )
        .unwrap();
        knob.input = Input::Knob { step: 4 };
        knob.label = "Knob 1 increase".into();
        let mut joystick = Mapping::new(
            Control {
                message: Message::Bend,
                channel: 1,
                id: 0,
                direction: 1,
            },
            "Alt",
            "hold",
        )
        .unwrap();
        joystick.input = Input::Joystick {
            center: 8192,
            min: 0,
            max: 16383,
            deadzone: 1024,
            hysteresis: 128,
        };
        joystick.label = "Joystick Right".into();
        let valid = Config {
            mappings: vec![piano.clone(), knob.clone(), joystick.clone()],
            idle: Settings::default(),
            ..Config::default()
        };
        save_config(&path, &valid).unwrap();
        let mut expected = valid.clone();
        expected.mappings.sort_by_key(|m| (m.context, m.control));
        assert_eq!(load_config(&path).unwrap(), expected);
        let mut relative = valid.clone();
        relative.mappings[1].input = Input::RelativeKnob { step: 2 };
        save_config(&path, &relative).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("vibeconsole-mappings-v22\n")
        );
        assert!(
            load_config(&path)
                .unwrap()
                .mappings
                .iter()
                .any(|m| m.input == Input::RelativeKnob { step: 2 })
        );
        save_config(&path, &valid).unwrap();
        let before = fs::read(&path).unwrap();
        for input in [
            Input::Knob { step: 0 },
            Input::Joystick {
                center: 20000,
                min: 0,
                max: 16383,
                deadzone: 10,
                hysteresis: 1,
            },
            Input::Joystick {
                center: 8192,
                min: 0,
                max: 16383,
                deadzone: 10,
                hysteresis: 10,
            },
        ] {
            let mut bad = valid.clone();
            bad.mappings[1].input = input;
            assert!(save_config(&path, &bad).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        let mut ambiguous = valid.clone();
        let mut other = knob;
        other.control.direction = -1;
        other.label = "Knob 1 decrease".into();
        other.input = Input::Knob { step: 8 };
        ambiguous.mappings.push(other);
        assert!(save_config(&path, &ambiguous).is_err());
        let mut musical = valid.clone();
        musical.idle.arp = Some(true);
        assert!(save_config(&path, &musical).is_err());
        musical.idle = Settings::default();
        musical.mappings[0].feedback.octave = Some(Octave::Offset(1));
        assert!(save_config(&path, &musical).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        let mut isolated = valid;
        isolated.mappings[0].context = 1;
        isolated.idle.tempo = Some(120);
        isolated.idle.arp = Some(true);
        save_config(&path, &isolated).unwrap();
        let saved = load_config(&path).unwrap();
        assert_eq!(saved.idle, isolated.idle);
        assert!(
            saved
                .mappings
                .iter()
                .any(|m| m.context == 1 && m.input == Input::Piano)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn contexts_preserve_original_and_reject_ambiguous_or_failed_migrations() {
        let original =
            parse_config("vibeconsole-mappings-v1\n09e8:1049\tnote\t10\t36\thold\tShift\n")
                .unwrap();
        // Pre-rename KeyAI files still load.
        let legacy = parse_config("keyai-mappings-v1\n09e8:1049\tnote\t10\t36\thold\tShift\n");
        assert_eq!(legacy.unwrap(), original);
        assert!(parse_config("keyai-mappings-v+5\n").is_err());
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-context-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let mut config = original.clone();
        let mut custom = Mapping::new(Control::note(3, 60).unwrap(), "Ctrl", "toggle").unwrap();
        custom.context = 1;
        custom.label = "Bank B Pad 3".into();
        config.mappings.push(custom.clone());
        save_config(&path, &config).unwrap();
        assert_eq!(load_config(&path).unwrap(), config);
        let before = fs::read(&path).unwrap();
        config.mappings.push(custom.clone());
        assert!(save_config(&path, &config).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        config.mappings.pop();
        custom.control = Control::note(3, 61).unwrap();
        config.mappings.push(custom);
        assert!(save_config(&path, &config).is_err()); // same physical label, different messages
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn earlier_config_migrates_with_feedback_and_idle_and_failed_saves_preserve_it() {
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-migration-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        fs::write(
            &path,
            "vibeconsole-mappings-v1\n09e8:1049\tnote\t10\t36\thold\tShift\n",
        )
        .unwrap();
        let mut config = load_config(&path).unwrap();
        assert_eq!(config.mappings[0].feedback, Feedback::default());
        config.mappings[0].feedback = Feedback {
            octave: Some(Octave::Offset(1)),
            tempo: Some(50),
            arp: Some(true),
        };
        config.idle = Settings {
            octave: Some(0),
            tempo: Some(120),
            arp: Some(false),
        };
        save_config(&path, &config).unwrap();
        assert_eq!(load_config(&path).unwrap(), config);
        let mut other =
            Mapping::new(Control::from_note(10, 44).unwrap(), "Ctrl", "toggle").unwrap();
        other.feedback.octave = Some(Octave::Absolute(-1));
        let mut mappings = config.mappings.clone();
        mappings.push(other);
        save(&path, &mappings).unwrap();
        assert_eq!(load_config(&path).unwrap().idle, config.idle);
        let before = fs::read(&path).unwrap();
        let mut bad = load_config(&path).unwrap();
        bad.mappings[0].feedback.arp = Some(false);
        assert!(save_config(&path, &bad).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        for bad in [
            "vibeconsole-mappings-v2\n",
            "vibeconsole-mappings-v2\nidle\t5\t120\ton\n",
            "vibeconsole-mappings-v2\nidle\t-\t120\tyes\n",
            "vibeconsole-mappings-v2\nidle\t-\t-\t-\n09e8:1049\tnote\t10\t36\thold\tShift\toffset:5\t-\t-\n",
        ] {
            assert!(parse_config(bad).is_err());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    /// v7 ticket 04: a v15 file loads unchanged with safe tuning defaults and saves as v16;
    /// tuning round-trips; invalid tuning is refused; a failed save keeps the v15 file.
    #[test]
    fn v15_config_migrates_to_v16_with_safe_tuning() {
        const V15: &str = "vibeconsole-mappings-v15\nidle\t-\t-\t-\nprogram-idle\t1\t-\t-\t-\nprogram-idle\t2\t-\t-\t-\nprogram-idle\t3\t-\t-\t-\nprogram-idle\t4\t-\t-\t-\nprogram-idle\t5\t-\t-\t-\nprogram-idle\t6\t-\t-\t-\nprogram-idle\t7\t-\t-\t-\nstart-flow\ton\nagent-dir\t-\nenv\t0\nvars\t0\nstatus-lights\ton\nlight-tempos\t50\t240\nlight-pattern\tneeds-decision\tfast\talternate\nlight-pattern\tneeds-input\tfast\tsteady\nlight-pattern\tthinking\tslow\toff\nlight-pattern\tfinished\toff\tblink\nlight-other\toff\t2\nlight-folders\t0\nagents\t2\nagent\tclaude\tclaude\tclaude\t-\t1\nagent\tcodex\tcodex\tcodex\t-\t-1\n0\tBank A Pad 1\tpad\t09e8:1049\tnote\t10\t36\t0\thold\tShift\t-\t-\t-\tshortcut\t-\t-\n";
        let dir = std::env::temp_dir().join(format!("vibeconsole-v16-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        fs::write(&path, V15).unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(config.mappings.len(), 1);
        assert_eq!(config.mappings[0].keys, ["Shift"]);
        // Safe defaults: rumble off.
        assert_eq!(
            config.tuning,
            Tuning {
                dead_zone: 15,
                trigger: 30,
                mouse_speed: 10,
                rumble: false,
                mouse: MouseStick::Off,
                sticks: Default::default(),
            }
        );
        // A failed save leaves the v15 file byte-identical.
        let mut bad = config.clone();
        bad.tuning.dead_zone = 95;
        assert!(save_config(&path, &bad).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), V15);
        // Saving writes v16 with the tuning line; it round-trips.
        let mut tuned = config.clone();
        tuned.tuning = Tuning {
            dead_zone: 20,
            trigger: 60,
            mouse_speed: 25,
            rumble: true,
            mouse: MouseStick::Off,
            sticks: Default::default(),
        };
        save_config(&path, &tuned).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("vibeconsole-mappings-v22\n"));
        assert!(text.contains("\ntuning\t20\t60\t25\ton\toff\n"));
        assert_eq!(load_config(&path).unwrap(), tuned);
        for bad in [
            Tuning {
                dead_zone: 91,
                ..tuned.tuning.clone()
            },
            Tuning {
                trigger: 0,
                ..tuned.tuning.clone()
            },
            Tuning {
                trigger: 101,
                ..tuned.tuning.clone()
            },
            Tuning {
                mouse_speed: 0,
                ..tuned.tuning.clone()
            },
            Tuning {
                mouse_speed: 51,
                ..tuned.tuning.clone()
            },
        ] {
            assert!(bad.clone().validate().is_err(), "{bad:?}");
        }
        // A v16 file without its tuning line, or with a bad one, is refused.
        let v16 = text.clone();
        assert!(parse_config(&v16.replace("tuning\t20\t60\t25\ton\toff\n", "")).is_err());
        assert!(
            parse_config(&v16.replace("tuning\t20\t60\t25\ton", "tuning\t20\t60\t25\tmaybe"))
                .is_err()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// A line only a v20 file has (v7 ticket 86), dropped when a test writes an older one.
    fn v20_record(line: &str) -> bool {
        ["device-lights\t", "xbox-pattern\t", "ps5-pattern\t"]
            .iter()
            .any(|p| line.starts_with(p))
    }

    /// v7 ticket 86: per-device light switches and patterns round-trip in v20; a v19 file
    /// loads today's MPK patterns and the board's Xbox and DualSense defaults; a bad word is
    /// refused and a refused save keeps the last file.
    #[test]
    fn device_light_patterns_round_trip_in_v20() {
        use crate::status::{DevicePatterns, Light, Rumble};
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-dev-lights-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let mut config = Config::default();
        assert_eq!(config.device_lights, DevicePatterns::default());
        save_config(&path, &config).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("vibeconsole-mappings-v22\n"), "{text}");
        assert!(
            text.contains(concat!(
                "device-lights\ton\ton\ton\n",
                "xbox-pattern\tneeds-decision\tfast-blink\tshort\n",
                "xbox-pattern\tneeds-input\tfast-blink\toff\n",
                "xbox-pattern\tthinking\tslow-pulse\toff\n",
                "xbox-pattern\tfinished\tblink\tshort\n",
                "ps5-pattern\tneeds-decision\tred\tfast-blink\tshort\n",
            )),
            "{text}"
        );
        let v19 = text
            .replace("vibeconsole-mappings-v22", "vibeconsole-mappings-v19")
            .lines()
            .filter(|l| !v20_record(l))
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        assert_eq!(
            parse_config(&v19).unwrap(),
            config,
            "v19 loads the defaults"
        );
        config.device_lights.on[1] = false;
        config.device_lights.xbox[2] = (Light::Steady, Rumble::Long);
        config.device_lights.ps5[3] = ("purple", Light::Off, Rumble::Off);
        save_config(&path, &config).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert_eq!(load_config(&path).unwrap(), config);
        for (from, to) in [
            (
                "xbox-pattern\tthinking\tsteady",
                "xbox-pattern\tthinking\tdisco",
            ),
            ("\tlong\n", "\tforever\n"),
            ("\tpurple\t", "\tbeige\t"),
            ("device-lights\ton\toff", "device-lights\ton\tmaybe"),
            ("xbox-pattern\tthinking", "xbox-pattern\tidle"),
        ] {
            assert!(parse_config(&saved.replace(from, to)).is_err(), "{to}");
        }
        let mut bad = config.clone();
        bad.device_lights.ps5[0].0 = "beige";
        assert!(save_config(&path, &bad).is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            saved,
            "the last valid file stays"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// v7 ticket 81: a sequence step's own text is saved with the sequence (config v19) and
    /// round-trips; a v18 file loads with no step text; a refused save keeps the last file.
    /// v7 ticket 98: Orca keys round-trip in v22; an older file can't carry them, and its
    /// Orca "needs me" sequence loads as Orca keys while other sequences stay as they are.
    #[test]
    fn orca_keys_round_trip_in_v22_and_old_needs_me_migrates() {
        use crate::actions::{Action, Step};
        let pad = Control::from_note(10, 36).unwrap();
        let other = Control::from_note(10, 37).unwrap();
        let mut config = Config::default();
        config.mappings = vec![
            Mapping::new_action(pad, Input::Pad, Action::OrcaKeys(orca_needs_me_steps())).unwrap(),
            Mapping::new_action(
                other,
                Input::Pad,
                Action::Sequence(vec![Step::Keys(vec!["Ctrl".into(), "J".into()])]),
            )
            .unwrap(),
        ];
        let text = serialize_config(&config).unwrap();
        assert!(text.starts_with("vibeconsole-mappings-v22\n"), "{text}");
        assert_eq!(parse_config(&text).unwrap(), config);
        let v21 = text.replace("vibeconsole-mappings-v22", "vibeconsole-mappings-v21");
        assert!(parse_config(&v21).unwrap_err().to_string().contains("v22"));
        // An old needs-me pad (a plain sequence) becomes Orca keys; another sequence doesn't.
        let old = v21.replace("orca-keys:", "sequence:");
        let loaded = parse_config(&old).unwrap();
        assert_eq!(
            loaded.mappings[0].action,
            Action::OrcaKeys(orca_needs_me_steps())
        );
        assert_eq!(loaded.mappings[1].action, config.mappings[1].action);
    }

    /// v7 ticket 96: the one-tap behavior round-trips in v21; a v20 file can't carry it, and
    /// older files load unchanged.
    #[test]
    fn one_tap_round_trips_in_v21_and_older_files_cannot_hold_it() {
        let pad = Control::from_note(10, 36).unwrap();
        let mut config = Config::default();
        config.mappings = vec![Mapping::new(pad, "Ctrl+T", "tap").unwrap()];
        let text = serialize_config(&config).unwrap();
        assert!(text.starts_with("vibeconsole-mappings-v22\n"), "{text}");
        assert!(text.contains("\ttap\tCtrl+T\t"), "{text}");
        assert_eq!(parse_config(&text).unwrap(), config);
        let v20 = text.replace("vibeconsole-mappings-v22", "vibeconsole-mappings-v20");
        assert!(parse_config(&v20).unwrap_err().to_string().contains("v21"));
        config.mappings = vec![Mapping::new(pad, "Ctrl+T", "hold").unwrap()];
        let old = serialize_config(&config)
            .unwrap()
            .replace("vibeconsole-mappings-v22", "vibeconsole-mappings-v20");
        assert_eq!(
            parse_config(&old).unwrap(),
            config,
            "a v20 file loads as is"
        );
    }

    #[test]
    fn sequence_step_text_round_trips_in_v19() {
        use crate::actions::{Action, Target};
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-step-text-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let saved = |file: &str| Action::Prompt {
            file: file.into(),
            target: Target::Agent("claude".into()),
            enter: true,
        };
        let pad = Control::from_note(10, 36).unwrap();
        let plain = Action::PromptSequence(vec![saved("plan"), saved("build")]);
        let mut config = Config::default();
        config.mappings = vec![Mapping::new_action(pad, Input::Pad, plain).unwrap()];
        save_config(&path, &config).unwrap();
        let v18 = fs::read_to_string(&path)
            .unwrap()
            .replace("vibeconsole-mappings-v22", "vibeconsole-mappings-v18")
            .lines()
            .filter(|l| !v20_record(l))
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        assert_eq!(
            parse_config(&v18).unwrap(),
            config,
            "a v18 file loads as is"
        );
        let own = Action::PromptOwn {
            file: "plan".into(),
            target: Target::Agent("claude".into()),
            enter: true,
            text: "Plan it:\n\t1. read\n".into(),
        };
        config.mappings[0].action = Action::PromptSequence(vec![own, saved("build")]);
        save_config(&path, &config).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("vibeconsole-mappings-v22\n"), "{text}");
        assert_eq!(load_config(&path).unwrap(), config);
        let mut bad = config.clone();
        bad.mappings[0].action = Action::PromptSequence(vec![Action::PromptOwn {
            file: "plan".into(),
            target: Target::Agent("claude".into()),
            enter: true,
            text: "  ".into(),
        }]);
        assert!(save_config(&path, &bad).is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            text,
            "the last valid file stays"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// v7 ticket 75: each stick's mode and ranges round-trip in v18; overlapping or bad
    /// ranges and an angle-mode mouse stick are refused, and a refused save keeps the file;
    /// a v17 file loads both sticks as Linear.
    #[test]
    fn stick_modes_round_trip_and_bad_ranges_are_refused() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-sticks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let mut config = Config::default();
        config.tuning.sticks = [
            StickMode::sectors(),
            StickMode::Angle(vec![(50, 80), (330, 30)]),
        ];
        save_config(&path, &config).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(
            "\nsticks\tangle:0-45,45-90,90-135,135-180,180-225,225-270,270-315,315-0\tangle:50-80,330-30\n"
        ));
        assert_eq!(load_config(&path).unwrap(), config);
        let v17 = text
            .replace("vibeconsole-mappings-v22", "vibeconsole-mappings-v17")
            .lines()
            .filter(|l| !v20_record(l))
            .map(|l| format!("{l}\n"))
            .collect::<String>()
            .lines()
            .filter(|l| !l.starts_with("sticks\t"))
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        assert_eq!(
            parse_config(&v17).unwrap().tuning.sticks,
            [StickMode::Linear, StickMode::Linear]
        );
        let refused = |sticks: [StickMode; 2], mouse: MouseStick| {
            let mut bad = config.clone();
            bad.tuning.sticks = sticks;
            bad.tuning.mouse = mouse;
            let error = save_config(&path, &bad).unwrap_err().to_string();
            assert_eq!(fs::read_to_string(&path).unwrap(), text, "{error}");
            error
        };
        let angle = |r: Vec<(u16, u16)>| StickMode::Angle(r);
        let linear = StickMode::Linear;
        assert!(
            refused(
                [angle(vec![(0, 90), (45, 100)]), linear.clone()],
                MouseStick::Off
            )
            .contains("overlap at 45°")
        );
        assert!(
            refused(
                [linear.clone(), angle(vec![(300, 20), (10, 40)])],
                MouseStick::Off
            )
            .contains("overlap at 10°")
        );
        assert!(
            refused([angle(vec![(10, 10)]), linear.clone()], MouseStick::Off)
                .contains("not 0–360°")
        );
        assert!(
            refused([angle(vec![(400, 10)]), linear.clone()], MouseStick::Off)
                .contains("not 0–360°")
        );
        assert!(refused([angle(vec![]), linear.clone()], MouseStick::Off).contains("1–16"));
        assert!(
            refused([angle(vec![(0, 1); 17]), linear.clone()], MouseStick::Off).contains("1–16")
        );
        assert!(
            refused([StickMode::sectors(), linear.clone()], MouseStick::Left)
                .contains("moves the pointer")
        );
        for bad in [
            "sticks\tdiagonal\tlinear",
            "sticks\tangle:10\tlinear",
            "sticks\tangle:a-b\tlinear",
            "sticks\tlinear",
        ] {
            let edited = text
                .lines()
                .map(|l| {
                    if l.starts_with("sticks\t") {
                        bad.to_string()
                    } else {
                        l.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(parse_config(&edited).is_err(), "{bad}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// v7 ticket 49: an agent has a host (tmux, Orca or herdr). A v16 file's agents load as
    /// tmux and save as v19 with the host; each host round-trips; a host's own name rules
    /// are checked; an unknown host is refused.
    #[test]
    fn agents_keep_their_host_and_v16_agents_are_tmux() {
        let mut config = Config::default();
        config.agents = vec![Agent::new("claude", "claude", "claude").unwrap()];
        let v16 = serialize_config(&config)
            .unwrap()
            .replace("vibeconsole-mappings-v22", "vibeconsole-mappings-v16")
            .lines()
            .filter(|l| !v20_record(l))
            .map(|l| format!("{l}\n"))
            .collect::<String>()
            .replace("sticks\tlinear\tlinear\n", "")
            .replace("\t1\ttmux\n", "\t1\n");
        let old = parse_config(&v16).unwrap();
        assert_eq!(old.agents[0].host, Host::Tmux);
        let mut orca = Agent::new("orca-claude", "claude", "orca-claude").unwrap();
        orca.host = Host::Orca;
        orca.dir = Some("/work/app".into());
        let mut herdr = Agent::new("api", "api", "api").unwrap();
        herdr.host = Host::Herdr;
        config.agents.extend([orca, herdr]);
        let text = serialize_config(&config).unwrap();
        assert!(text.starts_with("vibeconsole-mappings-v22\n"), "{text}");
        assert!(
            text.contains("\nagent\tclaude\tclaude\tclaude\t-\t1\ttmux\n"),
            "{text}"
        );
        assert!(
            text.contains("\tclaude\torca-claude\t/work/app\t1\torca\n"),
            "{text}"
        );
        assert_eq!(parse_config(&text).unwrap(), config);
        assert!(parse_config(&text.replace("\t1\torca\n", "\t1\tt3\n")).is_err());
        assert!(parse_config(&text.replace("\t1\torca\n", "\t1\n")).is_err());
        // An Orca agent's kind is one simple name (it is passed to `orca agent launch`).
        let mut bad = config.clone();
        bad.agents[1].launch = "claude --yolo".into();
        assert!(serialize_config(&bad).is_err());
        // A herdr agent's name follows herdr's rule.
        let mut bad = config.clone();
        bad.agents[2].name = "API".into();
        bad.agents[2].window = "API".into();
        assert!(serialize_config(&bad).is_err());
    }

    /// v7 ticket 06: other MIDI devices are listed by name; each gets its own context, so
    /// their mappings never mix with the Akai's programs or each other.
    #[test]
    fn other_midi_devices_round_trip_with_their_own_contexts() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-devices-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let mut config = Config::default();
        assert_eq!(config.device_context("Launchkey"), None);
        config.midi_devices = vec!["Launchkey".into(), "nanoPAD2".into()];
        config.midi_active = Some("nanoPAD2".into());
        let context = config.device_context("nanoPAD2").unwrap();
        assert_eq!(context, DEVICE_CONTEXT + 1);
        let mut mapping = Mapping::new(Control::note(1, 60).unwrap(), "F5", "hold").unwrap();
        mapping.context = context;
        mapping.label = "nanoPAD2 note 60".into();
        config.mappings = vec![mapping];
        save_config(&path, &config).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\nmidi-devices\t2\nmidi-device\tLaunchkey\nmidi-device\tnanoPAD2\nmidi-active\tnanoPAD2\n"));
        assert!(text.contains("\tmidi\tnote\t1\t60\t"));
        assert_eq!(load_config(&path).unwrap(), config);
        assert_eq!(program_label(context), "Other MIDI device 2");
        // Refused: a mapping for a device that isn't listed, an active device that isn't
        // listed, a name that could break the file, and feedback on another device.
        let mut bad = config.clone();
        bad.mappings[0].context = DEVICE_CONTEXT + 2;
        assert!(save_config(&path, &bad).is_err());
        let mut bad = config.clone();
        bad.midi_active = Some("Missing".into());
        assert!(save_config(&path, &bad).is_err());
        for name in ["", "-", "a\tb", "line\nbreak", &"x".repeat(65)] {
            let mut bad = config.clone();
            bad.midi_devices[0] = name.into();
            assert!(save_config(&path, &bad).is_err(), "{name:?}");
        }
        let mut bad = config.clone();
        bad.mappings[0].feedback.tempo = Some(120);
        assert!(save_config(&path, &bad).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        fs::remove_dir_all(dir).unwrap();
    }

    /// v7 ticket 12: an app profile holds overrides only. Its pads and buttons replace
    /// the Default ones in the same context; everything else keeps its Default meaning.
    #[test]
    fn app_profiles_round_trip_and_override_only_what_they_set() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-profiles-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let pad1 = Control::from_note(10, 36).unwrap();
        let pad2 = Control::from_note(10, 37).unwrap();
        let mut config = Config::default();
        config.mappings = vec![
            Mapping::new(pad1, "F1", "hold").unwrap(),
            Mapping::new(pad2, "F2", "hold").unwrap(),
        ];
        let mut firefox_pad1 = Mapping::new(pad1, "Ctrl+T", "hold").unwrap();
        firefox_pad1.label = "Bank A Pad 1".into();
        config.profiles = vec![Profile {
            name: "Firefox".into(),
            apps: vec![
                "firefox.desktop".into(),
                "org.mozilla.firefox.desktop".into(),
            ],
            mappings: vec![firefox_pad1],
        }];
        save_config(&path, &config).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(
            "\nprofiles\t1\nprofile\tFirefox\tfirefox.desktop,org.mozilla.firefox.desktop\t1\n0\t"
        ));
        let loaded = load_config(&path).unwrap();
        assert_eq!(loaded, config);
        assert_eq!(
            loaded.profile_for("firefox.desktop").unwrap().name,
            "Firefox"
        );
        assert!(loaded.profile_for("org.gnome.Ptyxis.desktop").is_none());
        let profile = loaded.profile_for("firefox.desktop");
        let set = overlay(&loaded.mappings, profile, 0);
        assert_eq!(set.len(), 2);
        assert_eq!(
            set.iter().find(|m| m.control == pad1).unwrap().keys,
            ["Ctrl", "T"]
        );
        assert_eq!(set.iter().find(|m| m.control == pad2).unwrap().keys, ["F2"]);
        assert_eq!(overlay(&loaded.mappings, None, 0), loaded.mappings);
        // Refused, keeping the file: bad names and app IDs, an app in two profiles, and
        // overrides that aren't plain pads/buttons without feedback.
        let refused = |change: &dyn Fn(&mut Config)| {
            let mut bad = config.clone();
            change(&mut bad);
            assert!(save_config(&path, &bad).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
        };
        refused(&|c| c.profiles[0].name = "".into());
        refused(&|c| c.profiles[0].name = "a\tb".into());
        refused(&|c| c.profiles[0].apps = vec!["../evil.desktop".into()]);
        refused(&|c| c.profiles[0].apps = vec!["firefox.desktop; rm -rf ~".into()]);
        refused(&|c| {
            let mut other = c.profiles[0].clone();
            other.name = "Other".into();
            other.mappings.clear();
            c.profiles.push(other);
        });
        refused(&|c| c.profiles[0].mappings[0].feedback.tempo = Some(120));
        fs::remove_dir_all(dir).unwrap();
    }

    /// v7 ticket 12: a focus change is a transition: a key held under Default is released,
    /// its release sends nothing, and the next press uses the profile's action.
    #[test]
    fn focus_change_releases_held_keys_and_the_next_press_uses_the_profile() {
        use evdev::KeyCode;
        let pad = Control::from_note(10, 36).unwrap();
        let default = vec![Mapping::new(pad, "F1", "hold").unwrap()];
        let profile = Profile {
            name: "Firefox".into(),
            apps: vec!["firefox.desktop".into()],
            mappings: vec![Mapping::new(pad, "F2", "hold").unwrap()],
        };
        let mut keyboard = crate::keyboard::Keyboard::new(&overlay(&default, None, 0)).unwrap();
        keyboard.resume();
        let mut sent = Vec::new();
        let mut emit = |key, down| {
            sent.push((key, down));
            Ok(())
        };
        keyboard
            .observe(crate::Event::Press(pad), &mut emit)
            .unwrap();
        // Firefox focused: what the session does on a profile change.
        keyboard.pause(&mut emit).unwrap();
        keyboard
            .prepare_connection(
                &overlay(&default, Some(&profile), 0),
                &[pad].into(),
                &mut false,
            )
            .unwrap();
        keyboard.resume();
        keyboard
            .observe(
                crate::Event::Release(pad, crate::ReleaseType::NoteOff),
                &mut emit,
            )
            .unwrap();
        keyboard
            .observe(crate::Event::Press(pad), &mut emit)
            .unwrap();
        assert_eq!(
            sent,
            [
                (KeyCode::KEY_F1, true),
                (KeyCode::KEY_F1, false),
                (KeyCode::KEY_F2, true),
            ]
        );
    }

    /// v7 ticket 13: profiles cycle Default -> each profile -> Default; a saved lock pins
    /// one (or Default) whatever app is focused; "Default" can't name a profile.
    #[test]
    fn profiles_cycle_and_a_saved_lock_ignores_focus() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-lock-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let profile = |name: &str, app: &str| Profile {
            name: name.into(),
            apps: vec![app.into()],
            mappings: Vec::new(),
        };
        let mut config = Config::default();
        config.profiles = vec![
            profile("Orca", "orca.desktop"),
            profile("Firefox", "firefox.desktop"),
        ];
        assert_eq!(config.cycle(None, true).as_deref(), Some("Orca"));
        assert_eq!(config.cycle(Some("Orca"), true).as_deref(), Some("Firefox"));
        assert_eq!(config.cycle(Some("Firefox"), true), None);
        assert_eq!(config.cycle(None, false).as_deref(), Some("Firefox"));
        assert_eq!(
            config.choice(Some("firefox.desktop")).as_deref(),
            Some("Firefox")
        );
        assert_eq!(config.choice(Some("gedit.desktop")), None);
        config.lock = Some(Lock::Profile("Orca".into()));
        assert_eq!(
            config.choice(Some("firefox.desktop")).as_deref(),
            Some("Orca")
        );
        save_config(&path, &config).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("\nprofile-lock\tOrca\n")
        );
        assert_eq!(load_config(&path).unwrap(), config);
        config.lock = Some(Lock::Default);
        assert_eq!(config.choice(Some("firefox.desktop")), None);
        save_config(&path, &config).unwrap();
        assert_eq!(load_config(&path).unwrap().lock, Some(Lock::Default));
        let mut bad = config.clone();
        bad.lock = Some(Lock::Profile("Missing".into()));
        assert!(save_config(&path, &bad).is_err());
        let mut bad = config.clone();
        bad.profiles[0].name = "default".into();
        assert!(save_config(&path, &bad).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    /// v7 ticket 14: a profile exports to one file and imports back the same; an import is
    /// checked in full and refused whole; file names stay inside their folder.
    #[test]
    fn profiles_export_and_import_whole_or_not_at_all() {
        let pad = Control::from_note(10, 36).unwrap();
        let profile = Profile {
            name: "Firefox".into(),
            apps: vec!["firefox.desktop".into()],
            mappings: vec![Mapping::new(pad, "Ctrl+T", "hold").unwrap()],
        };
        let text = export_profile(&profile).unwrap();
        assert!(text.starts_with("vibeconsole-profile-v1\nprofile\tFirefox\tfirefox.desktop\t1\n"));
        let config = Config::default();
        assert_eq!(import_profile(&text, &config).unwrap(), profile);
        // A name that exists, an app another profile has, a bad action or name, extra or
        // missing lines, another file type: all refused.
        let mut taken = config.clone();
        taken.profiles.push(profile.clone());
        assert!(import_profile(&text, &taken).is_err());
        let mut app_taken = taken.clone();
        app_taken.profiles[0].name = "Other".into();
        assert!(import_profile(&text, &app_taken).is_err());
        for bad in [
            text.replace("Ctrl+T", "Ctrl+Bogus"),
            text.replace("\tshortcut\t", "\tcommand:x\t"),
            text.replace("profile\tFirefox", "profile\tDefault"),
            text.replace("firefox.desktop", "../../etc/passwd"),
            text.replace("\t1\n", "\t2\n"),
            format!("{text}0\textra\n"),
            text.replace("vibeconsole-profile-v1", "vibeconsole-mappings-v16"),
            String::new(),
        ] {
            assert!(import_profile(&bad, &config).is_err(), "{bad:?}");
        }
        assert_eq!(profile_file_name("Firefox"), "Firefox.profile");
        assert_eq!(profile_file_name("../evil/x"), "evil-x.profile");
        assert_eq!(profile_file_name("Orca: agents"), "Orca-agents.profile");
    }

    /// v7 ticket 24: favourites round-trip and survive an edited action.
    #[test]
    fn favourites_round_trip_and_follow_the_control() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-favs-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        let pad = Control::from_note(10, 36).unwrap();
        let mapping = Mapping::new(pad, "F1", "hold").unwrap();
        let mut config = Config::default();
        config.mappings = vec![mapping.clone()];
        config.favourites = vec![favourite_key(&mapping), profile_favourite_key("Orca")];
        save_config(&path, &config).unwrap();
        assert_eq!(load_config(&path).unwrap(), config);
        let edited = Mapping::new(pad, "F2", "toggle").unwrap();
        assert_eq!(favourite_key(&edited), favourite_key(&mapping));
        let mut bad = config.clone();
        bad.favourites.push(bad.favourites[0].clone());
        assert!(save_config(&path, &bad).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    /// v7 ticket 25: a preset brings back mappings, profiles and their devices; settings
    /// stay; a lock on a profile the preset lacks is dropped.
    #[test]
    fn presets_bring_back_the_setup_and_keep_settings() {
        let pad = Control::from_note(10, 36).unwrap();
        let mut preset = Config::default();
        preset.mappings = vec![Mapping::new(pad, "F1", "hold").unwrap()];
        let mut config = Config::default();
        config.start_flow = false;
        config.profiles = vec![Profile {
            name: "Orca".into(),
            apps: vec!["orca.desktop".into()],
            mappings: Vec::new(),
        }];
        config.lock = Some(Lock::Profile("Orca".into()));
        let next = apply_preset(&config, &preset);
        assert_eq!(next.mappings, preset.mappings);
        assert!(next.profiles.is_empty());
        assert_eq!(next.lock, None);
        assert!(!next.start_flow, "settings stay");
        assert_eq!(preset_file_name("Work: day"), "Work-day.preset");
        assert_eq!(preset_file_name("../x"), "x.preset");
    }

    /// v7 ticket 26: the profile is cut out of Claude's reply and checked like an import.
    #[test]
    fn claudes_reply_is_cut_to_the_profile_and_checked_like_an_import() {
        let pad = Control::from_note(10, 36).unwrap();
        let profile = Profile {
            name: "Firefox".into(),
            apps: vec!["firefox.desktop".into()],
            mappings: vec![Mapping::new(pad, "Ctrl+T", "hold").unwrap()],
        };
        let file = export_profile(&profile).unwrap();
        let reply = format!("Here is your profile:\n\n```\n{file}```\nEnjoy!\n");
        let text = profile_text_from_reply(&reply).unwrap();
        assert_eq!(text, file);
        let config = Config::default();
        assert_eq!(import_profile(&text, &config).unwrap(), profile);
        assert_eq!(profile_text_from_reply("Sorry, I can't."), None);
        let broken = reply.replace("Ctrl+T", "Ctrl+Nope");
        assert!(import_profile(&profile_text_from_reply(&broken).unwrap(), &config).is_err());
        let mut with_pads = Config::default();
        with_pads.mappings = vec![Mapping::new(pad, "F1", "hold").unwrap()];
        let request = profile_request(&with_pads, "firefox.desktop", "Tabs on the pads.").unwrap();
        assert!(request.contains("vibeconsole-profile-v1") && request.contains("Bank A Pad 1"));
        // v7 ticket 104: a setup whose first mappings are a knob and a lit pad (like the user's)
        // still makes a request; its example holds only what a profile may.
        let knob = Control {
            channel: 1,
            id: 20,
            message: crate::Message::Cc,
            direction: 1,
        };
        let mut turn = Mapping::new(knob, "Right", "pulse").unwrap();
        turn.input = Input::RelativeKnob { step: 1 };
        let mut lit = Mapping::new(Control::from_note(10, 37).unwrap(), "Shift", "hold").unwrap();
        lit.feedback.octave = Some(crate::feedback::Octave::Offset(-1));
        let mut users = Config::default();
        users.mappings = vec![turn, lit, Mapping::new(pad, "F1", "hold").unwrap()];
        let request = profile_request(&users, "firefox.desktop", "Tabs.").unwrap();
        // The example sits between the header and the rules; the controls list follows them.
        let example = request
            [request.find(PROFILE_HEADER).unwrap()..request.find("Rules:").unwrap()]
            .to_owned();
        assert!(
            example.contains("\tF1\t") && !example.contains("Shift") && !example.contains("Right"),
            "{example}"
        );
        // No pad at all: a stand-in pad still shows the format.
        let request = profile_request(&Config::default(), "firefox.desktop", "Tabs.").unwrap();
        assert!(
            profile_text_from_reply(&request)
                .unwrap()
                .contains("Ctrl+T")
        );
    }

    /// Review fix: a v16 file written before devices, profiles, the lock or favourites
    /// existed still loads, with their safe defaults.
    #[test]
    fn early_v16_files_load_with_defaults_for_later_records() {
        let mut config = Config::default();
        config.mappings =
            vec![Mapping::new(Control::from_note(10, 36).unwrap(), "F1", "hold").unwrap()];
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-early-v16-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        save_config(&path, &config).unwrap();
        let full = fs::read_to_string(&path).unwrap();
        let early = full.replace(
            "midi-devices\t0\nmidi-active\t-\nprofiles\t0\nprofile-lock\t-\nfavourites\t0\n",
            "",
        );
        assert_ne!(early, full);
        assert_eq!(parse_config(&early).unwrap(), config);
        let without_favourites = full.replace("favourites\t0\n", "");
        assert_eq!(parse_config(&without_favourites).unwrap(), config);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn round_trip_replacement_and_failures_preserve_saved_mappings() {
        let directory =
            std::env::temp_dir().join(format!("vibeconsole-config-test-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("mappings.tsv");
        assert!(load(&path).unwrap().is_empty());
        let pad = Control::from_note(10, 36).unwrap();
        let mapping = Mapping::new(pad, "shift", "hold").unwrap();
        save(&path, &[mapping.clone()]).unwrap();
        assert_eq!(load(&path).unwrap(), vec![mapping]);

        let replacement = Mapping::new(pad, "k + control + alt", "toggle").unwrap();
        assert_eq!(replacement.keys.join("+"), "Ctrl+Alt+K");
        let bank_b = Mapping::new(Control::from_note(10, 44).unwrap(), "F12", "hold").unwrap();
        let valid = vec![replacement.clone(), bank_b];
        save(&path, &valid).unwrap();
        assert_eq!(load(&path).unwrap(), valid);
        let previous = fs::read(&path).unwrap();

        for shortcut in ["", "Ctrl++K", "Ctrl+Control", "F13", "shell:ls", "Ctrl+☃"] {
            assert!(Mapping::new(pad, shortcut, "hold").is_err());
        }
        assert!(Mapping::new(pad, "Shift", "macro").is_err());
        let mut invalid = replacement.clone();
        invalid.keys = vec!["Unknown".to_owned()];
        assert!(save(&path, &[invalid]).is_err());
        assert!(save(&path, &[replacement.clone(), replacement]).is_err());
        assert_eq!(fs::read(&path).unwrap(), previous);

        let temporary = path.with_extension("tmp");
        fs::write(&temporary, "occupied staging file").unwrap();
        assert!(save(&path, &[]).is_err());
        assert_eq!(fs::read(&path).unwrap(), previous);
        assert_eq!(
            fs::read_to_string(&temporary).unwrap(),
            "occupied staging file"
        );

        for malformed in [
            "",
            "vibeconsole-mappings-v2\n",
            "vibeconsole-mappings-v1\n09e8:1049\tnote\t10\t36\thold\tBogus\n",
            "vibeconsole-mappings-v1\n09e8:1049\tcc\t10\t36\thold\tShift\n",
            "vibeconsole-mappings-v1\nother\tnote\t10\t36\thold\tShift\n",
            "vibeconsole-mappings-v1\n09e8:1049\tnote\t1\t36\thold\tShift\n",
            "vibeconsole-mappings-v1\n09e8:1049\tnote\t10\t52\thold\tShift\n",
        ] {
            fs::write(&path, malformed).unwrap();
            assert!(load(&path).is_err());
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn old_flow_start_and_repair_mappings_load_as_the_merged_command() {
        let mut text = "vibeconsole-mappings-v9\nidle\t-\t-\t-\n".to_owned();
        for context in 1..8 {
            text.push_str(&format!("program-idle\t{context}\t-\t-\t-\n"));
        }
        text.push_str("start-flow\ton\nagents\t0\n");
        for (note, action) in [(36, "flow-start"), (37, "flow-repair")] {
            text.push_str(&format!(
                "0\tBank A Pad {}\tpad\t09e8:1049\tnote\t10\t{note}\t0\ttrigger\t-\t-\t-\t-\t{action}\t-\n",
                note - 35
            ));
        }
        let config = parse_config(&text).unwrap();
        assert!(
            config
                .mappings
                .iter()
                .all(|m| m.action == crate::actions::Action::Flow)
        );
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-flow-merge-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        save_config(&path, &config).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.starts_with("vibeconsole-mappings-v22\n"));
        assert!(saved.contains("\tflow\t") && !saved.contains("flow-start"));
        assert_eq!(load_config(&path).unwrap(), config);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn launcher_toggle_round_trips_and_old_launchers_keep_no_second_press() {
        use crate::actions::Action;
        let old = "vibeconsole-mappings-v10\nidle\t-\t-\t-\nprogram-idle\t1\t-\t-\t-\nprogram-idle\t2\t-\t-\t-\nprogram-idle\t3\t-\t-\t-\nprogram-idle\t4\t-\t-\t-\nprogram-idle\t5\t-\t-\t-\nprogram-idle\t6\t-\t-\t-\nprogram-idle\t7\t-\t-\t-\nstart-flow\ton\nagents\t0\n0\tBank A Pad 1\tpad\t09e8:1049\tnote\t10\t36\t0\ttrigger\t-\t-\t-\t-\tapp:/usr/share/applications/org.gnome.Ptyxis.desktop\t-\n";
        let mut config = parse_config(old).unwrap();
        assert_eq!(
            config.mappings[0].action,
            Action::Application("/usr/share/applications/org.gnome.Ptyxis.desktop".into())
        );
        config.mappings[0].action = Action::AppToggle {
            path: "/usr/share/applications/org.gnome.Ptyxis.desktop".into(),
            close: false,
        };
        let dir = std::env::temp_dir().join(format!("vibeconsole-toggle-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        save_config(&path, &config).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("vibeconsole-mappings-v22\n")
        );
        assert_eq!(load_config(&path).unwrap(), config);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Tickets 09–10: status feedback is a saved per-mapping option, off by default, and only
    /// for actions with observable state.
    #[test]
    fn status_feedback_is_saved_off_by_default_and_only_for_observable_actions() {
        use crate::actions::{Action, Target};
        let old = "vibeconsole-mappings-v10\nidle\t-\t-\t-\nprogram-idle\t1\t-\t-\t-\nprogram-idle\t2\t-\t-\t-\nprogram-idle\t3\t-\t-\t-\nprogram-idle\t4\t-\t-\t-\nprogram-idle\t5\t-\t-\t-\nprogram-idle\t6\t-\t-\t-\nprogram-idle\t7\t-\t-\t-\nstart-flow\ton\nagents\t0\n0\tBank A Pad 1\tpad\t09e8:1049\tnote\t10\t36\t0\ttrigger\t-\t-\t-\t-\tcommand:Bank-A-Pad-1:/tmp:claude\t-\n";
        let mut config = parse_config(old).unwrap();
        assert!(!config.mappings[0].status);
        config.mappings[0].status = true;
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-status-option-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        save_config(&path, &config).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("vibeconsole-mappings-v22\n") && text.contains("\tstatus\n"));
        assert_eq!(load_config(&path).unwrap(), config);
        let pad = Control::from_note(10, 37).unwrap();
        for (action, observable) in [
            (Action::Flow, true),
            (
                Action::Sequence(vec![crate::actions::Step::Wait(100)]),
                true,
            ),
            (Action::Application("/a/b.desktop".into()), true),
            (
                Action::Prompt {
                    file: "p".into(),
                    target: Target::Agent("claude".into()),
                    enter: true,
                },
                true,
            ),
            (
                Action::Prompt {
                    file: "p".into(),
                    target: Target::Focused { shift: false },
                    enter: true,
                },
                false,
            ),
            (Action::OutputMute, false),
        ] {
            let mut mapping = Mapping::new_action(pad, Input::Pad, action).unwrap();
            mapping.status = true;
            assert_eq!(
                mapping.validate().is_ok(),
                observable,
                "{}",
                mapping.action_label()
            );
        }
        let mut shortcut = Mapping::new(pad, "Shift", "hold").unwrap();
        shortcut.status = true;
        assert!(shortcut.validate().is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    /// Tickets 11–12: Settings data. Old configs load with no default directory, no agent
    /// overrides and empty variable lists; everything round-trips; bad input never saves.
    #[test]
    fn settings_directories_and_variables_migrate_round_trip_and_validate() {
        let old = "vibeconsole-mappings-v11\nidle\t-\t-\t-\nprogram-idle\t1\t-\t-\t-\nprogram-idle\t2\t-\t-\t-\nprogram-idle\t3\t-\t-\t-\nprogram-idle\t4\t-\t-\t-\nprogram-idle\t5\t-\t-\t-\nprogram-idle\t6\t-\t-\t-\nprogram-idle\t7\t-\t-\t-\nstart-flow\ton\nagents\t1\nagent\tclaude\tclaude\tclaude\n";
        let mut config = parse_config(old).unwrap();
        assert_eq!(config.agent_dir, None);
        assert_eq!(config.agents[0].dir, None);
        assert!(config.env.is_empty() && config.vars.is_empty());
        let dir = std::env::temp_dir().join(format!("vibeconsole-settings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("work")).unwrap();
        let path = dir.join("mappings.tsv");
        let work = dir.join("work").to_str().unwrap().to_owned();
        config.vars.push(("WORK".into(), work.clone()));
        config.env.push(("ANTHROPIC_MODEL".into(), "opus".into()));
        config.agent_dir = Some("${WORK}".into());
        config.agents[0].dir = Some(format!("{work}/../work"));
        config.mappings.push(
            Mapping::new_action(
                Control::from_note(10, 36).unwrap(),
                Input::Pad,
                crate::actions::Action::Command {
                    window: "w".into(),
                    dir: "${WORK}".into(),
                    line: "for f in *; do echo ${f} ${WORK}; done".into(), // the shell's
                },
            )
            .unwrap(),
        );
        save_config(&path, &config).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("vibeconsole-mappings-v22\n")
        );
        assert_eq!(load_config(&path).unwrap(), config);
        // ${NAME}: user variable, then startup variable; other text stays; no environment.
        assert!(expand("${HOME}", &config).is_err());
        assert_eq!(
            expand("${WORK}/a $X ${1:-x}", &config).unwrap(),
            format!("{work}/a $X ${{1:-x}}")
        );
        assert_eq!(expand("${ANTHROPIC_MODEL}", &config).unwrap(), "opus");
        assert!(
            expand("${NOPE_NOT_SET_ANYWHERE}", &config)
                .unwrap_err()
                .to_string()
                .contains("NOPE_NOT_SET_ANYWHERE")
        );
        assert_eq!(
            config.agent_directory(&config.agents[0]).unwrap(),
            Some(format!("{work}/../work"))
        );
        let mut plain = config.agents[0].clone();
        plain.dir = None;
        assert_eq!(config.agent_directory(&plain).unwrap(), Some(work.clone()));
        // Invalid input is rejected before save; the file keeps its bytes.
        let before = fs::read(&path).unwrap();
        for bad in [
            |c: &mut Config| c.vars.push(("9BAD".into(), "x".into())),
            |c: &mut Config| c.env.push(("OK".into(), "two\nlines".into())),
            |c: &mut Config| c.vars.push(("WORK".into(), "dup".into())),
            |c: &mut Config| c.agent_dir = Some("relative/dir".into()),
            |c: &mut Config| c.agent_dir = Some(String::new()),
            |c: &mut Config| c.agents[0].dir = Some("${MISSING_VARIABLE_X}".into()),
            |c: &mut Config| {
                c.vars.clear(); // the command still says ${WORK}
            },
        ] {
            let mut changed = config.clone();
            bad(&mut changed);
            assert!(save_config(&path, &changed).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
