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
}

impl fmt::Display for Behavior {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Hold => "hold",
            Self::Toggle => "toggle",
            Self::Pulse => "pulse",
            Self::Trigger => "trigger",
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

pub fn program_label(context: u8) -> String {
    if context < 8 {
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
            _ => return Err("behavior must be hold, toggle, or knob pulse".into()),
        };
        Ok(Self {
            control: pad,
            input: Input::Pad,
            context: 0,
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
        if self.context > 16
            || self.label.is_empty()
            || self.label.len() > 80
            || self.label.chars().any(char::is_control)
        {
            return Err("invalid mapping context or label".into());
        }
        use crate::Message;
        let valid = match self.input {
            Input::Pad | Input::Piano => {
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
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Agent {
    pub name: String,
    pub launch: String,
    pub window: String,
    /// Working directory overriding the Settings default; `${NAME}` allowed.
    pub dir: Option<String>,
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

fn parse_config(contents: &str) -> Result<Config> {
    let mut lines = contents.lines();
    // Files saved before the KeyAI → VibeConsole rename keep their old header.
    let version = lines.next().and_then(|header| {
        header
            .strip_prefix("vibeconsole-mappings-v")
            .or_else(|| header.strip_prefix("keyai-mappings-v"))
    });
    let version = match version {
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
        _ => {
            return Err(
                "invalid configuration header; expected vibeconsole-mappings-v1..v12".into(),
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
            let (name, launch, window, dir) = match fields.as_slice() {
                ["agent", name, launch, window] if version < 12 => (name, launch, window, None),
                ["agent", name, launch, window, dir] if version >= 12 => {
                    (name, launch, window, (*dir != "-").then(|| dir.to_string()))
                }
                _ => return Err("invalid agent record".into()),
            };
            let mut agent = Agent::new(name, launch, window)?;
            agent.dir = dir;
            agents.push(agent);
        }
        agents
    } else {
        Config::default().agents
    };
    validate_agents(&agents)?;
    let mut mappings = Vec::new();
    let mut seen = HashSet::new();
    let mut labels = HashSet::new();
    for (index, line) in lines.enumerate() {
        let result = (|| -> Result<Mapping> {
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
            if *device != DEVICE || (version < 3 && *message != "note") {
                return Err("unsupported device or MIDI message type".into());
            }
            let pad = if version >= 3 {
                let message = match *message {
                    "note" => crate::Message::Note,
                    "cc" => crate::Message::Cc,
                    "bend" => crate::Message::Bend,
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
                crate::actions::Action::parse(fields[10])?
            } else {
                crate::actions::Action::Shortcut
            };
            let mut mapping = if action == crate::actions::Action::Shortcut {
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
            if !seen.insert((context, pad)) {
                return Err("duplicate control assignment in context".into());
            }
            if !labels.insert((context, mapping.label.clone())) {
                return Err("duplicate physical label in context".into());
            }
            Ok(mapping)
        })();
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
    })
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

pub fn save_config(path: &Path, config: &Config) -> Result<()> {
    config.idle.validate()?;
    validate_agents(&config.agents)?;
    validate_variables(&config.env, "startup")?;
    validate_variables(&config.vars, "user")?;
    config.validate_references()?;
    let mut contents = format!(
        "vibeconsole-mappings-v12\nidle\t{}\t{}\t{}\n",
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
    contents.push_str(&format!("agents\t{}\n", config.agents.len()));
    for agent in &config.agents {
        contents.push_str(&format!(
            "agent\t{}\t{}\t{}\t{}\n",
            agent.name,
            agent.launch,
            agent.window,
            agent.dir.as_deref().unwrap_or("-")
        ));
    }
    for mapping in &config.mappings {
        mapping.validate()?;
        let feedback = mapping.feedback;
        let octave = match feedback.octave {
            None => "-".into(),
            Some(Octave::Offset(n)) => format!("offset:{n}"),
            Some(Octave::Absolute(n)) => format!("absolute:{n}"),
        };
        contents.push_str(&format!(
            "{}\t{}\t{}\t{DEVICE}\t{}\t{}\t{}\t{}\t{}\t{}\t{octave}\t{}\t{}\t{}\t{}\t{}\n",
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
        ));
    }
    parse_config(&contents)?;
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
            "/testdata/mpk-mini3-programs.hex"
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
        // Live cross-checks recorded in tickets/01-physical-control-names.md.
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
                .starts_with("vibeconsole-mappings-v12\n")
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
        assert!(saved.starts_with("vibeconsole-mappings-v12\n"));
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
                .starts_with("vibeconsole-mappings-v12\n")
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
        assert!(text.starts_with("vibeconsole-mappings-v12\n") && text.contains("\tstatus\n"));
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
                .starts_with("vibeconsole-mappings-v12\n")
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
