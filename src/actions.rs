use crate::Result;
use std::{
    collections::HashSet,
    fs,
    io::{self, Read},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    Agent(String),
    /// Paste with Ctrl+V, or Ctrl+Shift+V (terminals) when `shift`.
    Focused {
        shift: bool,
    },
}
impl Target {
    // `@` is never part of an agent's simple name, so focused targets cannot collide.
    fn encode(&self) -> &str {
        match self {
            Self::Agent(name) => name,
            Self::Focused { shift: false } => "@ctrl-v",
            Self::Focused { shift: true } => "@ctrl-shift-v",
        }
    }
    fn parse(value: &str) -> Self {
        match value {
            "@ctrl-v" => Self::Focused { shift: false },
            "@ctrl-shift-v" => Self::Focused { shift: true },
            name => Self::Agent(name.into()),
        }
    }
    pub fn description(&self) -> String {
        match self {
            Self::Agent(name) => name.clone(),
            Self::Focused { shift: false } => "focused window (Ctrl+V)".into(),
            Self::Focused { shift: true } => "focused window (Ctrl+Shift+V)".into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Step {
    /// Start the agent's tmux window if it is missing.
    Agent(String),
    Wait(u16),
    /// Press then release a shortcut on the session's keyboard.
    Keys(Vec<String>),
    /// Prompt, Command, Launch application, or a system action.
    Do(Action),
}
impl Step {
    fn encode(&self) -> String {
        match self {
            Self::Agent(name) => format!("agent:{name}"),
            Self::Wait(ms) => format!("wait:{ms}"),
            Self::Keys(keys) => format!("keys:{}", keys.join("+")),
            Self::Do(action) => action.encode(),
        }
    }
    fn parse(value: &str) -> Result<Self> {
        let step = if let Some(name) = value.strip_prefix("agent:") {
            Self::Agent(name.into())
        } else if let Some(ms) = value.strip_prefix("wait:") {
            Self::Wait(ms.parse()?)
        } else if let Some(keys) = value.strip_prefix("keys:") {
            Self::Keys(crate::mappings::parse_shortcut(keys)?)
        } else {
            Self::Do(Action::parse(value)?)
        };
        step.validate()?;
        Ok(step)
    }
    fn validate(&self) -> Result<()> {
        match self {
            Self::Agent(name) if !crate::mappings::simple_name(name) => {
                Err("step agent must be a simple name".into())
            }
            Self::Wait(ms) if !(100..=60000).contains(ms) => {
                Err("wait must be 100–60000 ms".into())
            }
            Self::Keys(keys) if crate::mappings::parse_shortcut(&keys.join("+"))? != *keys => {
                Err("invalid step shortcut".into())
            }
            Self::Do(
                Action::Shortcut
                | Action::Sequence(_)
                | Action::Choice(_)
                | Action::Value { .. }
                | Action::Flow
                | Action::AppToggle { .. },
            ) => Err("a step cannot be a mapping shortcut, sequence, knob, launcher toggle, or Flow action".into()),
            Self::Do(action) => action.validate(),
            _ => Ok(()),
        }
    }
    pub fn description(&self) -> String {
        match self {
            Self::Agent(name) => format!("Ensure agent {name} is running"),
            Self::Wait(ms) => format!("Wait {ms} ms"),
            Self::Keys(keys) => format!("Press {}", keys.join("+")),
            Self::Do(action) => action.description(),
        }
    }
}

// Unit separator between steps: every step encoding rejects control characters.
const STEP_SEPARATOR: char = '\x1f';

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Shortcut,
    Sequence(Vec<Step>),
    /// Knob zones, one prompt each, sent once the knob settles in a new zone.
    Choice(Vec<Action>),
    /// Knob position written to `path` as min..max with `decimals`, stored scaled by 10^decimals.
    Value {
        path: String,
        min: i64,
        max: i64,
        decimals: u8,
    },
    Prompt {
        file: String,
        target: Target,
        enter: bool,
    },
    /// `line` runs with `sh -c` in `dir`, in a new tmux window `window` of session `vibe`.
    Command {
        window: String,
        dir: String,
        line: String,
    },
    Application(String),
    /// Launcher pad: launch, or restore/raise its windows, or on a press while focused
    /// minimize (or `close`) them through the VibeConsole Shell extension.
    AppToggle {
        path: String,
        close: bool,
    },
    Volume {
        up: bool,
        step: u8,
    },
    OutputMute,
    MicrophoneMute,
    /// Start Wispr Flow when it is not running; repair (quit and reopen) it once when it is.
    Flow,
}
impl Action {
    pub fn flow(&self) -> bool {
        *self == Self::Flow
    }
    /// Agents this action sends to or starts (for clearing their finished state).
    pub fn agents(&self) -> Vec<&str> {
        match self {
            Self::Prompt {
                target: Target::Agent(name),
                ..
            } => vec![name],
            Self::Sequence(steps) => steps
                .iter()
                .flat_map(|step| match step {
                    Step::Agent(name) => vec![name.as_str()],
                    Step::Do(action) => action.agents(),
                    _ => Vec::new(),
                })
                .collect(),
            Self::Choice(choices) => choices.iter().flat_map(Self::agents).collect(),
            _ => Vec::new(),
        }
    }
    /// Actions with state the live pane can observe (tickets 09–10): a tmux window or agent,
    /// a queued sequence, an app's windows, or Flow's process. Shortcuts, focused-window
    /// pastes, audio steps and knob dials have none.
    pub fn observable(&self) -> bool {
        matches!(
            self,
            Self::Command { .. }
                | Self::Prompt {
                    target: Target::Agent(_),
                    ..
                }
                | Self::Sequence(_)
                | Self::Application(_)
                | Self::AppToggle { .. }
                | Self::Flow
        )
    }
    /// Saved prompt files this action sends, including choice zones and sequence steps.
    pub fn prompt_files(&self) -> Vec<&str> {
        match self {
            Self::Prompt { file, .. } => vec![file],
            Self::Choice(choices) => choices.iter().flat_map(Self::prompt_files).collect(),
            Self::Sequence(steps) => steps
                .iter()
                .flat_map(|step| match step {
                    Step::Do(action) => action.prompt_files(),
                    _ => Vec::new(),
                })
                .collect(),
            _ => Vec::new(),
        }
    }
    /// (command line, directory) of each tmux command this action runs, sequence steps included.
    pub fn commands(&self) -> Vec<(&str, &str)> {
        match self {
            Self::Command { line, dir, .. } => vec![(line, dir)],
            Self::Sequence(steps) => steps
                .iter()
                .flat_map(|step| match step {
                    Step::Do(action) => action.commands(),
                    _ => Vec::new(),
                })
                .collect(),
            _ => Vec::new(),
        }
    }
    /// Knob actions driven by absolute position rather than pulses.
    pub fn dial(&self) -> bool {
        matches!(self, Self::Choice(_) | Self::Value { .. })
    }
    pub fn audio(&self) -> bool {
        matches!(
            self,
            Self::Volume { .. } | Self::OutputMute | Self::MicrophoneMute
        )
    }
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Sequence(steps) if steps.is_empty() || steps.len() > 32 => {
                Err("a sequence needs 1–32 steps".into())
            }
            Self::Sequence(steps) => steps.iter().try_for_each(Step::validate),
            Self::Choice(choices)
                if !(2..=16).contains(&choices.len())
                    || choices.iter().any(|c| !matches!(c, Self::Prompt { .. })) =>
            {
                Err("a choice knob needs 2–16 prompts".into())
            }
            Self::Choice(choices) => choices.iter().try_for_each(Self::validate),
            Self::Value {
                path,
                min,
                max,
                decimals,
            } if !Path::new(path).is_absolute()
                || path.ends_with('/')
                || path.len() > 4096
                || path.chars().any(char::is_control)
                || *decimals > 6
                || min >= max
                || min.abs() > 1_000_000_000_000
                || max.abs() > 1_000_000_000_000 =>
            {
                Err("value knob needs an absolute file path, min < max, and 0–6 decimals".into())
            }
            Self::Prompt { file, target, .. }
                if !crate::mappings::simple_name(file)
                    || matches!(target, Target::Agent(a) if !crate::mappings::simple_name(a)) =>
            {
                Err("prompt file and agent must be simple names".into())
            }
            // A directory may start with a `${NAME}` user variable; saving checks it expands
            // to an absolute path.
            Self::Command { window, dir, line }
                if !crate::mappings::simple_name(window)
                    || !(Path::new(dir).is_absolute() || dir.starts_with("${"))
                    || dir.contains(':')
                    || dir.len() > 4096
                    || dir.chars().any(char::is_control)
                    || line.trim().is_empty()
                    || line.len() > 4096
                    || line.chars().any(char::is_control) =>
            {
                Err("command must be one nonempty line without control characters, run in an absolute directory without ':'".into())
            }
            Self::Application(path) | Self::AppToggle { path, .. }
                if !Path::new(path).is_absolute()
                    || !path.ends_with(".desktop")
                    || path.chars().any(char::is_control)
                    || path.len() > 4096 =>
            {
                Err("invalid installed desktop entry path".into())
            }
            Self::Volume { step, .. } if !(1..=100).contains(step) => {
                Err("volume step must be 1..100 percent".into())
            }
            _ => Ok(()),
        }
    }
    pub fn encode(&self) -> String {
        match self {
            Self::Shortcut => "shortcut".into(),
            Self::Choice(choices) => format!(
                "choice:{}",
                choices
                    .iter()
                    .map(Action::encode)
                    .collect::<Vec<_>>()
                    .join(&STEP_SEPARATOR.to_string())
            ),
            Self::Value {
                path,
                min,
                max,
                decimals,
            } => format!("value:{decimals}:{min}:{max}:{path}"),
            Self::Sequence(steps) => format!(
                "sequence:{}",
                steps
                    .iter()
                    .map(Step::encode)
                    .collect::<Vec<_>>()
                    .join(&STEP_SEPARATOR.to_string())
            ),
            Self::Prompt {
                file,
                target,
                enter,
            } => format!(
                "prompt:{file}:{}:{}",
                target.encode(),
                if *enter { "enter" } else { "paste" }
            ),
            Self::Command { window, dir, line } => format!("command:{window}:{dir}:{line}"),
            Self::Application(path) => format!("app:{path}"),
            Self::AppToggle { path, close } => format!(
                "app-toggle:{}:{path}",
                if *close { "close" } else { "minimize" }
            ),
            Self::Volume { up, step } => {
                format!("volume-{}:{step}", if *up { "up" } else { "down" })
            }
            Self::OutputMute => "output-mute".into(),
            Self::MicrophoneMute => "microphone-mute".into(),
            Self::Flow => "flow".into(),
        }
    }
    pub fn parse(value: &str) -> Result<Self> {
        let action = match value {
            "shortcut" => Self::Shortcut,
            _ if value.starts_with("prompt:") => {
                let fields: Vec<_> = value.split(':').collect();
                let ["prompt", file, agent, mode] = fields.as_slice() else {
                    return Err("invalid prompt action".into());
                };
                let enter = match *mode {
                    "enter" => true,
                    "paste" => false,
                    _ => return Err("invalid prompt submit mode".into()),
                };
                Self::Prompt {
                    file: (*file).into(),
                    target: Target::parse(agent),
                    enter,
                }
            }
            _ if value.starts_with("choice:") => Self::Choice(
                value["choice:".len()..]
                    .split(STEP_SEPARATOR)
                    .map(Action::parse)
                    .collect::<Result<_>>()?,
            ),
            _ if value.starts_with("value:") => {
                let mut fields = value.splitn(5, ':').skip(1);
                let (Some(decimals), Some(min), Some(max), Some(path)) =
                    (fields.next(), fields.next(), fields.next(), fields.next())
                else {
                    return Err("invalid value knob action".into());
                };
                Self::Value {
                    path: path.into(),
                    min: min.parse()?,
                    max: max.parse()?,
                    decimals: decimals.parse()?,
                }
            }
            _ if value.starts_with("sequence:") => Self::Sequence(
                value["sequence:".len()..]
                    .split(STEP_SEPARATOR)
                    .map(Step::parse)
                    .collect::<Result<_>>()?,
            ),
            _ if value.starts_with("command:") => {
                let mut fields = value.splitn(4, ':').skip(1);
                let (Some(window), Some(dir), Some(line)) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    return Err("invalid command action".into());
                };
                Self::Command {
                    window: window.into(),
                    dir: dir.into(),
                    line: line.into(),
                }
            }
            "output-mute" => Self::OutputMute,
            "microphone-mute" => Self::MicrophoneMute,
            // Mappings saved before the merge used separate start and repair actions.
            "flow" | "flow-start" | "flow-repair" => Self::Flow,
            _ if value.starts_with("app:") => Self::Application(value[4..].into()),
            _ if value.starts_with("app-toggle:") => {
                let (policy, path) = value["app-toggle:".len()..]
                    .split_once(':')
                    .ok_or("invalid app toggle")?;
                Self::AppToggle {
                    path: path.into(),
                    close: match policy {
                        "close" => true,
                        "minimize" => false,
                        _ => return Err("app toggle second press must be minimize or close".into()),
                    },
                }
            }
            _ if value.starts_with("volume-up:") => Self::Volume {
                up: true,
                step: value[10..].parse()?,
            },
            _ if value.starts_with("volume-down:") => Self::Volume {
                up: false,
                step: value[12..].parse()?,
            },
            _ => return Err("unsupported action".into()),
        };
        action.validate()?;
        Ok(action)
    }
    pub fn description(&self) -> String {
        match self {
            Self::Prompt {
                file,
                target,
                enter,
            } => format!(
                "Prompt {file} → {}{}{}",
                target.description(),
                if *enter { " + Enter" } else { " (paste only)" },
                if matches!(target, Target::Focused { .. }) {
                    "; lands in whichever window has focus"
                } else {
                    ""
                }
            ),
            Self::Command { window, dir, line } => {
                format!("Run `{line}` in {dir} (tmux window {window})")
            }
            Self::Choice(choices) => format!(
                "Choice knob: {}",
                choices
                    .iter()
                    .map(Action::description)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
            Self::Value {
                path,
                min,
                max,
                decimals,
            } => format!(
                "Value knob {}–{} → {path}",
                fixed(*min, *decimals),
                fixed(*max, *decimals)
            ),
            Self::Sequence(steps) => format!(
                "Sequence: {}",
                steps
                    .iter()
                    .map(Step::description)
                    .collect::<Vec<_>>()
                    .join(" → ")
            ),
            Self::Application(path) => format!(
                "Launch {}",
                Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
            Self::AppToggle { path, close } => format!(
                "Launch {}; pressed while focused it {}",
                Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                if *close { "closes" } else { "minimizes" }
            ),
            Self::Volume { up, step } => format!(
                "Default output volume {} {step}% (100% limit)",
                if *up { "up" } else { "down" }
            ),
            Self::OutputMute => "Toggle default output mute".into(),
            Self::MicrophoneMute => "Toggle default microphone mute".into(),
            Self::Flow => "Start or repair Wispr Flow".into(),
            Self::Shortcut => "Keyboard shortcut".into(),
        }
    }
}

fn roots() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")));
    let dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(std::env::split_paths(&dirs))
        .filter(|p| p.is_absolute())
        .map(|p| p.join("applications"))
        .collect()
}
fn desktop(text: &str, desktops: &[&str]) -> Option<String> {
    let values = entry_fields(text);
    let get = |key| values.get(key).copied().unwrap_or("");
    let matches = |value: &str| {
        value
            .split(';')
            .any(|s| !s.is_empty() && desktops.contains(&s))
    };
    if get("Type") != "Application"
        || get("Hidden") == "true"
        || get("NoDisplay") == "true"
        || (!get("OnlyShowIn").is_empty() && !matches(get("OnlyShowIn")))
        || matches(get("NotShowIn"))
        || (get("Exec").is_empty() && get("DBusActivatable") != "true")
    {
        return None;
    }
    let try_exec = get("TryExec");
    if !try_exec.is_empty() {
        use std::os::unix::fs::PermissionsExt;
        let executable = |p: PathBuf| {
            fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        };
        if !(if Path::new(try_exec).is_absolute() {
            executable(try_exec.into())
        } else {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .any(|dir| executable(dir.join(try_exec)))
        }) {
            return None;
        }
    }
    let name = get("Name")
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect::<String>();
    (!name.is_empty()).then_some(name)
}
fn catalog_at(roots: &[PathBuf], desktops: &[&str]) -> Result<Vec<(String, String)>> {
    fn files(dir: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
        for item in fs::read_dir(dir)? {
            let item = item?;
            if item.file_type()?.is_dir() {
                files(&item.path(), found)?;
            } else if item.path().extension().is_some_and(|s| s == "desktop") {
                found.push(item.path());
            }
        }
        Ok(())
    }
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        let mut paths = Vec::new();
        files(root, &mut paths)?;
        paths.sort();
        for path in paths {
            let id = path.strip_prefix(root)?.to_string_lossy().replace('/', "-");
            if !seen.insert(id) {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            if let Some(name) = desktop(&text, desktops) {
                let path = path
                    .to_str()
                    .ok_or("non-UTF8 desktop entry path")?
                    .to_owned();
                Action::Application(path.clone()).validate()?;
                result.push((name, path));
            }
        }
    }
    result.sort_by(|a, b| {
        a.0.to_lowercase()
            .cmp(&b.0.to_lowercase())
            .then(a.1.cmp(&b.1))
    });
    Ok(result)
}
pub fn catalog() -> Result<Vec<(String, String)>> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    catalog_at(&roots(), &desktop.split(':').collect::<Vec<_>>())
}
fn flow_launcher() -> Result<Option<String>> {
    Ok(catalog()?
        .into_iter()
        .find(|(_, path)| {
            Path::new(path)
                .file_name()
                .is_some_and(|n| n == "wispr-flow.desktop")
        })
        .map(|(_, path)| path))
}
pub fn flow_installed() -> Result<bool> {
    Ok(flow_launcher()?.is_some())
}

// Only child dispatch requests are cancellable. Never kill the resulting application.
fn command(program: &str, args: &[String], cancelled: &impl Fn() -> bool) -> Result<String> {
    if cancelled() {
        return Err("action cancelled before dispatch".into());
    }
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    for fd in [out.as_raw_fd(), err.as_raw_fd()] {
        // SAFETY: owned pipe descriptor, preserve flags and enable nonblocking reads.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::last_os_error().into());
        }
    }
    let started = Instant::now();
    let mut data = Vec::new();
    let mut errors = Vec::new();
    let drain = |reader: &mut dyn Read, data: &mut Vec<u8>| -> io::Result<()> {
        let mut bytes = [0; 4096];
        for _ in 0..16 {
            match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => {
                    if data.len() + n > 65536 {
                        return Err(io::Error::other("action output limit reached"));
                    }
                    data.extend_from_slice(&bytes[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    };
    let result = (|| -> Result<String> {
        loop {
            drain(&mut out, &mut data)?;
            drain(&mut err, &mut errors)?;
            if cancelled() {
                return Err(
                    "action cancelled; completed application/audio effects are preserved".into(),
                );
            }
            if started.elapsed() > Duration::from_secs(5) {
                return Err(format!("{program} dispatch timed out").into());
            }
            if let Some(status) = child.try_wait()? {
                drain(&mut out, &mut data)?;
                drain(&mut err, &mut errors)?;
                if !status.success() {
                    return Err(format!(
                        "{program}: {status}: {}",
                        String::from_utf8_lossy(&errors).trim()
                    )
                    .into());
                }
                return Ok(String::from_utf8_lossy(&data).trim().to_string());
            }
            thread::sleep(Duration::from_millis(20));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}
fn audio(action: &Action, mut run: impl FnMut(&[String]) -> Result<String>) -> Result<String> {
    let default = if *action == Action::MicrophoneMute {
        "@DEFAULT_AUDIO_SOURCE@"
    } else {
        "@DEFAULT_AUDIO_SINK@"
    };
    // Resolve anew for each activation, then read back the exact object we changed.
    let info = run(&strings(&["inspect", default]))?;
    let id = info
        .split_whitespace()
        .skip_while(|s| *s != "id")
        .nth(1)
        .ok_or("wpctl did not resolve a default audio target")?
        .trim_end_matches(',')
        .parse::<u32>()?
        .to_string();
    let request = match action {
        Action::Volume { up, step } => vec![
            "set-volume".into(),
            id.clone(),
            format!("{step}%{}", if *up { "+" } else { "-" }),
            "--limit".into(),
            "1.0".into(),
        ],
        Action::OutputMute | Action::MicrophoneMute => {
            vec!["set-mute".into(), id.clone(), "toggle".into()]
        }
        _ => return Err("not an audio action".into()),
    };
    run(&request)?;
    let state = run(&["get-volume".into(), id])?;
    let volume = state
        .strip_prefix("Volume: ")
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<f64>().ok());
    if !volume.is_some_and(|v| v.is_finite() && v >= 0.0) {
        return Err("audio request sent but read-back was invalid".into());
    }
    Ok(format!("{}: {state}", action.description()))
}
fn launch_request(
    path: &str,
    apps: &[(String, String)],
    session: bool,
    mut run: impl FnMut(&[String]) -> Result<String>,
) -> Result<String> {
    if !apps.iter().any(|(_, p)| p == path) {
        return Err(if Path::new(path).exists() {
            format!("launcher rejected: {path} is hidden, NoDisplay, not for this desktop, or its TryExec program is missing")
        } else {
            format!("launcher missing: {path} no longer exists; reselect the application in configuration")
        }
        .into());
    }
    if !session {
        return Err("launcher failed to start: desktop session unavailable".into());
    }
    // The desktop entry path is an identifier for gio, never a shell command.
    run(&strings(&["launch", path])).map_err(|e| format!("launcher failed to start: {e}"))?;
    Ok("Native launch request accepted; window/readiness not verified".into())
}
fn launch(path: &str, cancelled: &impl Fn() -> bool) -> Result<String> {
    let session = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        && (std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some());
    launch_request(path, &catalog()?, session, |args| {
        command("gio", args, cancelled)
    })
}

/// Desktop ID of an installed entry path, as the catalog and GNOME derive it: the path below
/// its `applications/` directory with `/` turned into `-` (e.g. `kde4-foo.desktop`).
pub fn desktop_id(path: &str) -> Option<String> {
    let (_, below) = path.rsplit_once("/applications/")?;
    Some(below.replace('/', "-"))
}

/// `gdbus call` argv for the window extension's `method` (2 s timeout: never hang a caller).
fn windows_call(method: &str, args: &[&str]) -> Vec<String> {
    let mut argv = strings(&[
        "call",
        "--session",
        "--timeout",
        "2",
        "--dest",
        "org.vibeconsole.Windows",
        "--object-path",
        "/org/vibeconsole/Windows",
        "--method",
    ]);
    argv.push(format!("org.vibeconsole.Windows.{method}"));
    argv.extend(args.iter().map(|a| a.to_string()));
    argv
}

/// A live pane in `list-panes -F '#{pane_dead}'` output (remain-on-exit keeps dead ones).
pub fn pane_alive(ok: bool, output: &str) -> bool {
    ok && output.lines().any(|line| line.trim() == "0")
}

/// Whether `vibe` window `name` has a live pane; None when tmux cannot be asked.
pub fn window_alive(name: &str) -> Option<bool> {
    let output = tmux(&[
        "list-panes",
        "-t",
        &format!("=vibe:={name}"),
        "-F",
        "#{pane_dead}",
    ])
    .ok()?;
    Some(pane_alive(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
    ))
}

/// Whether the action's observable process runs now (tickets 09–10); None when unknowable.
/// Sequences are answered by the dispatcher instead.
/// `agents` are (name, tmux window) pairs.
pub fn running(action: &Action, agents: &[(String, String)]) -> Option<bool> {
    let window = window_alive;
    match action {
        Action::Command { window: name, .. } => window(name),
        Action::Prompt {
            target: Target::Agent(agent),
            ..
        } => window(&agents.iter().find(|(name, _)| name == agent)?.1),
        Action::Application(path) | Action::AppToggle { path, .. } => {
            let id = desktop_id(path)?;
            let output = Command::new("gdbus")
                .args(windows_call("State", &[&id]))
                .output()
                .ok()
                .filter(|o| o.status.success())?;
            Some(!String::from_utf8_lossy(&output.stdout).contains("'none'"))
        }
        Action::Flow => flow_processes().ok().map(|(main, _)| !main.is_empty()),
        _ => None,
    }
}

/// One launcher-pad press, decided by the Shell extension's view of the app's windows.
fn app_toggle(
    path: &str,
    close: bool,
    mut gdbus: impl FnMut(&[String]) -> Result<String>,
    mut launch: impl FnMut() -> Result<String>,
) -> Result<String> {
    let id = desktop_id(path).ok_or("launcher path is not under an applications directory")?;
    let id = id.as_str();
    let mut call = |method: &str, args: &[&str]| gdbus(&windows_call(method, args));
    let state = match call("State", &[id]) {
        Ok(state) => state,
        Err(error) => {
            launch()?;
            let missing = ["ServiceUnknown", "not provided", "NameHasNoOwner"]
                .iter()
                .any(|s| error.to_string().contains(s));
            return Ok(if missing {
                format!(
                    "Launched {id}; minimize/close unavailable: VibeConsole window extension not enabled. See README, Window control."
                )
            } else {
                format!(
                    "Launched {id}; minimize/close unavailable: window extension cannot identify {id} ({error})"
                )
            });
        }
    };
    let mut apply = |action| -> Result<String> { call("Apply", &[id, action]) };
    match state
        .trim()
        .trim_start_matches("('")
        .trim_end_matches("',)")
    {
        "none" => {
            launch()?;
            Ok(format!("Launched {id}"))
        }
        "minimized" | "background" => {
            apply("restore")?;
            Ok(format!("Restored {id}"))
        }
        "focused" if close => {
            apply("close")?;
            Ok(format!("Asked {id} to close"))
        }
        "focused" => {
            apply("minimize")?;
            Ok(format!("Minimized {id}"))
        }
        other => {
            Err(format!("minimize/close unavailable: unexpected window state {other:?}").into())
        }
    }
}

pub fn prompts_dir() -> Result<PathBuf> {
    prompts_dir_for(&crate::mappings::config_path()?)
}
/// Saved prompts live in `prompts/` beside the configuration file.
pub fn prompts_dir_for(config: &Path) -> Result<PathBuf> {
    Ok(config
        .parent()
        .ok_or("configuration directory missing")?
        .join("prompts"))
}

/// `program`, in its own transient scope when `in_unit` (VibeConsole runs as a systemd
/// service): stopping the service kills its whole group, which must not take the tmux
/// server, agents or opened terminals with it.
fn scoped(program: &str, in_unit: bool) -> Command {
    if !in_unit {
        return Command::new(program);
    }
    let mut command = Command::new("systemd-run");
    command.args(["--user", "--scope", "--quiet", "--collect", "--", program]);
    command
}
/// systemd sets INVOCATION_ID for every unit it starts.
fn in_unit() -> bool {
    std::env::var_os("INVOCATION_ID").is_some()
}

fn tmux(args: &[&str]) -> Result<std::process::Output> {
    // Only new-session can start the server; every other call talks to it.
    let server = args.first() == Some(&"new-session");
    Ok(scoped("tmux", server && in_unit()).args(args).output()?)
}

fn tmux_ok(args: &[&str]) -> Result<()> {
    let output = tmux(args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "tmux {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into())
    }
}

fn prompt_tmux_args<'a>(path: &'a str, target: &'a str, enter: bool) -> Vec<Vec<&'a str>> {
    let mut commands = vec![
        vec!["load-buffer", "-b", "vibeconsole-prompt", path],
        vec![
            "paste-buffer",
            "-d",
            "-p",
            "-b",
            "vibeconsole-prompt",
            "-t",
            target,
        ],
    ];
    if enter {
        commands.push(vec!["send-keys", "-t", target, "Enter"]);
    }
    commands
}

/// `value` scaled by 10^decimals as text, e.g. (42, 2) → "0.42", (-5, 1) → "-0.5".
pub fn fixed(value: i64, decimals: u8) -> String {
    if decimals == 0 {
        return value.to_string();
    }
    let scale = 10u64.pow(u32::from(decimals));
    let sign = if value < 0 { "-" } else { "" };
    let abs = value.unsigned_abs();
    format!(
        "{sign}{}.{:0width$}",
        abs / scale,
        abs % scale,
        width = usize::from(decimals)
    )
}

pub fn copy_text(text: &str) -> Result<()> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("wl-copy unavailable ({e}); install wl-clipboard"))?;
    std::io::Write::write_all(
        &mut child.stdin.take().ok_or("wl-copy stdin")?,
        text.as_bytes(),
    )?;
    if !child.wait()?.success() {
        return Err("wl-copy failed".into());
    }
    Ok(())
}

/// Write `contents` so readers see the old or the new file, never a partial one.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".vibeconsole-tmp");
    fs::write(&temporary, contents)?;
    fs::rename(&temporary, path).map_err(|e| {
        let _ = fs::remove_file(&temporary);
        e.into()
    })
}

fn command_tmux_args<'a>(
    session: bool,
    window: &'a str,
    dir: &'a str,
    line: &'a str,
    env: &'a [String],
) -> Vec<&'a str> {
    let mut args = if session {
        vec!["new-window", "-t", "=vibe:"]
    } else {
        vec!["new-session", "-d", "-s", "vibe"]
    };
    // Not detached: the new window must be current so the chained set-option targets it
    // before tmux can process the command's exit (verified with tmux 3.7c).
    args.extend(["-n", window, "-c", dir]);
    for variable in env {
        args.extend(["-e", variable.as_str()]);
    }
    args.extend([
        "sh",
        "-c",
        line,
        ";",
        "set-option",
        "-w",
        "remain-on-exit",
        "on",
    ]);
    args
}

/// Startup and user variables as tmux `-e` values: the shell of every agent and command sees
/// them, and expands `${NAME}` itself with its own quoting rules.
fn startup_env(config: &crate::mappings::Config) -> Vec<String> {
    config
        .env
        .iter()
        .chain(&config.vars)
        .map(|(n, v)| format!("{n}={v}"))
        .collect()
}

fn run_command(window: &str, dir: &str, line: &str) -> Result<String> {
    let config = crate::mappings::load_config(&crate::mappings::config_path()?)?;
    let dir = crate::mappings::absolute_directory(dir, &config)?;
    show_command(
        window,
        &dir,
        line,
        &startup_env(&config),
        &mut |args| {
            let output = tmux(args)?;
            // Success: stdout; failure: stderr, for the error message.
            let text = if output.status.success() {
                &output.stdout
            } else {
                &output.stderr
            };
            Ok((
                output.status.success(),
                String::from_utf8_lossy(text).trim().to_string(),
            ))
        },
        &mut open_terminal,
    )
}

/// Send the command to tmux, then show the session: open a terminal attached to it unless a
/// client already is. Only a successful open is reported as shown.
fn show_command(
    window: &str,
    dir: &str,
    line: &str,
    env: &[String],
    tmux: &mut dyn FnMut(&[&str]) -> Result<(bool, String)>,
    open: &mut dyn FnMut() -> Result<()>,
) -> Result<String> {
    if !Path::new(dir).is_dir() {
        return Err(format!("working directory {dir} is missing").into());
    }
    let (session, _) = tmux(&["has-session", "-t", "=vibe"])?;
    let args = command_tmux_args(session, window, dir, line, env);
    let (ok, output) = tmux(&args)?;
    if !ok {
        return Err(format!("tmux {}: {output}", args.join(" ")).into());
    }
    let (listed, clients) = tmux(&["list-clients", "-t", "=vibe", "-F", "#{client_name}"])?;
    if listed && !clients.trim().is_empty() {
        return Ok(format!(
            "Command started in tmux window {window}; session vibe already attached"
        ));
    }
    Ok(match open() {
        Ok(()) => {
            format!("Command started in tmux window {window}; terminal opened on session vibe")
        }
        Err(error) => format!(
            "Command sent to tmux window {window}, sent to tmux, not shown: {error}. Run: tmux attach -t vibe"
        ),
    })
}

/// Key/value pairs of a desktop entry's [Desktop Entry] group.
fn entry_fields(text: &str) -> std::collections::HashMap<&str, &str> {
    let mut entry = false;
    let mut values = std::collections::HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            entry = line == "[Desktop Entry]";
        } else if entry && !line.starts_with('#') {
            if let Some((k, v)) = line.split_once('=') {
                values.insert(k.trim(), v.trim());
            }
        }
    }
    values
}

/// argv attaching to the `vibe` session in the first terminal entry of `texts`: the entry's Exec
/// program run directly (no shell), then its X-ExecArg (default `-e`), then tmux.
fn terminal_argv(texts: &[&str]) -> Option<Vec<String>> {
    texts.iter().find_map(|text| {
        let fields = entry_fields(text);
        if !fields
            .get("Categories")
            .is_some_and(|c| c.split(';').any(|c| c == "TerminalEmulator"))
        {
            return None;
        }
        let program = fields.get("Exec")?.split_whitespace().next()?;
        // Wrapped or quoted launchers (flatpak, env, "…") cannot reach the host tmux reliably.
        if program.contains(['=', '"', '\''])
            || program.starts_with('%')
            || ["env", "flatpak"].contains(&program.rsplit('/').next()?)
        {
            return None;
        }
        let mut argv = vec![program.to_string()];
        match fields.get("X-ExecArg") {
            Some(arg) if arg.is_empty() => {}
            Some(arg) => argv.push(arg.to_string()),
            None => argv.push("-e".into()),
        }
        argv.extend(strings(&["tmux", "attach-session", "-t", "=vibe"]));
        Some(argv)
    })
}

/// Open the installed terminal (Ptyxis first) attached to the `vibe` session, detached from us.
fn open_terminal() -> Result<()> {
    let mut paths = catalog()?.into_iter().map(|(_, p)| p).collect::<Vec<_>>();
    paths.sort_by_key(|p| !p.ends_with("/org.gnome.Ptyxis.desktop"));
    let texts = paths
        .iter()
        .filter_map(|p| fs::read_to_string(p).ok())
        .collect::<Vec<_>>();
    let argv = terminal_argv(&texts.iter().map(String::as_str).collect::<Vec<_>>())
        .ok_or("no terminal emulator desktop entry installed")?;
    use std::os::unix::process::CommandExt;
    let mut child = scoped(&argv[0], in_unit())
        .args(&argv[1..])
        .process_group(0) // Ctrl+C in VibeConsole's own terminal must not reach it
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    // A terminal that fails at once is reported; one that stays open is reaped when it exits.
    thread::sleep(Duration::from_millis(300));
    if let Some(status) = child.try_wait()? {
        if !status.success() {
            return Err(format!("{} exited: {status}", argv[0]).into());
        }
    } else {
        thread::spawn(move || child.wait());
    }
    Ok(())
}

fn read_prompt(path: &Path) -> Result<Vec<u8>> {
    let contents = fs::read(path)?;
    if contents.is_empty() || contents.iter().all(u8::is_ascii_whitespace) {
        return Err(format!("prompt {} is empty", path.display()).into());
    }
    std::str::from_utf8(&contents)?;
    Ok(contents)
}

// ponytail: fixed waits; tmux cannot tell when an agent's TUI is ready. Tune here, or use a
// sequence wait step (ticket 04) when a slower agent needs longer.
const AGENT_START: Duration = Duration::from_secs(4);
const PASTE_SETTLE: Duration = Duration::from_millis(200);

fn wait(duration: Duration, cancelled: &dyn Fn() -> bool) -> Result<()> {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        if cancelled() {
            return Err("prompt cancelled".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// Start the agent's tmux window when missing; returns its window name.
fn ensure_agent(agent_name: &str, cancelled: &dyn Fn() -> bool) -> Result<String> {
    let config = crate::mappings::load_config(&crate::mappings::config_path()?)?;
    let agent = config
        .agents
        .iter()
        .find(|a| a.name == agent_name)
        .ok_or("target agent was removed; edit this mapping")?;
    // Lets `agent-status -` hooks know which agent they run in.
    let tag = format!("{}={}", crate::status::AGENT_ENV, agent.name);
    let env = startup_env(&config);
    // The directory matters only when a window starts; a running agent ignores it.
    let start = |session: bool| -> Result<()> {
        let dir = config.agent_directory(agent)?;
        if let Some(dir) = &dir {
            if !Path::new(dir).is_dir() {
                return Err(format!(
                    "agent directory {dir} is missing; fix it in Settings or Agents"
                )
                .into());
            }
        }
        let args = agent_tmux_args(
            session,
            &agent.window,
            &tag,
            &agent.launch,
            dir.as_deref(),
            &env,
        );
        tmux_ok(&args.iter().map(String::as_str).collect::<Vec<_>>())
    };
    let started = if !tmux(&["has-session", "-t", "=vibe"])?.status.success() {
        start(false)?;
        true
    } else {
        let windows = tmux(&["list-windows", "-t", "=vibe", "-F", "#{window_name}"])?;
        if !windows.status.success() {
            return Err("cannot list vibe tmux windows".into());
        }
        let missing = !String::from_utf8(windows.stdout)?
            .lines()
            .any(|name| name == agent.window);
        if missing {
            start(true)?;
        }
        missing
    };
    if started {
        wait(AGENT_START, cancelled)?;
    }
    Ok(agent.window.clone())
}

/// tmux argv starting an agent: a new `vibe` session, or a window in it when `session` exists.
fn agent_tmux_args(
    session: bool,
    window: &str,
    tag: &str,
    launch: &str,
    dir: Option<&str>,
    env: &[String],
) -> Vec<String> {
    let mut args = if session {
        strings(&["new-window", "-t", "=vibe:"])
    } else {
        strings(&["new-session", "-d", "-s", "vibe"])
    };
    args.extend(strings(&["-n", window]));
    if let Some(dir) = dir {
        args.extend(strings(&["-c", dir]));
    }
    args.extend(strings(&["-e", tag]));
    for variable in env {
        args.extend(strings(&["-e", variable]));
    }
    args.push(launch.into());
    args
}

fn send_prompt(
    file: &str,
    agent_name: &str,
    enter: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<String> {
    if !crate::mappings::simple_name(file) || !crate::mappings::simple_name(agent_name) {
        return Err("invalid prompt file or agent".into());
    }
    let path = prompts_dir()?.join(file);
    read_prompt(&path)?;
    if cancelled() {
        return Err("prompt cancelled".into());
    }
    let target = format!("=vibe:={}", ensure_agent(agent_name, cancelled)?);
    if cancelled() {
        return Err("prompt cancelled".into());
    }
    let path = path.to_str().ok_or("prompt path is not UTF-8")?;
    let commands = prompt_tmux_args(path, &target, enter);
    tmux_ok(&commands[0])?;
    if cancelled() {
        return Err("prompt cancelled".into());
    }
    tmux_ok(&commands[1])?;
    if enter {
        // Enter after the paste has landed is a separate key, never part of the paste.
        wait(PASTE_SETTLE, cancelled)?;
        tmux_ok(&commands[2])?;
    }
    Ok(format!(
        "Prompt {file} sent to {agent_name}; tmux attach -t vibe"
    ))
}

// The worker then asks the session to press the paste shortcut (and Enter) on its keyboard.
// ponytail: the previous clipboard is not restored; restoring races the receiving
// application's clipboard read. Revisit if a paste-completion signal becomes available.
fn copy_prompt(path: &Path, mut wl_copy: Command) -> Result<()> {
    read_prompt(path)?;
    let status = wl_copy
        .arg("--trim-newline")
        .stdin(fs::File::open(path)?)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("wl-copy unavailable ({e}); install wl-clipboard"))?;
    if !status.success() {
        return Err(format!("wl-copy failed: {status}").into());
    }
    Ok(())
}

struct Job {
    action: Action,
    node: Option<PathBuf>,
    epoch: u64,
}
/// Keys the worker asks the session to tap on its virtual keyboard, with the reply channel.
pub type TapRequest = (
    Vec<evdev::KeyCode>,
    mpsc::SyncSender<std::result::Result<(), String>>,
);
/// `Dispatcher::submit` error when earlier requests are still waiting.
pub const QUEUE_FULL: &str = "action queue full; request skipped";

pub struct Dispatcher {
    requests: mpsc::SyncSender<Job>,
    notices: mpsc::Receiver<(u64, Action, bool, String)>,
    taps: Option<mpsc::Receiver<TapRequest>>,
    /// Sequences queued or running; a re-press of the same sequence is ignored.
    sequences: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    epoch: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Dispatcher {
    pub(crate) fn with(
        mut run: impl FnMut(&Action, Option<&Path>, &dyn Fn() -> bool) -> Result<String>
        + Send
        + 'static,
    ) -> Self {
        let (requests, receiver) = mpsc::sync_channel::<Job>(8);
        let (tx, notices) = mpsc::sync_channel(16);
        let epoch = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let sequences = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        let e = epoch.clone();
        let s = stop.clone();
        let running = sequences.clone();
        let worker = thread::spawn(move || {
            while !s.load(Ordering::Acquire) {
                let job = match receiver.recv_timeout(Duration::from_millis(20)) {
                    Ok(j) => j,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                };
                let cancelled =
                    || s.load(Ordering::Acquire) || e.load(Ordering::Acquire) != job.epoch;
                let result = if cancelled() {
                    None
                } else {
                    Some(run(&job.action, job.node.as_deref(), &cancelled))
                };
                if matches!(job.action, Action::Sequence(_)) {
                    running.lock().unwrap().remove(&job.action.encode());
                }
                if let (Some(result), false) = (result, cancelled()) {
                    let ok = result.is_ok();
                    let _ = tx.try_send((
                        job.epoch,
                        job.action,
                        ok,
                        result.unwrap_or_else(|e| format!("Action failed: {e}")),
                    ));
                }
            }
        });
        Self {
            requests,
            notices,
            taps: None,
            sequences,
            epoch,
            stop,
            worker: Some(worker),
        }
    }
    pub fn new() -> Self {
        let (taps, tap_requests) = mpsc::sync_channel::<TapRequest>(4);
        let mut dispatcher = Self::with(move |action, node, cancelled| {
            let tap = |keys: Vec<evdev::KeyCode>| tap(&taps, keys, cancelled);
            perform(action, node, cancelled, &tap)
        });
        dispatcher.taps = Some(tap_requests);
        dispatcher
    }
    pub fn submit(&self, action: Action, node: Option<PathBuf>) -> Result<()> {
        action.validate()?;
        let sequence = matches!(action, Action::Sequence(_)).then(|| action.encode());
        if let Some(key) = &sequence {
            if !self.sequences.lock().unwrap().insert(key.clone()) {
                return Ok(()); // already queued or running: re-press ignored
            }
        }
        self.requests
            .try_send(Job {
                action,
                node,
                epoch: self.epoch.load(Ordering::Acquire),
            })
            .map_err(|error| {
                if let Some(key) = &sequence {
                    self.sequences.lock().unwrap().remove(key);
                }
                match error {
                    mpsc::TrySendError::Full(_) => QUEUE_FULL,
                    mpsc::TrySendError::Disconnected(_) => {
                        "action worker unavailable; request skipped"
                    }
                }
                .into()
            })
    }
    /// Key taps the worker is waiting on; the session answers each on its keyboard.
    pub fn tap_requests(&self) -> Vec<TapRequest> {
        self.taps
            .as_ref()
            .map(|taps| taps.try_iter().collect())
            .unwrap_or_default()
    }
    /// A sequence is queued or running (its re-press is ignored meanwhile).
    pub fn sequence_running(&self, action: &Action) -> bool {
        self.sequences.lock().unwrap().contains(&action.encode())
    }
    pub fn notice(&self) -> Option<(Action, bool, String)> {
        while let Ok((epoch, action, ok, notice)) = self.notices.try_recv() {
            if epoch == self.epoch.load(Ordering::Acquire) {
                return Some((action, ok, notice));
            }
        }
        None
    }
    pub fn cancel(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
}

/// Ask the session to tap `keys` and wait for its answer; a pause answers with an error.
fn tap(
    taps: &mpsc::SyncSender<TapRequest>,
    keys: Vec<evdev::KeyCode>,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    if cancelled() {
        return Err("cancelled".into());
    }
    let (reply, answer) = mpsc::sync_channel(1);
    taps.try_send((keys, reply))
        .map_err(|_| "keyboard request queue full")?;
    loop {
        match answer.recv_timeout(Duration::from_millis(20)) {
            Ok(result) => return result.map_err(Into::into),
            Err(mpsc::RecvTimeoutError::Timeout) if cancelled() => {
                return Err("cancelled".into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("keyboard request dropped".into());
            }
        }
    }
}

fn perform(
    action: &Action,
    node: Option<&Path>,
    cancelled: &dyn Fn() -> bool,
    tap: &dyn Fn(Vec<evdev::KeyCode>) -> Result<()>,
) -> Result<String> {
    match action {
        Action::Sequence(steps) => {
            run_sequence(steps, cancelled, |step| match step {
                Step::Agent(name) => ensure_agent(name, cancelled).map(drop),
                Step::Wait(ms) => wait(Duration::from_millis(u64::from(*ms)), cancelled),
                Step::Keys(keys) => tap(keys
                    .iter()
                    .map(|k| crate::keyboard::key_code(k))
                    .collect::<Result<_>>()?),
                Step::Do(action) => perform(action, None, cancelled, tap).map(drop),
            })?;
            Ok(format!("Sequence finished ({} steps)", steps.len()))
        }
        Action::Prompt {
            file,
            target: Target::Agent(agent),
            enter,
        } => send_prompt(file, agent, *enter, cancelled),
        Action::Prompt {
            file,
            target: Target::Focused { shift },
            enter,
        } => {
            copy_prompt(&prompts_dir()?.join(file), Command::new("wl-copy"))?;
            tap(crate::keyboard::paste_keys(*shift))?;
            if *enter {
                // Enter only after the application has read the clipboard and inserted it.
                wait(PASTE_SETTLE, cancelled)?;
                tap(vec![evdev::KeyCode::KEY_ENTER])?;
            }
            Ok(format!("Prompt {file} pasted into the focused window"))
        }
        Action::Command { window, dir, line } => run_command(window, dir, line),
        Action::Application(path) => launch(path, &cancelled),
        Action::AppToggle { path, close } => app_toggle(
            path,
            *close,
            |args| command("gdbus", args, &cancelled),
            || launch(path, &cancelled),
        ),
        Action::Flow => flow(node.ok_or("keyboard has not been registered")?, &cancelled),
        Action::Shortcut => Err("shortcut belongs to keyboard output".into()),
        Action::Choice(_) | Action::Value { .. } => Err("knob actions run in the session".into()),
        _ => audio(action, |args| command("wpctl", args, &cancelled)),
    }
}

// ponytail: steps wait fixed delays; no readiness detection. Use agent status (ticket 06)
// if fixed waits prove unreliable.
fn run_sequence(
    steps: &[Step],
    cancelled: &dyn Fn() -> bool,
    mut run: impl FnMut(&Step) -> Result<()>,
) -> Result<()> {
    for (number, step) in steps.iter().enumerate() {
        if cancelled() {
            return Err(format!("sequence cancelled before step {}", number + 1).into());
        }
        run(step)
            .map_err(|e| format!("step {} ({}) failed: {e}", number + 1, step.description()))?;
    }
    Ok(())
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.cancel();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

const FLOW_APP: &str = "/usr/lib/wispr-flow/wispr-flow";
const FLOW_HELPER: &str = "/usr/lib/wispr-flow/resources/Release/wispr-flow-linux-helper";
fn flow_main_args(args: &[u8]) -> bool {
    !args
        .split(|b| *b == 0 || b.is_ascii_whitespace())
        .any(|s| s.starts_with(b"--type="))
}
fn flow_processes() -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut main = Vec::new();
    let mut helper = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let p = entry?.path();
        if !p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .chars()
            .all(|c| c.is_ascii_digit())
        {
            continue;
        }
        if !fs::metadata(&p).is_ok_and(|m| m.uid() == unsafe { libc::geteuid() }) {
            continue;
        }
        let Ok(exe) = fs::read_link(p.join("exe")) else {
            continue;
        };
        if exe == Path::new(FLOW_HELPER) {
            helper.push(p);
        } else if exe == Path::new(FLOW_APP) {
            let args = fs::read(p.join("cmdline"))?;
            if flow_main_args(&args) {
                main.push(p);
            }
        }
    }
    if main.len() > 1 {
        return Err("multiple Flow owners; resolve manually before repair".into());
    }
    Ok((main, helper))
}
fn capture(node: &Path, helpers: &[PathBuf]) -> Result<bool> {
    let keyboard = fs::metadata(node)?;
    for helper in helpers {
        let Ok(fds) = fs::read_dir(helper.join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if fs::metadata(fd.path()).is_ok_and(|m| {
                m.dev() == keyboard.dev()
                    && m.ino() == keyboard.ino()
                    && m.rdev() == keyboard.rdev()
            }) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
fn wait_until(
    cancelled: &impl Fn() -> bool,
    mut ready: impl FnMut() -> Result<bool>,
) -> Result<()> {
    let started = Instant::now();
    loop {
        if cancelled() {
            return Err("Flow operation cancelled; output remains paused".into());
        }
        if ready()? {
            return Ok(());
        }
        if started.elapsed() >= Duration::from_secs(8) {
            return Err("Flow stage timed out; output remains paused".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum FlowStep {
    Quit,
    WaitStopped,
    Launch,
    Capture,
}
/// Not running: launch, then confirm capture. Running: one graceful quit and reopen, then
/// confirm capture. No retry loop.
fn flow_lifecycle(
    installed: bool,
    running: bool,
    registered: bool,
    mut stage: impl FnMut(FlowStep) -> Result<()>,
) -> Result<()> {
    if !installed {
        return Err("Wispr Flow is not installed (no visible wispr-flow.desktop)".into());
    }
    if !registered {
        return Err("current keyboard device is unavailable".into());
    }
    if running {
        stage(FlowStep::Quit).map_err(|e| format!("Flow graceful quit request: {e}"))?;
        stage(FlowStep::WaitStopped).map_err(|e| format!("Flow wait for app/helper exit: {e}"))?;
    }
    stage(FlowStep::Launch).map_err(|e| format!("Flow launch request: {e}"))?;
    stage(FlowStep::Capture).map_err(|e| format!("Flow current keyboard capture: {e}"))?;
    Ok(())
}
fn flow(node: &Path, cancelled: &impl Fn() -> bool) -> Result<String> {
    let result = (|| -> Result<String> {
        let path = flow_launcher()?;
        let (main, helper) = flow_processes()?;
        if main.is_empty() && !helper.is_empty() {
            // --quit-app needs a running app; never guess what a lone helper belongs to.
            return Err("stale Flow helper is running without the app; log out and in (or end it), then retry".into());
        }
        flow_lifecycle(path.is_some(), !main.is_empty(), node.exists(), |stage| {
            match stage {
                FlowStep::Quit => {
                    // Installed Electron second-instance handler supports app.quit via --quit-app.
                    // ponytail: exact inspected package paths; recheck adapter when Flow packaging changes.
                    command("/usr/bin/wispr-flow", &strings(&["--quit-app"]), cancelled)?;
                }
                FlowStep::WaitStopped => wait_until(cancelled, || {
                    let (m, h) = flow_processes()?;
                    Ok(m.is_empty() && h.is_empty())
                })?,
                FlowStep::Launch => {
                    launch(path.as_deref().ok_or("Flow launcher missing")?, cancelled)?;
                }
                FlowStep::Capture => wait_until(cancelled, || {
                    let (_, h) = flow_processes()?;
                    capture(node, &h)
                })?,
            }
            Ok(())
        })?;
        Ok("Flow helper opened the current keyboard; dictation remains unverified.".into())
    })();
    result.map_err(|e| format!("{e}. Manual recovery: fully Quit Wispr Flow from its tray; keep VibeConsole's keyboard ready; reopen Wispr Flow; test physical Shift; then select Run. No forced stop or settings reset was attempted.").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt_file_and_action_validation_without_tmux() {
        assert_eq!(
            prompt_tmux_args("/tmp/review", "=vibe:=claude", true),
            vec![
                vec!["load-buffer", "-b", "vibeconsole-prompt", "/tmp/review"],
                vec![
                    "paste-buffer",
                    "-d",
                    "-p",
                    "-b",
                    "vibeconsole-prompt",
                    "-t",
                    "=vibe:=claude"
                ],
                vec!["send-keys", "-t", "=vibe:=claude", "Enter"],
            ]
        );
        assert_eq!(
            prompt_tmux_args("/tmp/review", "=vibe:=codex", false).len(),
            2
        );
        let path =
            std::env::temp_dir().join(format!("vibeconsole-prompt-test-{}", std::process::id()));
        fs::write(&path, " \n").unwrap();
        assert!(read_prompt(&path).is_err());
        fs::write(&path, "line 1\nline 2\n").unwrap();
        assert_eq!(read_prompt(&path).unwrap(), b"line 1\nline 2\n");
        fs::remove_file(&path).unwrap();
        assert!(read_prompt(&path).is_err());
        let action = Action::Prompt {
            file: "review".into(),
            target: Target::Agent("claude".into()),
            enter: true,
        };
        assert_eq!(Action::parse(&action.encode()).unwrap(), action);
        for shift in [false, true] {
            let focused = Action::Prompt {
                file: "review".into(),
                target: Target::Focused { shift },
                enter: false,
            };
            assert_eq!(Action::parse(&focused.encode()).unwrap(), focused);
        }
        assert_eq!(
            Action::parse("prompt:review:@ctrl-shift-v:enter").unwrap(),
            Action::Prompt {
                file: "review".into(),
                target: Target::Focused { shift: true },
                enter: true,
            }
        );
        assert!(Action::parse("prompt:review:@other:enter").is_err());
        assert!(Action::parse("prompt:../secret:claude:enter").is_err());
        assert!(Action::parse("prompt:review:claude:bogus").is_err());
    }
    #[test]
    fn command_tmux_args_and_validation() {
        let tail = [
            "-n",
            "Bank-A-Pad-1",
            "-c",
            "/home/u/repo",
            "sh",
            "-c",
            "gh pr create; echo done",
            ";",
            "set-option",
            "-w",
            "remain-on-exit",
            "on",
        ];
        let args = command_tmux_args(
            true,
            "Bank-A-Pad-1",
            "/home/u/repo",
            "gh pr create; echo done",
            &[],
        );
        assert_eq!(args[..3], ["new-window", "-t", "=vibe:"]);
        assert_eq!(args[3..], tail);
        let args = command_tmux_args(
            false,
            "Bank-A-Pad-1",
            "/home/u/repo",
            "gh pr create; echo done",
            &[],
        );
        assert_eq!(args[..4], ["new-session", "-d", "-s", "vibe"]);
        assert_eq!(args[4..], tail);
        let command = Action::Command {
            window: "Bank-A-Pad-1".into(),
            dir: "/home/u/repo".into(),
            line: "echo a:b && claude --dangerously-skip-permissions".into(),
        };
        assert_eq!(Action::parse(&command.encode()).unwrap(), command);
        for bad in [
            "command:w:relative:ls",
            "command:w:/a\x1b:ls",
            "command:w:/tmp: ",
            "command:w:/tmp:ls\x07",
            "command:bad name:/tmp:ls",
            "command:w:/tmp",
        ] {
            assert!(Action::parse(bad).is_err(), "{bad}");
        }
        assert!(run_command("w", "/nonexistent/vibeconsole", "ls").is_err());
    }
    #[test]
    fn focused_prompt_feeds_the_file_to_wl_copy_and_rejects_empty_files() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-copy-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (prompt, out) = (dir.join("review"), dir.join("clipboard"));
        // Stand-in for wl-copy: records its stdin and arguments; no real clipboard is touched.
        let fake = || {
            let mut c = Command::new("sh");
            c.arg("-c")
                .arg(format!("cat > {0}; echo \"$0\" >> {0}", out.display()));
            c
        };
        fs::write(&prompt, "line 1\nline 2\n").unwrap();
        copy_prompt(&prompt, fake()).unwrap();
        assert_eq!(
            fs::read_to_string(&out).unwrap(),
            "line 1\nline 2\n--trim-newline\n"
        );
        fs::remove_file(&out).unwrap();
        fs::write(&prompt, "\n").unwrap();
        assert!(copy_prompt(&prompt, fake()).is_err());
        assert!(!out.exists());
        let mut failing = Command::new("false");
        failing.arg("--");
        fs::write(&prompt, "x").unwrap();
        assert!(copy_prompt(&prompt, failing).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn flow_order_explicit_repair_failure_and_exact_targets_are_recorded_only() {
        use crate::{Control, Event, keyboard::Keyboard, mappings::Mapping};
        let pad = Control::from_note(10, 36).unwrap();
        let mut keyboard = Keyboard::new(&[Mapping::new(pad, "Shift", "hold").unwrap()]).unwrap();
        let mut keys = Vec::new();
        keyboard
            .observe(Event::Press(pad), &mut |k, d| {
                keys.push((k, d));
                Ok(())
            })
            .unwrap();
        keyboard
            .pause(&mut |k, d| {
                keys.push((k, d));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            keys,
            [
                (evdev::KeyCode::KEY_LEFTSHIFT, true),
                (evdev::KeyCode::KEY_LEFTSHIFT, false)
            ]
        );
        // One command: start when Flow is not running, exactly one repair when it is.
        let run = |installed, running, registered, fail: Option<FlowStep>| {
            let mut stages = Vec::new();
            let result = flow_lifecycle(installed, running, registered, |s| {
                stages.push(s);
                if Some(s) == fail {
                    Err("stage failed".into())
                } else {
                    Ok(())
                }
            });
            (result.map_err(|e| e.to_string()), stages)
        };
        use FlowStep::*;
        assert_eq!(
            run(true, false, true, None),
            (Ok(()), vec![Launch, Capture])
        );
        assert_eq!(
            run(true, true, true, None),
            (Ok(()), vec![Quit, WaitStopped, Launch, Capture])
        );
        let (result, stages) = run(false, true, true, None);
        assert!(result.unwrap_err().contains("not installed"));
        assert!(stages.is_empty());
        let (result, stages) = run(true, true, false, None);
        assert!(result.is_err() && stages.is_empty());
        let (result, stages) = run(true, true, true, Some(WaitStopped));
        assert!(result.unwrap_err().contains("wait for app/helper exit"));
        assert_eq!(stages, [Quit, WaitStopped]); // no reopen after stop failure
        let (result, stages) = run(true, false, true, Some(Capture));
        assert!(result.unwrap_err().contains("current keyboard capture"));
        assert_eq!(stages, [Launch, Capture]); // capture failed: no retry loop
        assert_eq!(Action::parse("flow-start").unwrap(), Action::Flow);
        assert_eq!(Action::parse("flow-repair").unwrap(), Action::Flow);
        assert_eq!(Action::parse(&Action::Flow.encode()).unwrap(), Action::Flow);
        assert!(flow_main_args(
            b"/usr/lib/wispr-flow/wispr-flow --class=Wispr Flow\0"
        ));
        assert!(!flow_main_args(
            b"/usr/lib/wispr-flow/wispr-flow --type=zygote\0"
        ));
        assert!(!flow_main_args(
            b"/usr/lib/wispr-flow/wispr-flow\0--type=renderer\0"
        ));
        assert_eq!(FLOW_APP, "/usr/lib/wispr-flow/wispr-flow");
        assert_eq!(
            FLOW_HELPER,
            "/usr/lib/wispr-flow/resources/Release/wispr-flow-linux-helper"
        );
    }
    #[test]
    fn native_catalog_audio_targets_limits_and_dispatch_are_recorded_only() {
        assert!(
            desktop(
                "[Desktop Entry]\nType=Application\nName=Hidden\nExec=unsafe %U\nHidden=true",
                &["GNOME"]
            )
            .is_none()
        );
        assert_eq!(
            desktop(
                "[Desktop Entry]\nType=Application\nName=Visible\nExec=never execute this",
                &["GNOME"]
            ),
            Some("Visible".into())
        );
        assert!(
            Action::Volume {
                up: true,
                step: 101
            }
            .validate()
            .is_err()
        );
        let mut requests = Vec::new();
        for (id, action) in [
            (42, Action::Volume { up: true, step: 5 }),
            (51, Action::OutputMute),
            (60, Action::MicrophoneMute),
        ] {
            let result = audio(&action, |args| {
                requests.push(args.to_vec());
                Ok(match args[0].as_str() {
                    "inspect" => format!("id {id}, type PipeWire:Interface:Node"),
                    "get-volume" => "Volume: 1.00 [MUTED]".into(),
                    _ => String::new(),
                })
            })
            .unwrap();
            assert!(result.contains("Volume:"));
        }
        assert_eq!(
            requests[1],
            strings(&["set-volume", "42", "5%+", "--limit", "1.0"])
        );
        assert_eq!(requests[4], strings(&["set-mute", "51", "toggle"]));
        assert_eq!(requests[6], strings(&["inspect", "@DEFAULT_AUDIO_SOURCE@"]));
        assert!(audio(&Action::OutputMute, |_| Err("missing target".into())).is_err());
        let (entered, rx) = mpsc::channel();
        let dispatch = Dispatcher::with(move |_, _, cancelled| {
            entered.send(()).unwrap();
            while !cancelled() {
                thread::sleep(Duration::from_millis(1));
            }
            Err("cancelled slow request".into())
        });
        dispatch.submit(Action::OutputMute, None).unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        for _ in 0..8 {
            dispatch.submit(Action::OutputMute, None).unwrap();
        }
        let full = dispatch.submit(Action::OutputMute, None).unwrap_err();
        assert_eq!(full.to_string(), QUEUE_FULL);
        let started = Instant::now();
        dispatch.cancel();
        drop(dispatch);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn sequences_encode_validate_run_in_order_stop_on_failure_and_cancel() {
        let steps = vec![
            Step::Agent("claude".into()),
            Step::Wait(3000),
            Step::Do(Action::Prompt {
                file: "review".into(),
                target: Target::Agent("claude".into()),
                enter: true,
            }),
            Step::Do(Action::Command {
                window: "Bank-A-Pad-1".into(),
                dir: "/tmp".into(),
                line: "echo a:b".into(),
            }),
            Step::Do(Action::Application(
                "/usr/share/applications/x.desktop".into(),
            )),
            Step::Keys(vec!["Ctrl".into(), "V".into()]),
        ];
        let sequence = Action::Sequence(steps.clone());
        assert_eq!(Action::parse(&sequence.encode()).unwrap(), sequence);
        assert_eq!(sequence.agents(), ["claude", "claude"]); // ensure step + prompt step
        let focused = Action::Prompt {
            file: "x".into(),
            target: Target::Focused { shift: false },
            enter: false,
        };
        assert!(focused.agents().is_empty());
        for bad in [
            Action::Sequence(Vec::new()),
            Action::Sequence(vec![Step::Wait(99)]),
            Action::Sequence(vec![Step::Wait(60001)]),
            Action::Sequence(vec![Step::Do(Action::Sequence(vec![Step::Wait(100)]))]),
            Action::Sequence(vec![Step::Do(Action::Flow)]),
            Action::Sequence(vec![Step::Do(Action::Shortcut)]),
            Action::Sequence(vec![Step::Agent("bad name".into())]),
            Action::Sequence(vec![Step::Wait(100); 33]),
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        // Recorded run with a fake clock: waits advance it instead of sleeping.
        let mut clock = 0;
        let mut ran = Vec::new();
        run_sequence(&steps, &|| false, |step| {
            if let Step::Wait(ms) = step {
                clock += u32::from(*ms);
            }
            ran.push((clock, step.clone()));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            ran.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>(),
            steps
        );
        assert_eq!(ran[2].0, 3000); // the prompt runs after the 3 s wait
        let mut ran = Vec::new();
        let error = run_sequence(&steps, &|| false, |step| {
            ran.push(step.clone());
            if matches!(step, Step::Wait(_)) {
                Err("boom".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err()
        .to_string();
        assert_eq!(error, "step 2 (Wait 3000 ms) failed: boom");
        assert_eq!(ran.len(), 2);
        let cancel = std::cell::Cell::new(false);
        ran.clear();
        assert!(
            run_sequence(&steps, &|| cancel.get(), |step| {
                ran.push(step.clone());
                cancel.set(true); // pause during step 1
                Ok(())
            })
            .is_err()
        );
        assert_eq!(ran.len(), 1);
        // Keyboard steps go through the session's tap channel, in order.
        let keys = std::cell::RefCell::new(Vec::new());
        let short = Action::Sequence(vec![
            Step::Keys(vec!["Ctrl".into(), "V".into()]),
            Step::Wait(100),
            Step::Keys(vec!["Enter".into()]),
        ]);
        perform(&short, None, &|| false, &|k| {
            keys.borrow_mut().push(k);
            Ok(())
        })
        .unwrap();
        use evdev::KeyCode as K;
        assert_eq!(
            keys.into_inner(),
            [vec![K::KEY_LEFTCTRL, K::KEY_V], vec![K::KEY_ENTER]]
        );
    }
    #[test]
    fn repressing_a_running_sequence_is_ignored_and_taps_wait_for_the_session() {
        let (started, runs) = mpsc::channel();
        let release = Arc::new(AtomicBool::new(false));
        let go = release.clone();
        let dispatch = Dispatcher::with(move |action, _, _| {
            started.send(action.clone()).unwrap();
            while !go.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
            Ok("done".into())
        });
        let sequence = Action::Sequence(vec![Step::Wait(100)]);
        dispatch.submit(sequence.clone(), None).unwrap();
        runs.recv_timeout(Duration::from_secs(1)).unwrap();
        dispatch.submit(sequence.clone(), None).unwrap(); // re-press while running
        release.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(1);
        while dispatch.notice().is_none() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(runs.recv_timeout(Duration::from_millis(100)).is_err());
        dispatch.submit(sequence, None).unwrap(); // after it finished: runs again
        runs.recv_timeout(Duration::from_secs(1)).unwrap();
        // tap() blocks until the session answers, and gives up when cancelled.
        let (taps, requests) = mpsc::sync_channel::<TapRequest>(4);
        let session = thread::spawn(move || {
            let (keys, reply) = requests.recv().unwrap();
            reply.send(Ok(())).unwrap();
            keys
        });
        tap(&taps, vec![evdev::KeyCode::KEY_ENTER], &|| false).unwrap();
        assert_eq!(session.join().unwrap(), [evdev::KeyCode::KEY_ENTER]);
        let (taps, _unanswered) = mpsc::sync_channel::<TapRequest>(4);
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            c.store(true, Ordering::Release);
        });
        assert!(
            tap(&taps, vec![evdev::KeyCode::KEY_ENTER], &|| cancel
                .load(Ordering::Acquire))
            .is_err()
        );
    }

    /// Ticket 06: a desktop ID is passed to gio as an identifier; missing, rejected and
    /// failed-to-start launches say which.
    #[test]
    fn launcher_passes_desktop_ids_and_names_missing_rejected_and_failed_to_start() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-launch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let apps_dir = dir.join("applications");
        fs::create_dir_all(&apps_dir).unwrap();
        let ptyxis = apps_dir.join("org.gnome.Ptyxis.desktop");
        fs::write(
            &ptyxis,
            "[Desktop Entry]\nType=Application\nName=Terminal\nExec=ptyxis\nDBusActivatable=true\n",
        )
        .unwrap();
        let hidden = apps_dir.join("hidden.desktop");
        fs::write(
            &hidden,
            "[Desktop Entry]\nType=Application\nName=Hidden\nExec=x\nNoDisplay=true\n",
        )
        .unwrap();
        let apps = catalog_at(&[apps_dir.clone()], &["GNOME"]).unwrap();
        let path = ptyxis.to_str().unwrap();
        let mut sent = Vec::new();
        launch_request(path, &apps, true, |args| {
            sent.push(args.to_vec());
            Ok(String::new())
        })
        .unwrap();
        assert_eq!(sent, [vec!["launch".to_string(), path.to_string()]]);
        let error = |path: &Path, run: &dyn Fn() -> Result<String>| {
            launch_request(path.to_str().unwrap(), &apps, true, |_| run())
                .unwrap_err()
                .to_string()
        };
        let ok = || Ok(String::new());
        assert!(error(&apps_dir.join("gone.desktop"), &ok).starts_with("launcher missing:"));
        assert!(error(&hidden, &ok).starts_with("launcher rejected:"));
        let failing = || Err("gio: exit status: 2: Failed to launch".into());
        assert!(error(&ptyxis, &failing).starts_with("launcher failed to start:"));
        fs::remove_dir_all(dir).unwrap();
    }

    /// Ticket 07: a launcher pad asks the Shell extension for its app's windows. None: launch.
    /// Minimized or behind: restore. Focused: the saved second-press action. No extension: launch.
    #[test]
    fn app_toggle_launches_restores_minimizes_or_closes_and_reports_unavailable() {
        let path = "/usr/share/applications/org.gnome.Ptyxis.desktop";
        let run = |close: bool, state: Result<&'static str>| {
            let calls = std::cell::RefCell::new(Vec::new());
            let state = std::cell::RefCell::new(Some(state));
            let result = app_toggle(
                path,
                close,
                |args| {
                    calls.borrow_mut().push(args[9..].join(" "));
                    if args[9].ends_with(".State") {
                        state
                            .borrow_mut()
                            .take()
                            .unwrap()
                            .map(|s| format!("('{s}',)"))
                    } else {
                        Ok("(uint32 2,)".into())
                    }
                },
                || {
                    calls.borrow_mut().push("launch".into());
                    Ok("accepted".into())
                },
            );
            (result.map_err(|e| e.to_string()), calls.into_inner())
        };
        let state = "org.vibeconsole.Windows.State org.gnome.Ptyxis.desktop";
        let apply =
            |action| format!("org.vibeconsole.Windows.Apply org.gnome.Ptyxis.desktop {action}");
        let (result, calls) = run(false, Ok("none"));
        assert!(result.unwrap().starts_with("Launched"));
        assert_eq!(calls, [state.into(), "launch".to_string()]);
        let (result, calls) = run(false, Ok("focused"));
        assert!(result.unwrap().starts_with("Minimized"));
        assert_eq!(calls, [state.into(), apply("minimize")]);
        let (result, calls) = run(true, Ok("focused"));
        assert!(result.unwrap().starts_with("Asked"));
        assert_eq!(calls, [state.into(), apply("close")]);
        for hidden in ["minimized", "background"] {
            let (result, calls) = run(true, Ok(hidden));
            assert!(result.unwrap().starts_with("Restored"));
            assert_eq!(calls, [state.into(), apply("restore")]);
        }
        let (result, calls) = run(
            false,
            Err("The name org.vibeconsole.Windows was not provided".into()),
        );
        let text = result.unwrap();
        assert!(text.contains("minimize/close unavailable") && text.contains("not enabled"));
        assert_eq!(calls, [state.into(), "launch".to_string()]);
        let unknown = Err("GDBus.Error: unknown application org.gnome.Ptyxis.desktop".into());
        assert!(run(false, unknown).0.unwrap().contains("cannot identify")); // not blamed on install
        assert_eq!(
            desktop_id("/usr/share/applications/kde4/foo.desktop").unwrap(),
            "kde4-foo.desktop"
        );
        assert!(desktop_id("/opt/foo.desktop").is_none());
        let (result, _) = run(false, Ok("sideways"));
        assert!(result.unwrap_err().contains("unavailable"));
        let toggle = Action::AppToggle {
            path: path.into(),
            close: true,
        };
        assert_eq!(Action::parse(&toggle.encode()).unwrap(), toggle);
        assert!(Action::parse("app-toggle:close:relative.desktop").is_err());
    }

    /// Ticket 08: after tmux takes the command, a terminal attaches unless one already is;
    /// a terminal failure says "sent to tmux, not shown"; a tmux failure opens nothing.
    #[test]
    fn command_opens_one_attached_terminal_and_never_claims_an_unshown_session() {
        let dir = std::env::temp_dir();
        let dir = dir.to_str().unwrap();
        let run = |clients: &str, tmux_fails: bool, open_fails: bool| {
            let mut tmux_calls = Vec::new();
            let mut opened = 0;
            let result = show_command(
                "Bank-A-Pad-5",
                dir,
                "claude",
                &[],
                &mut |args| {
                    tmux_calls.push(args[0].to_string());
                    match args[0] {
                        "has-session" => Ok((true, String::new())),
                        "list-clients" => Ok((clients != "ERR", clients.to_string())),
                        _ if tmux_fails => Ok((false, "no server running".into())),
                        _ => Ok((true, String::new())),
                    }
                },
                &mut || {
                    opened += 1;
                    if open_fails {
                        Err("no terminal emulator installed".into())
                    } else {
                        Ok(())
                    }
                },
            );
            (result.map_err(|e| e.to_string()), tmux_calls, opened)
        };
        let (result, calls, opened) = run("", false, false);
        assert!(result.unwrap().contains("terminal opened"));
        assert_eq!(calls, ["has-session", "new-window", "list-clients"]);
        assert_eq!(opened, 1);
        let (result, _, opened) = run("/dev/pts/3\n", false, false);
        assert!(result.unwrap().contains("already attached"));
        assert_eq!(opened, 0);
        let (result, _, opened) = run("ERR", false, false); // failed listing is not a client
        assert!(result.unwrap().contains("terminal opened"));
        assert_eq!(opened, 1);
        let (result, _, _) = run("", false, true);
        let text = result.unwrap();
        assert!(text.contains("sent to tmux, not shown") && !text.contains("terminal opened"));
        let (result, _, opened) = run("", true, false);
        assert!(result.unwrap_err().contains("no server running"));
        assert_eq!(opened, 0);
        // The terminal comes from its desktop entry: program plus X-ExecArg, never a shell line.
        let ptyxis = "[Desktop Entry]\nType=Application\nName=Terminal\nExec=ptyxis\nX-ExecArg=--\nCategories=GNOME;System;TerminalEmulator;\n";
        let xterm = "[Desktop Entry]\nType=Application\nName=XTerm\nExec=xterm %u\nCategories=System;TerminalEmulator;\n";
        let editor =
            "[Desktop Entry]\nType=Application\nName=Editor\nExec=gedit\nCategories=Utility;\n";
        let attach = ["tmux", "attach-session", "-t", "=vibe"];
        assert_eq!(
            terminal_argv(&[editor, ptyxis]).unwrap()[..2],
            ["ptyxis", "--"]
        );
        assert_eq!(
            terminal_argv(&[xterm]).unwrap(),
            ["xterm", "-e", attach[0], attach[1], attach[2], attach[3]]
        );
        let flatpak = "[Desktop Entry]\nType=Application\nName=T\nExec=/usr/bin/flatpak run org.x.T\nCategories=TerminalEmulator;\n";
        assert_eq!(terminal_argv(&[flatpak, xterm]).unwrap()[0], "xterm"); // wrappers skipped
        assert!(terminal_argv(&[editor]).is_none());
    }

    /// Ticket 10: a status sequence is running while queued or running, stopped after.
    #[test]
    fn sequence_status_is_running_until_its_worker_finishes() {
        let (release, wait) = mpsc::channel::<()>();
        let wait = std::sync::Mutex::new(wait);
        let dispatcher = Dispatcher::with(move |_, _, _| {
            wait.lock().unwrap().recv().unwrap();
            Ok("done".into())
        });
        let sequence = Action::Sequence(vec![Step::Wait(100)]);
        assert!(!dispatcher.sequence_running(&sequence));
        dispatcher.submit(sequence.clone(), None).unwrap();
        assert!(dispatcher.sequence_running(&sequence));
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while dispatcher.notice().is_none() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!dispatcher.sequence_running(&sequence));
    }

    /// As a systemd service, anything that must outlive VibeConsole (the tmux server, the
    /// terminal it opens) starts in its own scope; systemd kills the service's group on stop.
    #[test]
    fn long_lived_children_get_their_own_scope_only_inside_a_unit() {
        let argv = |c: &Command| {
            std::iter::once(c.get_program())
                .chain(c.get_args())
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(argv(&scoped("tmux", false)), ["tmux"]);
        assert_eq!(
            argv(&scoped("tmux", true)),
            [
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "--",
                "tmux"
            ]
        );
    }

    /// Tickets 11–12: agents start in their override, else the Settings default, else tmux's
    /// default; startup variables reach every agent and command tmux starts.
    #[test]
    fn agents_and_commands_get_their_directory_and_startup_variables() {
        let env = vec!["ANTHROPIC_MODEL=opus".to_string()];
        let tag = "VIBECONSOLE_AGENT=claude";
        let args = agent_tmux_args(false, "claude", tag, "claude", Some("/work/override"), &env);
        assert_eq!(
            args,
            [
                "new-session",
                "-d",
                "-s",
                "vibe",
                "-n",
                "claude",
                "-c",
                "/work/override",
                "-e",
                tag,
                "-e",
                "ANTHROPIC_MODEL=opus",
                "claude"
            ]
        );
        let args = agent_tmux_args(true, "claude", tag, "claude", None, &[]);
        assert_eq!(
            args,
            [
                "new-window",
                "-t",
                "=vibe:",
                "-n",
                "claude",
                "-e",
                tag,
                "claude"
            ]
        );
        let args = command_tmux_args(true, "w", "/tmp", "echo $ANTHROPIC_MODEL", &env);
        let at = args.iter().position(|a| *a == "-e").unwrap();
        assert_eq!(args[at + 1], "ANTHROPIC_MODEL=opus");
        assert!(at < args.iter().position(|a| *a == "sh").unwrap()); // options before the command
    }
}
