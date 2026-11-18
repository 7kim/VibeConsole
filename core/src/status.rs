//! Agent status: `vibeconsole agent-status` writes one small file per agent; the session reads them.
use crate::Result;
use crate::feedback::Settings;
use std::path::{Path, PathBuf};

pub const STATES: [&str; 5] = [
    "idle",
    "thinking",
    "needs-input",
    "finished",
    "needs-decision",
];
/// Set in every tmux window VibeConsole launches, so hooks from other sessions are ignored.
pub const AGENT_ENV: &str = "VIBECONSOLE_AGENT";

/// Where agent state and context files live (`$XDG_RUNTIME_DIR/vibeconsole`).
pub fn state_dir() -> Result<PathBuf> {
    dir()
}
pub(crate) fn dir() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .ok_or("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(runtime).join("vibeconsole"))
}

fn write_in(dir: &Path, agent: &str, state: &str) -> Result<()> {
    if !crate::mappings::simple_name(agent) || !STATES.contains(&state) {
        return Err(format!("unknown agent state {state:?}; use {}", STATES.join(", ")).into());
    }
    std::fs::create_dir_all(dir)?;
    crate::actions::write_atomic(&dir.join(format!("agent-{agent}")), &format!("{state}\n"))
}

/// Missing or unreadable files read as idle.
fn read_in(dir: &Path, agent: &str) -> &'static str {
    let text = std::fs::read_to_string(dir.join(format!("agent-{agent}"))).unwrap_or_default();
    STATES
        .into_iter()
        .find(|s| *s == text.trim())
        .unwrap_or("idle")
}

pub fn write(agent: &str, state: &str) -> Result<()> {
    write_in(&dir()?, agent, state)
}

pub fn read(agent: &str) -> &'static str {
    dir().map_or("idle", |d| read_in(&d, agent))
}

/// `agent-status <agent|-> <state> [ignored]`. `-` means "the agent in $VIBECONSOLE_AGENT", and
/// does nothing outside VibeConsole's windows. The optional extra argument is the JSON Codex
/// `notify` appends. Prints nothing: Claude Code adds UserPromptSubmit hook output to the prompt.
pub fn command(args: &[String]) -> Result<()> {
    let (agent, state) = match args {
        [agent, state] | [agent, state, _] => (agent.clone(), state.as_str()),
        _ => return Err("usage: vibeconsole agent-status <agent|-> <state>".into()),
    };
    // SessionEnd: the agent is gone; nothing waits on it any more.
    let state = if state == "ended" { "idle" } else { state };
    let hook = agent == "-";
    let agent = if hook {
        match std::env::var(AGENT_ENV) {
            Ok(name) if !name.is_empty() => name,
            _ => return session_hook(state),
        }
    } else {
        agent
    };
    // v7.7 ticket 197: a hook reads only; the session moves an old mappings.tsv.
    let config = crate::mappings::read_config(&crate::mappings::config_path()?)?;
    let Some(window) = config
        .agents
        .iter()
        .find(|a| a.name == agent)
        .map(|a| a.window.clone())
    else {
        return Err(format!("unknown agent {agent:?}").into());
    };
    // A process that outlives its window keeps the variable: e.g. Codex's shared app-server
    // daemon, started from the agent's window and then used by other Codex clients. Only a
    // live pane in the agent's own `vibe` window may report.
    if hook && !own_window(pane_window().as_deref(), &window) {
        return session_hook(state);
    }
    write(&agent, state)
}

/// Ticket 16: a hook from a session VibeConsole did not start reports by `session_id` and
/// `cwd` from the hook's stdin JSON. Which folder source it lights is decided when read, so
/// Settings changes apply at once. Without hook input (a terminal) nothing is recorded.
fn session_hook(state: &str) -> Result<()> {
    // SAFETY: isatty only inspects descriptor 0.
    if unsafe { libc::isatty(0) } == 1 {
        return Ok(());
    }
    let mut input = String::new();
    std::io::Read::read_to_string(
        &mut std::io::Read::take(std::io::stdin(), 1 << 20),
        &mut input,
    )?;
    let (pid, kind) = agent_process();
    session_report(&dir()?, &input, state, pid, kind)
}

/// `kind` is the agent program (`claude`, `codex`) or "" when unknown (v7 ticket 59).
fn session_report(dir: &Path, input: &str, state: &str, pid: u32, kind: &str) -> Result<()> {
    if !STATES.contains(&state) {
        return Err(format!("unknown agent state {state:?}; use {}", STATES.join(", ")).into());
    }
    if input.trim().is_empty() {
        return Ok(()); // no hook payload: nothing to attribute
    }
    let hook: serde_json::Value = serde_json::from_str(input)?;
    let (Some(id), Some(cwd)) = (hook["session_id"].as_str(), hook["cwd"].as_str()) else {
        return Ok(()); // not a hook payload: nothing to attribute
    };
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        || cwd.contains(['\t', '\n'])
    {
        return Err("invalid session_id or cwd in hook input".into());
    }
    let file = dir.join(format!("session-{id}"));
    if state == "idle" {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(dir)?;
    let kind = if kind.is_empty() { "-" } else { kind };
    crate::actions::write_atomic(&file, &format!("{state}\t{pid}\t{kind}\t{cwd}\n"))
}

/// The Claude Code or Codex process this hook runs under (it runs hooks through a shell) and
/// its program name, or (0, "") when none is found; a session whose process is gone is dropped.
fn agent_process() -> (u32, &'static str) {
    // SAFETY: getppid has no preconditions.
    let mut pid = unsafe { libc::getppid() } as u32;
    for _ in 0..8 {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return (0, "");
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            return (0, "");
        };
        if let Some(kind) = KINDS.into_iter().find(|k| *k == &stat[open + 1..close]) {
            return (pid, kind);
        }
        match stat[close + 1..]
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse().ok())
        {
            Some(parent) if parent > 1 => pid = parent,
            _ => return (0, ""),
        }
    }
    (0, "")
}
/// Agent programs whose hooks report sessions.
const KINDS: [&str; 2] = ["claude", "codex"];

/// A session outside VibeConsole's tmux: its state and working folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Session {
    pub state: &'static str,
    pub cwd: String,
    /// The agent program that reported it (`claude`, `codex`); "" when not known (v7 ticket 59).
    pub kind: &'static str,
}

/// Recorded sessions, dropping those whose agent process is gone, plus Orca's agents in
/// folders our hooks don't cover (v7 ticket 16).
pub fn sessions() -> Vec<Session> {
    let hooks = dir()
        .map(|d| sessions_in(&d, |pid| Path::new(&format!("/proc/{pid}")).exists()))
        .unwrap_or_default();
    let orca = std::env::var_os("HOME")
        .map(|home| Path::new(&home).join(".config/orca/agent-hooks/last-status.json"))
        .filter(|path| std::fs::metadata(path).is_ok_and(|m| m.len() <= 8 << 20))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| orca_sessions(&text))
        .unwrap_or_default();
    let mut fallback = orca;
    fallback.extend(herdr_sessions(&herdr_list()));
    with_fallback(hooks, fallback)
}
/// The last `herdr agent list` answer, refreshed on a background thread (2 s at most each),
/// so a slow herdr never stalls the session loop; empty when herdr isn't installed.
fn herdr_list() -> String {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    static LAST: Mutex<String> = Mutex::new(String::new());
    static ASKING: AtomicBool = AtomicBool::new(false);
    let installed = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| dir.join("herdr").is_file());
    if !installed {
        return String::new();
    }
    if !ASKING.swap(true, Ordering::AcqRel) {
        std::thread::spawn(|| {
            let answer = std::process::Command::new("timeout")
                .args(["2", "herdr", "agent", "list"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            *LAST.lock().unwrap_or_else(|e| e.into_inner()) = answer;
            ASKING.store(false, Ordering::Release);
        });
    }
    LAST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
/// herdr's agents as a light source (v7 ticket 18; herdr.dev/docs/socket-api): each
/// agent's state and folder (`foreground_cwd`, else `cwd`). `done` lights as finished,
/// `unknown` is ignored; our hooks win (see `with_fallback`).
pub fn herdr_sessions(json: &str) -> Vec<Session> {
    herdr_agents(json)
        .into_iter()
        .filter_map(|(_, state, folder)| {
            let state = match state.as_str() {
                "working" => "thinking",
                "blocked" => "needs-input",
                "done" => "finished",
                "idle" => "idle",
                _ => return None,
            };
            folder.map(|cwd| Session {
                state,
                cwd,
                kind: "",
            })
        })
        .collect()
}
/// (name, state, folder) per agent in `herdr agent list` output, whichever envelope it uses.
pub fn herdr_agents(json: &str) -> Vec<(String, String, Option<String>)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let list = [
        &value["result"]["agents"],
        &value["result"],
        &value["agents"],
        &value,
    ]
    .into_iter()
    .find_map(|v| v.as_array());
    let Some(list) = list else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|agent| {
            let name = ["name", "agent"]
                .iter()
                .find_map(|k| agent[*k].as_str())?
                .to_owned();
            let state = ["agent_status", "state", "status"]
                .iter()
                .find_map(|k| agent[*k].as_str())?
                .to_owned();
            let folder = ["foreground_cwd", "cwd"]
                .iter()
                .find_map(|k| agent[*k].as_str())
                .filter(|f| f.starts_with('/') && !f.contains(['\t', '\n']))
                .map(str::to_owned);
            Some((name, state, folder))
        })
        .collect()
}
/// Orca's own status file as a fallback source: each entry's `payload.state` and the folder
/// in its `worktreeId` (`<id>::<folder>`), nothing else; prompts and messages in it are
/// never kept. A state Orca adds later, or a file it reshapes, is ignored, not an error.
pub fn orca_sessions(text: &str) -> Vec<Session> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(entries) = value["entries"].as_object() else {
        return Vec::new();
    };
    entries
        .values()
        .filter_map(|entry| {
            let state = match entry["payload"]["state"].as_str()? {
                "working" | "thinking" | "running" => "thinking",
                "waiting" | "blocked" | "needs-input" | "permission" => "needs-input",
                "done" | "finished" | "completed" => "finished",
                "idle" => "idle",
                _ => return None,
            };
            let (_, folder) = entry["worktreeId"].as_str()?.split_once("::")?;
            (folder.starts_with('/') && !folder.contains(['\t', '\n'])).then(|| Session {
                state,
                cwd: folder.to_owned(),
                kind: "",
            })
        })
        .collect()
}
/// Our hooks win: an Orca session counts only for a folder no hook session reports.
fn with_fallback(hooks: Vec<Session>, orca: Vec<Session>) -> Vec<Session> {
    let covered = hooks.iter().map(|s| s.cwd.clone()).collect::<Vec<_>>();
    let mut all = hooks;
    all.extend(orca.into_iter().filter(|s| !covered.contains(&s.cwd)));
    all
}

fn sessions_in(dir: &Path, alive: impl Fn(u32) -> bool) -> Vec<Session> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("session-") {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
        // `state pid kind cwd`; files from before v7 ticket 59 have no kind.
        let mut fields = text.trim_end_matches('\n').splitn(4, '\t');
        let (Some(state), Some(pid), Some(third)) = (fields.next(), fields.next(), fields.next())
        else {
            let _ = std::fs::remove_file(entry.path());
            continue;
        };
        let (kind, cwd) = match fields.next() {
            Some(cwd) => (KINDS.into_iter().find(|k| *k == third).unwrap_or(""), cwd),
            None => ("", third),
        };
        let state = STATES.into_iter().find(|s| *s == state);
        let pid = pid.parse::<u32>().ok();
        // ponytail: no process found (pid 0) relies on SessionEnd; a crash then leaves the
        // state until the next prompt in that session. Add an age limit if that bites.
        match (state, pid) {
            (Some(state), Some(pid)) if pid == 0 || alive(pid) => found.push(Session {
                state,
                cwd: cwd.to_owned(),
                kind,
            }),
            _ => {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    found
}

/// v7 ticket 59: sessions that belong to one Orca-hosted agent, matched by host facts only:
/// the same agent kind, inside the agent's folder (the most specific folder wins). `hosted`
/// is (agent name, kind, folder). Returns each matched agent's most urgent state, and the
/// sessions left for folder lights; a session two agents could claim stays a folder session.
pub fn hosted_states(
    sessions: Vec<Session>,
    hosted: &[(String, String, String)],
) -> (Vec<(String, &'static str)>, Vec<Session>) {
    let urgency = |state: &str| {
        std::cmp::Reverse(BUSY.iter().position(|s| *s == state).unwrap_or(BUSY.len()))
    };
    let mut states: Vec<(String, &'static str)> = Vec::new();
    let mut rest = Vec::new();
    for session in sessions {
        let candidates = hosted
            .iter()
            .filter(|(_, kind, folder)| {
                !session.kind.is_empty() && *kind == session.kind && within(&session.cwd, folder)
            })
            .collect::<Vec<_>>();
        let longest = candidates
            .iter()
            .map(|(_, _, f)| f.trim_end_matches('/').len())
            .max();
        let best = candidates
            .iter()
            .filter(|(_, _, f)| Some(f.trim_end_matches('/').len()) == longest)
            .collect::<Vec<_>>();
        let [(name, _, _)] = best.as_slice() else {
            rest.push(session); // none, or ambiguous: light by folder only
            continue;
        };
        match states.iter_mut().find(|(n, _)| n == name) {
            Some(entry) if urgency(session.state) > urgency(entry.1) => entry.1 = session.state,
            Some(_) => {}
            None => states.push((name.clone(), session.state)),
        }
    }
    (states, rest)
}

/// Per light source, the most urgent state of the sessions it matches: the most specific
/// folder containing the session's `cwd`, else Every other session when it is on (one entry
/// per session folder). Returns (folder, state, octave) for busy sources only.
pub fn source_states(
    sessions: &[Session],
    folders: &[(String, i8)],
    other: Option<i8>,
) -> Vec<(String, &'static str, i8)> {
    // `BUSY` order, matching `lights`: lower index is more urgent.
    let urgency = |state: &str| {
        std::cmp::Reverse(BUSY.iter().position(|s| *s == state).unwrap_or(BUSY.len()))
    };
    let mut busy: Vec<(String, &'static str, i8)> = Vec::new();
    for session in sessions.iter().filter(|s| s.state != "idle") {
        let folder = folders
            .iter()
            .filter(|(folder, _)| within(&session.cwd, folder))
            .max_by_key(|(folder, _)| folder.len());
        let (name, light) = match (folder, other) {
            (Some((folder, light)), _) => (folder.clone(), *light),
            // Every other session: one entry per project folder, named by it on screen.
            (None, Some(light)) => (session.cwd.clone(), light),
            (None, None) => continue,
        };
        match busy.iter_mut().find(|(n, _, _)| *n == name) {
            Some(entry) if urgency(session.state) > urgency(entry.1) => entry.1 = session.state,
            Some(_) => {}
            None => busy.push((name, session.state, light)),
        }
    }
    busy
}

/// `cwd` is `folder` or inside it, by whole path components.
fn within(cwd: &str, folder: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    cwd == folder
        || cwd
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `session<TAB>window` of the pane in $TMUX_PANE, None when there is no such live pane.
fn pane_window() -> Option<String> {
    let pane = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty())?;
    let output = std::process::Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            &pane,
            "#{session_name}\t#{window_name}",
        ])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(String::from_utf8(output.stdout).ok()?.trim_end().to_owned())
}

fn own_window(pane: Option<&str>, window: &str) -> bool {
    pane == Some(format!("vibe\t{window}").as_str())
}

/// Finished agents to clear: the attached `vibe` session shows that agent's window.
/// `active` is (session attached, active window name) from tmux, None when unavailable.
pub fn attended<'a>(
    agents: &'a [(String, String, &'static str)],
    active: Option<(bool, String)>,
) -> Vec<&'a str> {
    let Some((true, window)) = active else {
        return Vec::new();
    };
    agents
        .iter()
        .filter(|(_, w, state)| *state == "finished" && *w == window)
        .map(|(name, _, _)| name.as_str())
        .collect()
}

/// Busy agents whose tmux window is gone (the agent exited, or `tmux kill-server`). Their hooks
/// can no longer report, so the state would stick, and so would the status lights.
/// `alive` answers per window, None when tmux cannot be asked (then nothing is cleared).
pub fn vanished<'a>(
    agents: &'a [(String, String, &'static str)],
    alive: impl Fn(&str) -> Option<bool>,
) -> Vec<&'a str> {
    agents
        .iter()
        .filter(|(_, window, state)| *state != "idle" && alive(window) == Some(false))
        .map(|(name, _, _)| name.as_str())
        .collect()
}

/// Attached flag and active window of session `vibe`, if it exists.
pub fn tmux_active() -> Option<(bool, String)> {
    let output = std::process::Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            "=vibe:",
            "#{session_attached} #{window_name}",
        ])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8(output.stdout).ok()?;
    let (attached, window) = text.trim_end().split_once(' ')?;
    Some((attached != "0", window.into()))
}

/// The tempo light per state (ticket 17): slow and fast are the two BPM values in Patterns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tempo {
    Off,
    Slow,
    Fast,
}
/// The octave light per state, on the source's own octave (which agent or folder).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Octave {
    Off,
    Steady,
    Blink,
    /// The source's octave and its negative, twice a second ("needs a decision").
    Alternate,
}
pub const TEMPOS: [(&str, Tempo); 3] = [
    ("off", Tempo::Off),
    ("slow", Tempo::Slow),
    ("fast", Tempo::Fast),
];
pub const OCTAVES: [(&str, Octave); 4] = [
    ("off", Octave::Off),
    ("steady", Octave::Steady),
    ("blink", Octave::Blink),
    ("alternate", Octave::Alternate),
];
pub fn tempo_word(value: Tempo) -> &'static str {
    word(&TEMPOS, value)
}
pub fn octave_word(value: Octave) -> &'static str {
    word(&OCTAVES, value)
}
/// Busy states, most urgent first: the order that decides the shared tempo light.
pub const BUSY: [&str; 4] = ["needs-decision", "needs-input", "thinking", "finished"];

/// v7 ticket 86: what a controller's own light shows for a state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Light {
    Off,
    Steady,
    SlowPulse,
    FastBlink,
    /// Once a second.
    Blink,
    /// v7 ticket 133: the controller's slow blink (also its paused light) and fast fade.
    SlowBlink,
    FastPulse,
    /// v7 ticket 134: on and off at the Status light patterns' slow or fast BPM, switched by
    /// VibeConsole itself (`at_tempo`), like the MPK's tempo light.
    TempoSlow,
    TempoFast,
}
/// Every pattern word. The Xbox button light can show all of them.
pub const LIGHTS: [(&str, Light); 9] = [
    ("off", Light::Off),
    ("steady", Light::Steady),
    ("slow-pulse", Light::SlowPulse),
    ("fast-blink", Light::FastBlink),
    ("blink", Light::Blink),
    ("slow-blink", Light::SlowBlink),
    ("fast-pulse", Light::FastPulse),
    ("tempo-slow", Light::TempoSlow),
    ("tempo-fast", Light::TempoFast),
];
/// v7 ticket 133: what a keyboard's on/off lock LED can show; the controller-only modes are
/// left out.
pub const KEYBOARD_LIGHTS: [(&str, Light); 5] = [
    ("off", Light::Off),
    ("steady", Light::Steady),
    ("slow-pulse", Light::SlowPulse),
    ("fast-blink", Light::FastBlink),
    ("blink", Light::Blink),
];
/// v7 ticket 86: a rumble as a state begins. Declared shortest first: `max` picks the longest.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Rumble {
    Off,
    Short,
    Long,
}
pub const RUMBLES: [(&str, Rumble); 3] = [
    ("off", Rumble::Off),
    ("short", Rumble::Short),
    ("long", Rumble::Long),
];
/// The config word for a pattern value (`LIGHTS`, `RUMBLES`, …).
pub fn word<T: PartialEq>(list: &[(&'static str, T)], value: T) -> &'static str {
    list.iter()
        .find(|(_, v)| *v == value)
        .map_or("off", |(w, _)| w)
}
/// The DualSense light-bar colours offered (v7 ticket 86).
pub const COLOURS: [&str; 6] = ["blue", "orange", "red", "green", "purple", "white"];
/// v7 ticket 86: status-light patterns per device, in `BUSY` order, each device with its own
/// switch under the master Status lights switch. The MPK's are `Patterns`.
/// ponytail: the DualSense rows are kept but drive nothing until VibeConsole reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevicePatterns {
    /// MPK, Xbox, DualSense.
    pub on: [bool; 3],
    pub xbox: [(Light, Rumble); 4],
    pub ps5: [(&'static str, Light, Rumble); 4],
    /// v7 ticket 113 (config v26): the computer keyboard's lock-key LED, off until turned on.
    pub keyboard_on: bool,
    pub keyboard: [Light; 4],
    /// v7 ticket 128 (config v28): which keyboard lights show it, in `KEY_LEDS` order. None
    /// chosen: Scroll Lock, else Caps Lock (ticket 113).
    pub keyboard_leds: [bool; 4],
    /// v7 ticket 132 (config v29): the Xbox button light's brightness per state, 0–50.
    pub xbox_brightness: [u8; 4],
    /// v7.7 ticket 199: the RGB devices (`RGB_DEVICES`: keyboard, speakers, mousepad, mouse
    /// logo) follow one shared effect per state and one resting effect; a device's own
    /// set overrides it while it has one. Each device is off until turned on.
    pub rgb_shared: RgbSet,
    pub rgb_devices: [RgbDevice; 4],
    /// v7 ticket 128: the Razer keyboard's brightness while it shows a state (its own comes
    /// back after). v7.7 ticket 209: one per state, in `BUSY` order. The accessories' and the
    /// mouse's brightness are left alone (ticket 189).
    pub rgb_brightness: [u8; 4],
    /// v7.7 ticket 212 (lights.json v3): breath and spectrum run from VibeConsole's one
    /// clock on every RGB device, so they pulse together; off: each device's own firmware
    /// effect, as before.
    pub rgb_in_step: bool,
}
/// v7.7 ticket 212: the cycle of an in-step breath or spectrum that sets none, so the
/// Razer frames and the G502 run at the same speed.
/// ponytail: one fixed default; a setting for it when someone wants another.
pub const STEP_PERIOD_MS: u16 = 4000;
/// v7.7 ticket 212: an in-step effect at `ms` into its cycle, as the static colour to write
/// now. Breath fades its colour by its form (sine, square, triangle, sawtooth; shark-fin and
/// exponential as sine) up to its intensity; two colours take a cycle each; spectrum turns
/// the hue once a period. None: an effect that isn't stepped (it stays the firmware's).
pub fn rgb_frame(effect: Rgb, ms: u128) -> Option<Rgb> {
    let period = |t: Timing| u128::from(t.period.unwrap_or(STEP_PERIOD_MS).max(1));
    let peak = |t: Timing| f64::from(t.intensity.unwrap_or(100).min(100)) / 100.0;
    let scale = |c: Colour, level: f64| c.map(|v| (f64::from(v) * level).round() as u8);
    let fade = |x: f64, form: Option<u8>| match form.map(|f| FORMS[usize::from(f) % FORMS.len()]) {
        Some("square") => f64::from(u8::from(x < 0.5)),
        Some("triangle") => 1.0 - (2.0 * x - 1.0).abs(),
        Some("sawtooth") => x,
        _ => (1.0 - (std::f64::consts::TAU * x).cos()) / 2.0,
    };
    let phase = |p: u128| (ms % p) as f64 / p as f64;
    Some(Rgb::Static(match effect {
        Rgb::Breath(c, t) => scale(c, fade(phase(period(t)), t.form) * peak(t)),
        Rgb::BreathTwo(a, b) => {
            let p = period(Timing::default());
            let c = if ms % (2 * p) < p { a } else { b };
            scale(c, fade(phase(p), None))
        }
        Rgb::Spectrum(t) => {
            let h = phase(period(t)) * 6.0;
            let x = 1.0 - (h % 2.0 - 1.0).abs();
            let (r, g, b) = match h as u8 {
                0 => (1.0, x, 0.0),
                1 => (x, 1.0, 0.0),
                2 => (0.0, 1.0, x),
                3 => (0.0, x, 1.0),
                4 => (x, 0.0, 1.0),
                _ => (1.0, 0.0, x),
            };
            let k = peak(t) * 255.0;
            [r, g, b].map(|v: f64| (v * k).round() as u8)
        }
        _ => return None,
    }))
}
/// v7.7 ticket 212: what the G502 is sent for an in-step effect: breath and spectrum get the
/// period the Razer frames use, so it pulses at their speed.
pub fn in_step_mouse(effect: Rgb) -> Rgb {
    let fill = |t: Timing| Timing {
        period: Some(t.period.unwrap_or(STEP_PERIOD_MS)),
        ..t
    };
    match effect {
        Rgb::Breath(c, t) => Rgb::Breath(c, fill(t)),
        Rgb::Spectrum(t) => Rgb::Spectrum(fill(t)),
        other => other,
    }
}
/// v7.7 ticket 199: an effect per busy state (`BUSY` order) and the resting effect shown
/// when idle, paused and after quit. The G502 logo rests on its own stored lighting instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RgbSet {
    pub rest: Rgb,
    pub states: [Rgb; 4],
}
/// v7.7 ticket 199: an RGB device's switch and its own set, which overrides the shared one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RgbDevice {
    pub on: bool,
    pub own: Option<RgbSet>,
}
impl DevicePatterns {
    /// The set an RGB device shows: its own, else the shared one.
    pub fn rgb_set(&self, device: usize) -> RgbSet {
        self.rgb_devices[device].own.unwrap_or(self.rgb_shared)
    }
}
/// v7.7 ticket 199: the RGB devices: config key, name shown, and the effects each shows
/// (ticket 193's table: OpenRazer 3.12.4 files, logirgb 0.6.0's logo zone). The Firefly's
/// reactive lights only on a trigger VibeConsole doesn't send, so it isn't offered.
pub const RGB_DEVICES: [(&str, &str, &[RgbKind]); 4] = {
    use RgbKind::*;
    [
        (
            "keyboard_rgb",
            "Keyboard RGB (Razer, needs OpenRazer's driver)",
            &[
                Off,
                Static,
                Breath,
                BreathTwo,
                BreathRandom,
                Reactive,
                Wave,
                Spectrum,
            ],
        ),
        (
            "speakers_rgb",
            "Speakers RGB (Razer Nommo)",
            &[Off, Static, Breath, BreathTwo, BreathRandom, Wave, Spectrum],
        ),
        (
            "mousepad_rgb",
            "Mousepad RGB (Razer Firefly)",
            &[Off, Static, Breath, BreathTwo, BreathRandom, Wave, Spectrum],
        ),
        (
            "mouse_rgb",
            "Mouse light (Logitech G502 logo, needs logirgb)",
            &[Off, Static, Breath, Spectrum],
        ),
    ]
};
/// v7.7 ticket 199: the effects every switched-on device shows (all four when none is on),
/// in `RGB_KINDS` order: what the shared set offers, so no device goes dark.
pub fn shared_kinds(devices: &[RgbDevice; 4]) -> Vec<RgbKind> {
    let any_on = devices.iter().any(|d| d.on);
    RGB_KINDS
        .iter()
        .map(|(_, _, k)| *k)
        .filter(|k| {
            RGB_DEVICES
                .iter()
                .zip(devices)
                .filter(|(_, d)| d.on || !any_on)
                .all(|((_, _, kinds), _)| kinds.contains(k))
        })
        .collect()
}
/// v7.7 ticket 189: the Razer accessories under OpenRazer's `razeraccessory` driver: the
/// config word, the name shown, and what its `device_type` contains.
/// ponytail: matched by model name (the user's Nommo Chroma and Firefly V2); add a
/// Goliathus or Strider here when someone has one.
pub const ACCESSORIES: [(&str, &str, &str); 2] = [
    ("speakers-rgb", "Speakers RGB (Razer Nommo)", "Nommo"),
    ("mousepad-rgb", "Mousepad RGB (Razer Firefly)", "Firefly"),
];
/// v7.7 ticket 199: a colour, red, green and blue.
pub type Colour = [u8; 3];
/// v7.7 ticket 199: the G502 logo's breathe and cycle settings (logirgb 0.6.0); None: as
/// the mouse has it. The Razer devices take no timing (their breath speed is in firmware).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Timing {
    /// 1000–20000 ms.
    pub period: Option<u16>,
    /// An index into `FORMS`.
    pub form: Option<u8>,
    /// 1–100 %.
    pub intensity: Option<u8>,
}
/// logirgb's breathe waveforms.
pub const FORMS: [&str; 7] = [
    "default",
    "sine",
    "square",
    "triangle",
    "sawtooth",
    "shark-fin",
    "exponential",
];
/// v7.7 ticket 199: an RGB effect with its settings (ticket 193's research). A setting left
/// None keeps what VibeConsole did before: reactive speed 2, wave direction 1, the logo's
/// own timing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rgb {
    Off,
    Static(Colour),
    Breath(Colour, Timing),
    /// Razer only: two colours in turn.
    BreathTwo(Colour, Colour),
    /// Razer only: random colours.
    BreathRandom,
    /// Speed 1 (short) – 4 (long).
    Reactive(Colour, Option<u8>),
    /// Direction 1 or 2.
    Wave(Option<u8>),
    Spectrum(Timing),
}
impl Rgb {
    pub const SPECTRUM: Rgb = Rgb::Spectrum(Timing {
        period: None,
        form: None,
        intensity: None,
    });
}
/// v7.7 ticket 199: an effect without its settings, as picked first in the menu.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RgbKind {
    Off,
    Static,
    Breath,
    BreathTwo,
    BreathRandom,
    Reactive,
    Wave,
    Spectrum,
}
/// Each kind: its word in an effect, its menu label.
pub const RGB_KINDS: [(&str, &str, RgbKind); 8] = [
    ("off", "off", RgbKind::Off),
    ("static", "static: one colour", RgbKind::Static),
    (
        "breath",
        "breath: one colour fading in and out",
        RgbKind::Breath,
    ),
    ("breath", "breath: two colours in turn", RgbKind::BreathTwo),
    ("breath", "breath: random colours", RgbKind::BreathRandom),
    (
        "reactive",
        "reactive: lights on each key press",
        RgbKind::Reactive,
    ),
    ("wave", "wave: colours moving across", RgbKind::Wave),
    ("spectrum", "spectrum: a colour cycle", RgbKind::Spectrum),
];
pub fn rgb_kind(effect: Rgb) -> RgbKind {
    match effect {
        Rgb::Off => RgbKind::Off,
        Rgb::Static(_) => RgbKind::Static,
        Rgb::Breath(..) => RgbKind::Breath,
        Rgb::BreathTwo(..) => RgbKind::BreathTwo,
        Rgb::BreathRandom => RgbKind::BreathRandom,
        Rgb::Reactive(..) => RgbKind::Reactive,
        Rgb::Wave(_) => RgbKind::Wave,
        Rgb::Spectrum(_) => RgbKind::Spectrum,
    }
}
/// A colour as `#rrggbb`.
pub fn colour_word(c: Colour) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}
/// v7.7 ticket 199: a colour as `#rrggbb`, `r,g,b` (each 0–255) or one of `COLOURS`, or
/// why not.
/// v7.7 ticket 207: one of a colour's Red, Green or Blue, typed as a whole number 0–255; a
/// wrong one is refused with the reason.
pub fn parse_channel(name: &str, text: &str) -> Result<u8> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(
            format!("{name}: {text:?} isn't a number; type a whole number from 0 to 255").into(),
        );
    }
    text.parse::<u16>()
        .ok()
        .and_then(|n| u8::try_from(n).ok())
        .ok_or_else(|| {
            format!("{name}: {text} is over 255; type a whole number from 0 to 255").into()
        })
}
pub fn parse_colour(text: &str) -> Result<Colour> {
    let text = text.trim();
    if let Some(name) = COLOURS.iter().find(|c| **c == text) {
        return Ok(rgb_bytes(name));
    }
    if let Some(hex) = text.strip_prefix('#') {
        if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digits");
            return Ok([byte(0), byte(2), byte(4)]);
        }
        return Err(
            format!("{text} isn't a colour: #rrggbb takes six hex digits, 0–9 and a–f").into(),
        );
    }
    let parts = text.split(',').map(str::trim).collect::<Vec<_>>();
    let digits = |p: &&str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    if parts.len() == 3 && parts.iter().all(digits) {
        let mut out = [0u8; 3];
        for (slot, part) in out.iter_mut().zip(&parts) {
            *slot = part.parse().map_err(|_| {
                format!("{text} isn't a colour: each of r,g,b is 0–255, not {part}")
            })?;
        }
        return Ok(out);
    }
    Err(format!(
        "{text} isn't a colour: type #rrggbb, r,g,b (each 0–255) or one of {}",
        COLOURS.join(", ")
    )
    .into())
}
/// The effect as its config word: `off`, `static #ff0000`, `breath #0000ff period=3000`,
/// `breath #ff0000 #0000ff`, `breath random`, `reactive #ffffff speed=3`,
/// `wave direction=2`, `spectrum intensity=50`. Settings left as they were are left out.
pub fn rgb_word(effect: Rgb) -> String {
    let timing = |t: Timing| {
        let mut out = String::new();
        if let Some(p) = t.period {
            out += &format!(" period={p}");
        }
        if let Some(f) = t.form {
            out += &format!(" form={}", FORMS[usize::from(f) % FORMS.len()]);
        }
        if let Some(i) = t.intensity {
            out += &format!(" intensity={i}");
        }
        out
    };
    match effect {
        Rgb::Off => "off".into(),
        Rgb::Static(c) => format!("static {}", colour_word(c)),
        Rgb::Breath(c, t) => format!("breath {}{}", colour_word(c), timing(t)),
        Rgb::BreathTwo(a, b) => format!("breath {} {}", colour_word(a), colour_word(b)),
        Rgb::BreathRandom => "breath random".into(),
        Rgb::Reactive(c, speed) => format!(
            "reactive {}{}",
            colour_word(c),
            speed.map_or(String::new(), |s| format!(" speed={s}"))
        ),
        Rgb::Wave(d) => format!(
            "wave{}",
            d.map_or(String::new(), |d| format!(" direction={d}"))
        ),
        Rgb::Spectrum(t) => format!("spectrum{}", timing(Timing { form: None, ..t })),
    }
}
/// v7.7 ticket 210: a colour as shown: its name when its bytes equal a named colour, else
/// `custom #rrggbb`.
pub fn colour_name(c: Colour) -> String {
    match COLOURS.iter().find(|n| rgb_bytes(n) == c) {
        Some(n) => n[..1].to_uppercase() + &n[1..],
        None => format!("custom {}", colour_word(c)),
    }
}
/// v7.7 ticket 210: the effect as shown on screen: `Static · White`,
/// `Breath · Blue · 5 s · sine · 25%`, `Spectrum · 1 s`. The config keeps `rgb_word`.
pub fn rgb_name(effect: Rgb) -> String {
    let timing = |t: Timing| {
        let mut out = Vec::new();
        if let Some(p) = t.period {
            out.push(format!("{} s", f64::from(p) / 1000.0));
        }
        if let Some(f) = t.form {
            out.push(FORMS[usize::from(f) % FORMS.len()].to_owned());
        }
        if let Some(i) = t.intensity {
            out.push(format!("{i}%"));
        }
        out
    };
    let parts = match effect {
        Rgb::Off => vec!["Off".into()],
        Rgb::Static(c) => vec!["Static".into(), colour_name(c)],
        Rgb::Breath(c, t) => [vec!["Breath".into(), colour_name(c)], timing(t)].concat(),
        Rgb::BreathTwo(a, b) => vec![
            "Breath".into(),
            format!("{} and {}", colour_name(a), colour_name(b)),
        ],
        Rgb::BreathRandom => vec!["Breath".into(), "random colours".into()],
        Rgb::Reactive(c, s) => [
            vec!["Reactive".into(), colour_name(c)],
            s.map(|s| format!("speed {s}")).into_iter().collect(),
        ]
        .concat(),
        Rgb::Wave(d) => [
            vec!["Wave".into()],
            d.map(|d| format!("direction {d}")).into_iter().collect(),
        ]
        .concat(),
        Rgb::Spectrum(t) => [vec!["Spectrum".into()], timing(Timing { form: None, ..t })].concat(),
    };
    parts.join(" · ")
}
/// v7.7 ticket 199: an effect word, checked in full; also the older `static-red` words
/// (config v32–34), whose named colour becomes its RGB.
pub fn parse_rgb(word: &str) -> Result<Rgb> {
    let fail =
        |why: &str| -> Box<dyn std::error::Error> { format!("RGB effect {word:?}: {why}").into() };
    let mut parts = word.split_whitespace();
    let head = parts.next().ok_or_else(|| fail("is empty"))?;
    let alone = parts.clone().next().is_none();
    if let Some((kind, name)) = head.split_once('-').filter(|_| alone) {
        let colour = COLOURS
            .iter()
            .find(|c| **c == name)
            .map(|c| rgb_bytes(c))
            .ok_or_else(|| fail(&format!("colour must be one of {}", COLOURS.join(", "))))?;
        return match kind {
            "static" => Ok(Rgb::Static(colour)),
            "breath" => Ok(Rgb::Breath(colour, Timing::default())),
            "reactive" => Ok(Rgb::Reactive(colour, None)),
            _ => Err(fail(
                "must be off, static, breath, reactive, wave or spectrum",
            )),
        };
    }
    let (mut colours, mut settings) = (Vec::new(), Vec::new());
    let mut random = false;
    for part in parts {
        match part.split_once('=') {
            Some(pair) => settings.push(pair),
            None if part == "random" => random = true,
            None => colours.push(parse_colour(part).map_err(|e| fail(&e.to_string()))?),
        }
    }
    let number = |key: &str, value: &str, low: u16, high: u16| -> Result<u16> {
        value
            .parse::<u16>()
            .ok()
            .filter(|n| (low..=high).contains(n))
            .ok_or_else(|| fail(&format!("{key} must be {low}–{high}, not {value}")))
    };
    let allowed: &[&str] = match head {
        "breath" => &["period", "form", "intensity"],
        "spectrum" => &["period", "intensity"],
        "reactive" => &["speed"],
        "wave" => &["direction"],
        _ => &[],
    };
    let mut timing = Timing::default();
    let (mut speed, mut direction) = (None, None);
    for (key, value) in settings {
        if !allowed.contains(&key) {
            return Err(fail(&format!("{head} has no {key} setting")));
        }
        match key {
            "period" => timing.period = Some(number(key, value, 1000, 20000)?),
            "intensity" => timing.intensity = Some(number(key, value, 1, 100)? as u8),
            "form" => {
                let f = FORMS
                    .iter()
                    .position(|f| *f == value)
                    .ok_or_else(|| fail(&format!("form must be one of {}", FORMS.join(", "))))?;
                timing.form = Some(f as u8);
            }
            "speed" => speed = Some(number(key, value, 1, 4)? as u8),
            _ => direction = Some(number(key, value, 1, 2)? as u8),
        }
    }
    Ok(match (head, colours.as_slice(), random) {
        ("off", [], false) => Rgb::Off,
        ("static", [c], false) => Rgb::Static(*c),
        ("breath", [c], false) => Rgb::Breath(*c, timing),
        ("breath", [a, b], false) => Rgb::BreathTwo(*a, *b),
        ("breath", [], true) => Rgb::BreathRandom,
        ("reactive", [c], false) => Rgb::Reactive(*c, speed),
        ("wave", [], false) => Rgb::Wave(direction),
        ("spectrum", [], false) => Rgb::Spectrum(timing),
        ("off" | "static" | "breath" | "reactive" | "wave" | "spectrum", ..) => {
            return Err(fail("wrong colours for this effect"));
        }
        _ => {
            return Err(fail(
                "must be off, static, breath, reactive, wave or spectrum",
            ));
        }
    })
}
/// A colour name's red, green and blue: the named shortcuts in `COLOURS`, saved as their
/// RGB (ticket 199).
pub fn rgb_bytes(colour: &str) -> [u8; 3] {
    match colour {
        "blue" => [0, 0, 255],
        "orange" => [255, 128, 0],
        "red" => [255, 0, 0],
        "green" => [0, 255, 0],
        "purple" => [128, 0, 255],
        _ => [255, 255, 255],
    }
}
/// v7 ticket 128: the RGB effect for these source states: the most urgent busy state's.
/// None when nothing is busy (the resting effect then shows).
pub fn rgb_for(states: &[&str], set: &RgbSet) -> Option<Rgb> {
    Some(set.states[busy_index(states)?])
}
/// The most urgent busy state among `states`, as its place in `BUSY`; None when all idle.
pub fn busy_index(states: &[&str]) -> Option<usize> {
    BUSY.iter().position(|s| states.contains(s))
}
/// The keyboard lights a pattern can use (v7 ticket 128): lock-key LEDs, then a Razer
/// keyboard's M (macro) light through OpenRazer's driver.
pub const KEY_LEDS: [&str; 4] = ["scroll", "caps", "num", "macro"];
impl Default for DevicePatterns {
    /// v7.7 ticket 195: the shipped `lights.json` (`core/defaults/`), not code.
    fn default() -> Self {
        crate::config::shipped_lights().devices.clone()
    }
}
/// xone's Xbox button light brightness at start-up, and its maximum (`GIP_LED_BRIGHTNESS_MAX`).
pub const XBOX_BRIGHTNESS: u8 = 20;
pub const XBOX_MAX_BRIGHTNESS: u8 = 50;
/// Why a keyboard row can't take this pattern word (v7 ticket 133).
pub fn keyboard_light_error(word: &str) -> String {
    let words = KEYBOARD_LIGHTS.iter().map(|(w, _)| *w).collect::<Vec<_>>();
    let (last, rest) = words.split_last().expect("keyboard patterns");
    format!(
        "keyboard light must be {} or {last}, not {word} (a lock LED is only on or off)",
        rest.join(", ")
    )
}
impl DevicePatterns {
    pub fn validate(&self) -> Result<()> {
        if let Some(l) = self
            .keyboard
            .iter()
            .find(|l| !KEYBOARD_LIGHTS.iter().any(|(_, k)| k == *l))
        {
            return Err(keyboard_light_error(word(&LIGHTS, *l)).into());
        }
        if let Some(b) = self
            .xbox_brightness
            .iter()
            .find(|b| **b > XBOX_MAX_BRIGHTNESS)
        {
            return Err(
                format!("Xbox light brightness must be 0–{XBOX_MAX_BRIGHTNESS}, not {b}").into(),
            );
        }
        match self.ps5.iter().find(|(c, ..)| !COLOURS.contains(c)) {
            Some((c, ..)) => Err(format!(
                "light-bar colour must be one of {}, not {c}",
                COLOURS.join(", ")
            )
            .into()),
            None => Ok(()),
        }
    }
}
/// v7 ticket 113: the keyboard LED's pattern for these source states: the most urgent busy
/// state's, as on the MPK and the Xbox button. None when nothing is busy (the LED then
/// shows the real lock state).
pub fn keyboard_light(states: &[&str], patterns: &DevicePatterns) -> Option<Light> {
    let busy = BUSY.iter().position(|s| states.contains(s))?;
    Some(patterns.keyboard[busy])
}
/// Whether a one-LED pattern is lit `ms` milliseconds in: blink once a second, fast blink
/// four times a second, slow pulse once every two seconds.
pub fn light_on(light: Light, ms: u64) -> bool {
    match light {
        Light::Off => false,
        Light::Steady => true,
        Light::Blink => ms % 1000 < 500,
        Light::FastBlink => ms % 250 < 125,
        Light::SlowPulse => ms % 2000 < 1000,
        // Not in `KEYBOARD_LIGHTS`, so a keyboard never gets them; the nearest on/off.
        Light::SlowBlink | Light::TempoSlow => ms % 2000 < 1000,
        Light::FastPulse | Light::TempoFast => ms % 500 < 250,
    }
}
/// v7 ticket 134: the shortest on or off time for a light VibeConsole switches itself, so it
/// sends at most 8 changes a second whatever the tempo.
pub const TEMPO_MIN_MS: u64 = 125;
/// v7 ticket 134: whether a light blinking at `bpm` is lit `ms` milliseconds in: on for the
/// first half of each beat, as the MPK's tempo light. Beats shorter than 250 ms are capped.
pub fn tempo_on(bpm: u16, ms: u64) -> bool {
    let beat = (60_000 / u64::from(bpm.max(1))).max(2 * TEMPO_MIN_MS);
    ms % beat < beat / 2
}
/// v7 ticket 134: a tempo pattern as the steady or off it shows at `ms`; others unchanged.
pub fn at_tempo(light: Light, ms: u64, patterns: &Patterns) -> Light {
    let bpm = match light {
        Light::TempoSlow => patterns.slow,
        Light::TempoFast => patterns.fast,
        other => return other,
    };
    if tempo_on(bpm, ms) {
        Light::Steady
    } else {
        Light::Off
    }
}
/// The Xbox button light for these source states (v7 ticket 86): the pattern of the most
/// urgent busy state, in `BUSY` order as on the MPK; "off" turns it off. None when no source
/// is busy: the plain running light shows.
/// With its brightness (v7 ticket 132).
pub fn xbox_light(states: &[&str], patterns: &DevicePatterns) -> Option<(Light, u8)> {
    let busy = BUSY.iter().position(|s| states.contains(s))?;
    Some((patterns.xbox[busy].0, patterns.xbox_brightness[busy]))
}
/// The rumble due now: the longest of the states that sources newly entered.
pub fn rumble_for(
    before: &[(String, &str)],
    now: &[(String, &str)],
    patterns: &DevicePatterns,
) -> Rumble {
    now.iter()
        .filter(|(name, state)| {
            before
                .iter()
                .find(|(n, _)| n == name)
                .is_none_or(|(_, was)| was != state)
        })
        .filter_map(|(_, state)| BUSY.iter().position(|s| s == state))
        .map(|i| patterns.xbox[i].1)
        .max()
        .unwrap_or(Rumble::Off)
}

/// Ticket 17: what each busy state shows, in `BUSY` order, and the two tempos.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Patterns {
    pub slow: u16,
    pub fast: u16,
    pub states: [(Tempo, Octave); 4],
}
impl Default for Patterns {
    /// v7.7 ticket 195: the shipped `lights.json` (`core/defaults/`), not code.
    fn default() -> Self {
        crate::config::shipped_lights().patterns
    }
}
impl Patterns {
    pub fn validate(self) -> Result<Self> {
        if !(1..=300).contains(&self.slow) || !(1..=300).contains(&self.fast) {
            return Err("status light tempos must be 1–300 BPM".into());
        }
        Ok(self)
    }
}

/// A source's octave light (an agent or a folder): ±1..±4.
pub fn valid_octave(octave: i8) -> Result<i8> {
    if octave == 0 || !(-4..=4).contains(&octave) {
        return Err("status light octave must be ±1..±4".into());
    }
    Ok(octave)
}

/// Busy sources (state, octave) to tempo/arpeggiator/octave settings; None when every source
/// is idle, so the program's own feedback shows. `millis` drives the blinking.
/// - Tempo: the most urgent busy state whose pattern has a tempo sets it; none: arpeggiator off.
/// - Octave: alternate > steady > blink. Sources showing the winning pattern take turns once a
///   second; blink with one source flashes its octave against 0; alternate flips the shown
///   octave's sign twice a second.
// ponytail: one-second turns and the half-second flip are not yet checked on hardware
// (archive/tests/v5/07 gate unrecorded); retune from its measured limit.
pub fn lights(sources: &[(&str, i8)], patterns: &Patterns, millis: u128) -> Option<Settings> {
    let busy = sources
        .iter()
        .filter_map(|(state, octave)| {
            let index = BUSY.iter().position(|s| s == state)?;
            Some((patterns.states[index], *octave))
        })
        .collect::<Vec<_>>();
    if busy.is_empty() {
        return None;
    }
    let tempo = BUSY
        .iter()
        .enumerate()
        .filter(|(i, s)| {
            sources.iter().any(|(state, _)| state == *s) && patterns.states[*i].0 != Tempo::Off
        })
        .map(|(i, _)| match patterns.states[i].0 {
            Tempo::Fast => patterns.fast,
            _ => patterns.slow,
        })
        .next();
    let rank = |octave: Octave| match octave {
        Octave::Off => 0,
        Octave::Blink => 1,
        Octave::Steady => 2,
        Octave::Alternate => 3,
    };
    let best = busy.iter().map(|((_, o), _)| *o).max_by_key(|o| rank(*o));
    let voices = busy
        .iter()
        .filter(|((_, o), _)| Some(*o) == best)
        .map(|(_, octave)| *octave)
        .collect::<Vec<_>>();
    let turn = voices
        .get((millis / 1000) as usize % voices.len().max(1))
        .copied();
    let octave = match (best, turn) {
        (Some(Octave::Alternate), Some(o)) if (millis / 500) % 2 == 1 => -o,
        (Some(Octave::Blink), Some(_)) if voices.len() == 1 && (millis / 1000) % 2 == 1 => 0,
        (Some(Octave::Off), _) | (_, None) => 0,
        (_, Some(o)) => o,
    };
    Some(Settings {
        octave: Some(octave),
        tempo,
        arp: Some(tempo.is_some()),
    })
}

/// (event, matcher, state) per agent tool; both the shown hooks and the installer use them.
type Events = [(&'static str, Option<&'static str>, &'static str)];
const CLAUDE: &Events = &[
    ("UserPromptSubmit", None, "thinking"),
    // Ticket 17: a multiple-choice question is a decision; a tool approval reports at once
    // (Notification's permission_prompt waits about six seconds); any finished tool means
    // the agent works again.
    ("PreToolUse", Some("AskUserQuestion"), "needs-decision"),
    ("PermissionRequest", None, "needs-input"),
    ("PostToolUse", None, "thinking"),
    (
        "Notification",
        Some("elicitation_dialog|elicitation_url_dialog"),
        "needs-input",
    ),
    ("Stop", None, "finished"),
    ("SessionEnd", None, "ended"),
];
const CODEX: &Events = &[
    ("UserPromptSubmit", None, "thinking"),
    ("PermissionRequest", None, "needs-input"),
    ("PostToolUse", None, "thinking"),
    ("Stop", None, "finished"),
    ("SessionEnd", None, "ended"),
];

pub fn claude_hooks() -> String {
    hooks(CLAUDE)
}

pub fn codex_hooks() -> String {
    hooks(CODEX)
}

fn hook_command(state: &str) -> String {
    format!("vibeconsole agent-status - {state}")
}

/// Ticket 15: the user settings file each tool reads hooks from.
pub fn hooks_file(codex: bool) -> Result<PathBuf> {
    let (variable, default, file) = if codex {
        ("CODEX_HOME", ".codex", "hooks.json")
    } else {
        ("CLAUDE_CONFIG_DIR", ".claude", "settings.json")
    };
    let dir = match std::env::var_os(variable).filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?).join(default),
    };
    Ok(dir.join(file))
}

/// Events whose VibeConsole hook `settings` lacks. Refuses anything but a JSON object.
fn missing<'a>(settings: &serde_json::Value, events: &'a Events) -> Result<Vec<&'a str>> {
    let hooks = settings
        .as_object()
        .ok_or("not a JSON object")?
        .get("hooks");
    Ok(events
        .iter()
        .filter(|(event, _, state)| {
            !hooks
                .and_then(|h| h.get(*event))
                .is_some_and(|groups| groups.to_string().contains(&hook_command(state)))
        })
        .map(|(event, _, _)| *event)
        .collect())
}

/// The file's text with VibeConsole's missing hooks added, keeping everything else and its
/// order; None when nothing is missing. `text` None means the file does not exist yet.
fn merge(text: Option<&str>, events: &Events) -> Result<Option<String>> {
    let mut settings: serde_json::Value = match text {
        Some(text) => serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?,
        None => serde_json::json!({}),
    };
    let missing = missing(&settings, events)?;
    if missing.is_empty() {
        return Ok(None);
    }
    let hooks = settings
        .as_object_mut()
        .ok_or("not a JSON object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("\"hooks\" is not a JSON object")?;
    for (event, matcher, state) in events.iter().filter(|(e, _, _)| missing.contains(e)) {
        let mut group = serde_json::json!({
            "hooks": [{"type": "command", "command": hook_command(state)}]
        });
        if let Some(matcher) = matcher {
            group["matcher"] = (*matcher).into();
        }
        hooks
            .entry(*event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or(format!("\"hooks.{event}\" is not a list"))?
            .push(group);
    }
    Ok(Some(serde_json::to_string_pretty(&settings)? + "\n"))
}

/// doctor's line for one tool.
pub fn hooks_status(codex: bool) -> String {
    let tool = if codex { "Codex" } else { "Claude Code" };
    match hooks_file(codex).and_then(|path| status_in(&path, if codex { CODEX } else { CLAUDE })) {
        Ok(state) => format!("{tool} status hooks: {state}"),
        Err(error) => format!("{tool} status hooks: unreadable: {error}"),
    }
}

fn status_in(path: &Path, events: &Events) -> Result<String> {
    {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(format!(
                    "missing (no {}); Agents can install them",
                    path.display()
                ));
            }
            Err(e) => return Err(e.into()),
        };
        let settings = serde_json::from_str(&text).map_err(|e| format!("not valid JSON: {e}"))?;
        let missing = missing(&settings, events)?;
        Ok(if missing.is_empty() {
            format!("installed ({})", path.display())
        } else {
            format!(
                "missing {} in {}; Agents can install them",
                missing.join(", "),
                path.display()
            )
        })
    }
}

/// Merges VibeConsole's hooks into the tool's settings: a timestamped backup first, then an
/// atomic replace that keeps the file's permissions. Invalid files are refused unchanged.
/// v7.7 ticket 162: an agent's context as Claude Code's status line reports it: the
/// percent of the window used, the window's size, the transcript. Absent fields stay `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    pub used: Option<u8>,
    pub size: Option<u64>,
    pub transcript: Option<PathBuf>,
}
/// `<dir>/<agent>.context`, beside the agent's state file.
pub fn context_file(dir: &Path, agent: &str) -> PathBuf {
    dir.join(format!("{agent}.context"))
}
/// v7.7 ticket 162: `vibeconsole statusline`: Claude Code's status line JSON (`input`) to
/// the agent's context file (atomic). Only for a valid agent name; nothing is guessed.
pub fn write_context(input: &str, agent: &str, dir: &Path) -> Result<()> {
    if !crate::mappings::simple_name(agent) {
        return Err("not a VibeConsole agent".into());
    }
    let json: serde_json::Value = serde_json::from_str(input)?;
    let window = &json["context_window"];
    let used = window["used_percentage"]
        .as_f64()
        .map(|p| p.clamp(0.0, 100.0).round() as u8);
    let size = window["context_window_size"].as_u64();
    let transcript = json["transcript_path"]
        .as_str()
        .filter(|p| p.starts_with('/'));
    let text = format!(
        "{}\t{}\t{}\n",
        used.map_or("-".into(), |u| u.to_string()),
        size.map_or("-".into(), |s| s.to_string()),
        transcript.unwrap_or("-")
    );
    std::fs::create_dir_all(dir)?;
    let temporary = dir.join(format!(".{agent}.context.tmp"));
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, context_file(dir, agent))?;
    Ok(())
}
/// The context file of `agent`, or `None` when it reports nothing.
pub fn read_context(dir: &Path, agent: &str) -> Option<Context> {
    let text = std::fs::read_to_string(context_file(dir, agent)).ok()?;
    let mut fields = text.trim_end().split('\t');
    let mut next = || fields.next().filter(|f| *f != "-");
    Some(Context {
        used: next().and_then(|f| f.parse().ok()),
        size: next().and_then(|f| f.parse().ok()),
        transcript: next().map(PathBuf::from),
    })
}
/// v7.7 ticket 162: the session's tokens so far, summed from the transcript's per-message
/// usage (input, output and cache writes; cache reads are the same tokens read again, so
/// they aren't counted). `cache` holds (bytes read, sum): only new lines are read.
pub fn transcript_tokens(path: &Path, cache: &mut (u64, u64)) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    if file.metadata()?.len() < cache.0 {
        *cache = (0, 0); // a new transcript in the same place
    }
    file.seek(SeekFrom::Start(cache.0))?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let complete = text.rfind('\n').map_or(0, |at| at + 1);
    for line in text[..complete].lines() {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let usage = &json["message"]["usage"];
        cache.1 += [
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
        ]
        .iter()
        .filter_map(|k| usage[*k].as_u64())
        .sum::<u64>();
    }
    cache.0 += complete as u64;
    Ok(cache.1)
}
/// The command VibeConsole puts in Claude Code's `statusLine`.
pub const STATUSLINE: &str = "vibeconsole statusline";
/// v7.7 ticket 162: point Claude Code's `statusLine` at `vibeconsole statusline`, after a
/// backup. A status line command the user already had is kept in `saved` and the wrapper
/// runs it and prints its output, so their status line stays as it was.
pub fn install_statusline(settings: &Path, saved: &Path) -> Result<String> {
    let text = match std::fs::read_to_string(settings) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let mut json: serde_json::Value = match &text {
        Some(text) => serde_json::from_str(text)
            .map_err(|e| format!("{} not changed: not valid JSON: {e}", settings.display()))?,
        None => serde_json::json!({}),
    };
    let current = json["statusLine"]["command"].as_str().map(str::to_owned);
    if current.as_deref() == Some(STATUSLINE) {
        return Ok("Status line: already installed".into());
    }
    let object = json
        .as_object_mut()
        .ok_or_else(|| format!("{} not changed: not a JSON object", settings.display()))?;
    if let Some(theirs) = &current {
        if let Some(folder) = saved.parent() {
            std::fs::create_dir_all(folder)?;
        }
        std::fs::write(saved, theirs)?;
    }
    let mut line = object
        .get("statusLine")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    line.insert("type".into(), "command".into());
    line.insert("command".into(), STATUSLINE.into());
    object.insert("statusLine".into(), serde_json::Value::Object(line));
    let mut note = "Status line: installed".to_string();
    if text.is_some() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let backup = PathBuf::from(format!("{}.bak-vibeconsole-{stamp}", settings.display()));
        std::fs::copy(settings, &backup)?;
        note.push_str(&format!("; backup {}", backup.display()));
    }
    if current.is_some() {
        note.push_str("; your own status line still shows (VibeConsole runs it)");
    }
    if let Some(folder) = settings.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let temporary = settings.with_extension("json.vibeconsole-tmp");
    std::fs::write(&temporary, serde_json::to_string_pretty(&json)? + "\n")?;
    std::fs::rename(&temporary, settings)?;
    Ok(note)
}

pub fn install_hooks(codex: bool) -> Result<String> {
    let note = install_in(&hooks_file(codex)?, if codex { CODEX } else { CLAUDE })?;
    Ok(if codex && note.starts_with("Installed") {
        note.replace(
            ". Restart",
            ". Codex runs new hooks only after you trust them in Codex. Restart",
        )
    } else {
        note
    })
}

fn install_in(path: &Path, events: &Events) -> Result<String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let Some(merged) = merge(text.as_deref(), events)
        .map_err(|e| format!("{} not changed: {e}", path.display()))?
    else {
        return Ok(format!("Already installed in {}", path.display()));
    };
    let mut note = format!("Installed in {}", path.display());
    if text.is_some() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let backup = PathBuf::from(format!("{}.bak-vibeconsole-{stamp}", path.display()));
        std::fs::copy(path, &backup)?;
        note.push_str(&format!("; backup {}", backup.display()));
    } else if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = PathBuf::from(format!("{}.vibeconsole-tmp", path.display()));
    std::fs::write(&temporary, merged)?;
    if text.is_some() {
        std::fs::set_permissions(&temporary, std::fs::metadata(path)?.permissions())?;
    }
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })?;
    Ok(note + ". Restart running agents to load them.")
}

fn hooks(events: &[(&str, Option<&str>, &str)]) -> String {
    let groups = events
        .iter()
        .map(|(event, matcher, state)| {
            let matcher = matcher.map_or(String::new(), |m| format!("\"matcher\": \"{m}\", "));
            format!(
                "    \"{event}\": [{{{matcher}\"hooks\": [{{\"type\": \"command\", \"command\": \"{}\"}}]}}]",
                hook_command(state)
            )
        })
        .collect::<Vec<_>>();
    format!("{{\n  \"hooks\": {{\n{}\n  }}\n}}\n", groups.join(",\n"))
}

#[cfg(test)]
mod tests {
    /// v7.7 ticket 212: in-step frames: breath fades from dark to its colour at half the
    /// period, scaled by its intensity, by its form; spectrum turns the hue once a period;
    /// static and the firmware-only effects aren't stepped; the G502 gets the same period.
    #[test]
    fn in_step_frames_follow_one_clock() {
        use super::*;
        let frame = |w: &str, ms| rgb_frame(parse_rgb(w).unwrap(), ms);
        let blue = |b| Some(Rgb::Static([0, 0, b]));
        let w = "breath #0000ff period=5000 form=sine intensity=25";
        assert_eq!(frame(w, 0), blue(0));
        assert_eq!(frame(w, 2500), blue(64), "the peak at 25%");
        assert_eq!(frame(w, 5000), blue(0), "one cycle");
        assert_eq!(
            frame("breath #0000ff", 2000),
            blue(255),
            "default period 4000"
        );
        assert_eq!(
            frame("breath #0000ff period=1000 form=square", 400),
            blue(255)
        );
        assert_eq!(
            frame("breath #0000ff period=1000 form=square", 600),
            blue(0)
        );
        assert_eq!(
            frame("breath #0000ff period=1000 form=triangle", 250),
            blue(128)
        );
        assert_eq!(
            frame("spectrum period=6000", 0),
            Some(Rgb::Static([255, 0, 0]))
        );
        assert_eq!(
            frame("spectrum period=6000", 2000),
            Some(Rgb::Static([0, 255, 0]))
        );
        assert_eq!(
            frame("spectrum period=6000", 4000),
            Some(Rgb::Static([0, 0, 255]))
        );
        for w in [
            "static #ff0000",
            "off",
            "breath random",
            "wave",
            "reactive #ffffff",
        ] {
            assert_eq!(frame(w, 1234), None, "{w}");
        }
        let mouse = |w: &str| rgb_word(in_step_mouse(parse_rgb(w).unwrap()));
        assert_eq!(mouse("breath #0000ff"), "breath #0000ff period=4000");
        assert_eq!(
            mouse("breath #0000ff period=5000"),
            "breath #0000ff period=5000"
        );
        assert_eq!(mouse("spectrum"), "spectrum period=4000");
        assert_eq!(mouse("static #ff0000"), "static #ff0000");
    }
    /// v7.7 ticket 210: effects show words; only a colour off the named list shows its code.
    #[test]
    fn effects_show_their_names() {
        use super::*;
        let name = |w: &str| rgb_name(parse_rgb(w).unwrap());
        assert_eq!(name("static #ffffff"), "Static · White");
        assert_eq!(name("static #12ab34"), "Static · custom #12ab34");
        assert_eq!(
            name("breath #0000ff period=5000 form=sine intensity=25"),
            "Breath · Blue · 5 s · sine · 25%"
        );
        assert_eq!(name("spectrum period=1000"), "Spectrum · 1 s");
        assert_eq!(name("breath #ff0000 #00ff00"), "Breath · Red and Green");
        assert_eq!(name("breath random"), "Breath · random colours");
        assert_eq!(
            name("reactive #ff8000 speed=3"),
            "Reactive · Orange · speed 3"
        );
        assert_eq!(name("wave direction=2"), "Wave · direction 2");
        assert_eq!(name("off"), "Off");
        assert_eq!(
            name("breath #8000ff period=1500"),
            "Breath · Purple · 1.5 s"
        );
    }
    /// v7.7 ticket 162: the status line JSON becomes the agent's context file; tokens are
    /// summed from the transcript and only new lines are read; installing wraps an existing
    /// `statusLine` command and keeps it, once.
    #[test]
    fn statusline_context_tokens_and_install_keep_theirs() {
        use super::*;
        let dir = std::env::temp_dir().join(format!("vibeconsole-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("t.jsonl");
        let input = format!(
            r#"{{"session_id":"s","transcript_path":"{}","context_window":{{"context_window_size":200000,"used_percentage":61.6,"total_input_tokens":1}}}}"#,
            transcript.display()
        );
        write_context(&input, "claude", &dir).unwrap();
        assert_eq!(
            read_context(&dir, "claude"),
            Some(Context {
                used: Some(62),
                size: Some(200000),
                transcript: Some(transcript.clone())
            })
        );
        write_context(
            r#"{"context_window":{"used_percentage":null}}"#,
            "early",
            &dir,
        )
        .unwrap();
        assert_eq!(
            read_context(&dir, "early"),
            Some(Context::default()),
            "nothing guessed"
        );
        assert!(write_context(&input, "../x", &dir).is_err());
        assert_eq!(read_context(&dir, "codex"), None);

        let usage = |i: u64, o: u64, c: u64| {
            format!(
                r#"{{"message":{{"usage":{{"input_tokens":{i},"output_tokens":{o},"cache_creation_input_tokens":{c},"cache_read_input_tokens":999}}}}}}"#
            )
        };
        let lines = format!(
            "{}\n{{\"type\":\"user\"}}\n{}\n",
            usage(10, 20, 30),
            usage(1, 2, 3)
        );
        std::fs::write(&transcript, lines).unwrap();
        let mut cache = (0, 0);
        assert_eq!(transcript_tokens(&transcript, &mut cache).unwrap(), 66);
        let read = cache.0;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        let more = format!("{}\n{{\"partial", usage(100, 0, 0));
        std::io::Write::write_all(&mut file, more.as_bytes()).unwrap();
        assert_eq!(transcript_tokens(&transcript, &mut cache).unwrap(), 166);
        assert!(cache.0 > read, "only the new line was read");

        let settings = dir.join(".claude/settings.json");
        let saved = dir.join("statusline-command");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            r#"{"model":"opus","statusLine":{"type":"command","command":"~/my-line.sh","padding":2}}"#,
        )
        .unwrap();
        let note = install_statusline(&settings, &saved).unwrap();
        assert!(
            note.contains("backup") && note.contains("your own status line"),
            "{note}"
        );
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(json["statusLine"]["command"], STATUSLINE);
        assert_eq!(json["statusLine"]["padding"], 2);
        assert_eq!(json["model"], "opus");
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "~/my-line.sh");
        assert!(
            install_statusline(&settings, &saved)
                .unwrap()
                .contains("already installed")
        );
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "~/my-line.sh");
        std::fs::remove_dir_all(&dir).unwrap();
    }
    /// v7 ticket 86: the Xbox button light shows the most urgent busy state whose pattern
    /// lights it; rumble fires once for a source newly in a state that rumbles, the longest
    /// winning; the defaults are the board's (version 34).
    #[test]
    fn xbox_light_and_rumble_follow_each_states_pattern() {
        let p = DevicePatterns::default();
        assert_eq!(p.on, [true, true, true]);
        assert_eq!(
            p.xbox,
            [
                (Light::FastBlink, Rumble::Short), // needs a decision
                (Light::FastBlink, Rumble::Off),   // needs input
                (Light::SlowPulse, Rumble::Off),   // thinking
                (Light::Blink, Rumble::Short),     // finished
            ]
        );
        assert_eq!(p.ps5[0], ("red", Light::FastBlink, Rumble::Short));
        assert_eq!(
            xbox_light(&["idle"], &p),
            None,
            "nothing busy: the running light"
        );
        assert_eq!(
            xbox_light(&["thinking", "idle"], &p),
            Some((Light::SlowPulse, 20))
        );
        assert_eq!(
            xbox_light(&["finished", "thinking"], &p),
            Some((Light::SlowPulse, 20)),
            "BUSY order, as the MPK"
        );
        let mut bright = p.clone();
        bright.xbox_brightness = [50, 35, 5, 20]; // v7 ticket 132: each state's own brightness
        assert_eq!(
            xbox_light(&["finished", "needs-decision"], &bright),
            Some((Light::FastBlink, 50))
        );
        assert_eq!(
            xbox_light(&["thinking"], &bright),
            Some((Light::SlowPulse, 5))
        );
        let mut quiet = p.clone();
        quiet.xbox[0].0 = Light::Off;
        assert_eq!(
            xbox_light(&["needs-decision", "thinking"], &quiet),
            Some((Light::Off, 20)),
            "a state set to off turns the light off"
        );
        let src = |s: &'static str| ("claude".to_string(), s);
        let codex = |s: &'static str| ("codex".to_string(), s);
        assert_eq!(
            rumble_for(&[src("thinking")], &[src("finished")], &p),
            Rumble::Short
        );
        assert_eq!(
            rumble_for(&[src("finished")], &[src("finished")], &p),
            Rumble::Off,
            "once"
        );
        assert_eq!(
            rumble_for(&[src("thinking")], &[src("needs-input")], &p),
            Rumble::Off
        );
        assert_eq!(rumble_for(&[], &[src("idle")], &p), Rumble::Off);
        // One source staying put while another changes: only the change counts.
        let before = [src("finished"), codex("thinking")];
        assert_eq!(
            rumble_for(&before, &[src("finished"), codex("needs-input")], &p),
            Rumble::Off
        );
        assert_eq!(
            rumble_for(&before, &[src("finished"), codex("needs-decision")], &p),
            Rumble::Short
        );
        // Going busy again rumbles only if that state's pattern says so; the longest wins.
        let mut busy = p.clone();
        busy.xbox[2].1 = Rumble::Short;
        assert_eq!(
            rumble_for(&[src("finished")], &[src("thinking")], &p),
            Rumble::Off
        );
        assert_eq!(
            rumble_for(&[src("finished")], &[src("thinking")], &busy),
            Rumble::Short
        );
        let mut long = p.clone();
        long.xbox[3].1 = Rumble::Long;
        let both = [src("finished"), codex("needs-decision")];
        assert_eq!(
            rumble_for(&[], &both, &long),
            Rumble::Long,
            "the longest wins"
        );
    }

    use super::*;

    #[test]
    fn state_files_validate_and_finished_clears_only_for_the_attended_window() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-status-{}", std::process::id()));
        assert_eq!(read_in(&dir, "claude"), "idle"); // nothing written yet
        write_in(&dir, "claude", "thinking").unwrap();
        assert_eq!(read_in(&dir, "claude"), "thinking");
        write_in(&dir, "claude", "finished").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("agent-claude")).unwrap(),
            "finished\n"
        );
        for (agent, state) in [("claude", "busy"), ("../x", "idle"), ("", "idle")] {
            assert!(write_in(&dir, agent, state).is_err());
        }
        assert_eq!(read_in(&dir, "claude"), "finished"); // rejected writes changed nothing
        std::fs::write(dir.join("agent-codex"), "garbage").unwrap();
        assert_eq!(read_in(&dir, "codex"), "idle");
        std::fs::remove_dir_all(&dir).unwrap();

        let agents = vec![
            ("claude".to_string(), "claude".to_string(), "finished"),
            ("codex".to_string(), "codex".to_string(), "finished"),
            ("yolo".to_string(), "yolo".to_string(), "thinking"),
        ];
        // Fake tmux answers.
        assert_eq!(attended(&agents, Some((true, "claude".into()))), ["claude"]);
        assert!(attended(&agents, Some((false, "claude".into()))).is_empty()); // detached
        assert!(attended(&agents, Some((true, "yolo".into()))).is_empty()); // not finished
        assert!(attended(&agents, None).is_empty()); // no tmux / no session
        // Stuck states: only busy agents whose window is known to be gone are cleared.
        let alive = |window: &str| match window {
            "claude" => Some(false), // window closed or tmux server killed
            "codex" => Some(true),
            _ => None, // tmux could not be asked
        };
        assert_eq!(vanished(&agents, alive), ["claude"]);
        // Hooks report only from a live pane in the agent's own vibe window.
        assert!(own_window(Some("vibe\tcodex"), "codex"));
        assert!(!own_window(Some("vibe\tclaude"), "codex")); // another agent's window
        assert!(!own_window(Some("work\tcodex"), "codex")); // another tmux session
        assert!(!own_window(None, "codex")); // pane or tmux server gone (leaked daemon)
        let idle = vec![("claude".to_string(), "claude".to_string(), "idle")];
        assert!(vanished(&idle, alive).is_empty());
        assert!(command(&["claude".into()]).is_err());
        assert!(command(&["a".into(), "b".into(), "c".into(), "d".into()]).is_err());
        let claude = claude_hooks();
        assert!(claude.contains("\"matcher\": \"AskUserQuestion\""));
        assert!(claude.contains("\"PermissionRequest\""));
        assert!(claude.contains("agent-status - finished"));
        assert!(codex_hooks().contains("\"PermissionRequest\""));
    }

    /// Ticket 15: install merges only VibeConsole's missing hooks, keeps everything else and its
    /// order, backs up first, refuses invalid files unchanged; doctor names what is missing.
    #[test]
    fn hooks_install_merges_backs_up_and_refuses_invalid_files() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-hooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        assert!(
            status_in(&path, CLAUDE)
                .unwrap()
                .starts_with("missing (no ")
        );
        // A missing file is created with all three hooks.
        assert!(install_in(&path, CLAUDE).unwrap().starts_with("Installed"));
        assert!(status_in(&path, CLAUDE).unwrap().starts_with("installed"));
        let created: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            created["hooks"]["Notification"][0]["matcher"],
            "elicitation_dialog|elicitation_url_dialog"
        );
        assert_eq!(
            created["hooks"]["PreToolUse"][0]["matcher"],
            "AskUserQuestion"
        );
        // An existing file keeps its keys, order, other hooks and permissions.
        let existing = r#"{"theme": "dark", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "notify-send done"}]}]}, "tui": "fullscreen"}"#;
        std::fs::write(&path, existing).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            status_in(&path, CLAUDE).unwrap(),
            format!(
                "missing UserPromptSubmit, PreToolUse, PermissionRequest, PostToolUse, Notification, Stop, SessionEnd in {}; Agents can install them",
                path.display()
            )
        );
        let note = install_in(&path, CLAUDE).unwrap();
        assert!(note.contains("backup "), "{note}");
        let text = std::fs::read_to_string(&path).unwrap();
        let merged: serde_json::Value = serde_json::from_str(&text).unwrap();
        let keys = merged.as_object().unwrap().keys().collect::<Vec<_>>();
        assert_eq!(keys, ["theme", "hooks", "tui"]);
        assert_eq!(
            merged["hooks"]["Stop"][0]["hooks"][0]["command"],
            "notify-send done"
        );
        assert_eq!(
            merged["hooks"]["Stop"][1]["hooks"][0]["command"],
            "vibeconsole agent-status - finished"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let backups = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".bak-vibeconsole-")
            })
            .count();
        assert_eq!(backups, 1);
        // A second install changes nothing.
        assert!(
            install_in(&path, CLAUDE)
                .unwrap()
                .starts_with("Already installed")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        // Invalid JSON, a non-object, or a malformed hooks entry is refused unchanged.
        for bad in [
            "{not json",
            "[1, 2]",
            r#"{"hooks": []}"#,
            r#"{"hooks": {"Stop": 3}}"#,
        ] {
            std::fs::write(&path, bad).unwrap();
            assert!(install_in(&path, CODEX).is_err(), "{bad}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bad);
        }
        std::fs::write(&path, "{not json").unwrap();
        assert!(status_in(&path, CODEX).is_err()); // doctor: unreadable
        assert!(codex_hooks().contains("vibeconsole agent-status - needs-input"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// v7 ticket 16: Orca's status file lights folders our hooks don't report; only the
    /// state and folder are read, and our hooks win any disagreement.
    #[test]
    fn orca_status_file_is_a_fallback_that_reads_only_state_and_folder() {
        let file = r#"{"version":1,"entries":{
            "a":{"worktreeId":"w1::/work/app","payload":{"state":"working","prompt":"SECRET"}},
            "b":{"worktreeId":"w2::/work/lib","payload":{"state":"waiting","lastAssistantMessage":"x"}},
            "c":{"worktreeId":"w3::/work/new","payload":{"state":"some-future-state"}},
            "d":{"worktreeId":"no-folder","payload":{"state":"done"}}}}"#;
        let mut orca = orca_sessions(file);
        orca.sort_by(|a, b| a.cwd.cmp(&b.cwd));
        assert_eq!(
            orca,
            [
                Session {
                    state: "thinking",
                    cwd: "/work/app".into(),
                    kind: "",
                },
                Session {
                    state: "needs-input",
                    cwd: "/work/lib".into(),
                    kind: "",
                },
            ]
        );
        assert!(format!("{orca:?}").find("SECRET").is_none());
        assert!(orca_sessions("not json").is_empty());
        assert!(orca_sessions(r#"{"entries":[]}"#).is_empty());
        // Our hook says /work/app finished: it wins over Orca's "working".
        let hooks = vec![Session {
            state: "finished",
            cwd: "/work/app".into(),
            kind: "",
        }];
        let all = with_fallback(hooks, orca);
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.iter().find(|s| s.cwd == "/work/app").unwrap().state,
            "finished"
        );
    }

    /// v7 ticket 18: herdr's agent list lights folders (foreground cwd first); done is
    /// finished, unknown is ignored, and our hooks win.
    #[test]
    fn herdr_agents_light_their_folders_and_our_hooks_win() {
        let list = r#"{"id":"1","result":{"agents":[
            {"name":"api","pane_id":"w1:p1","agent_status":"working","cwd":"/home","foreground_cwd":"/work/api"},
            {"name":"docs","pane_id":"w1:p2","agent_status":"blocked","cwd":"/work/docs"},
            {"name":"old","pane_id":"w1:p3","agent_status":"done","cwd":"/work/old"},
            {"name":"odd","pane_id":"w1:p4","agent_status":"unknown","cwd":"/work/odd"}]}}"#;
        let found = herdr_sessions(list);
        assert_eq!(
            found,
            [
                Session {
                    state: "thinking",
                    cwd: "/work/api".into(),
                    kind: "",
                },
                Session {
                    state: "needs-input",
                    cwd: "/work/docs".into(),
                    kind: "",
                },
                Session {
                    state: "finished",
                    cwd: "/work/old".into(),
                    kind: "",
                },
            ]
        );
        assert!(herdr_sessions("").is_empty());
        let names = herdr_agents(list)
            .into_iter()
            .map(|a| a.0)
            .collect::<Vec<_>>();
        assert_eq!(names, ["api", "docs", "old", "odd"]);
        let hooks = vec![Session {
            state: "idle",
            cwd: "/work/api".into(),
            kind: "",
        }];
        assert_eq!(with_fallback(hooks, found).len(), 3);
    }

    /// v7 ticket 59: a session from Claude in an Orca-hosted agent's folder lights that
    /// agent's own state; a session two agents could claim, or of another kind, stays a
    /// folder session; files written before the kind was recorded still read.
    #[test]
    fn orca_hosted_agents_light_their_own_state_and_ambiguous_ones_light_by_folder() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-hosted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hook = |id: &str, cwd: &str| format!(r#"{{"session_id": "{id}", "cwd": "{cwd}"}}"#);
        session_report(
            &dir,
            &hook("a1", "/work/app/src"),
            "needs-input",
            0,
            "claude",
        )
        .unwrap();
        session_report(&dir, &hook("b2", "/work/app"), "thinking", 0, "claude").unwrap();
        session_report(&dir, &hook("c3", "/work/app"), "finished", 0, "codex").unwrap();
        session_report(&dir, &hook("d4", "/work/both"), "thinking", 0, "claude").unwrap();
        std::fs::write(dir.join("session-e5"), "thinking\t0\t/work/old\n").unwrap();
        let mut sessions = sessions_in(&dir, |_| true);
        sessions.sort_by(|a, b| (&a.cwd, a.kind).cmp(&(&b.cwd, b.kind)));
        let hosted = [
            (
                "orca-claude".to_owned(),
                "claude".to_owned(),
                "/work/app/".to_owned(),
            ),
            (
                "one".to_owned(),
                "claude".to_owned(),
                "/work/both".to_owned(),
            ),
            (
                "two".to_owned(),
                "claude".to_owned(),
                "/work/both".to_owned(),
            ),
        ];
        let (states, rest) = hosted_states(sessions, &hosted);
        assert_eq!(states, [("orca-claude".to_owned(), "needs-input")]);
        let rest = rest
            .iter()
            .map(|s| (s.cwd.as_str(), s.kind, s.state))
            .collect::<Vec<_>>();
        assert_eq!(
            rest,
            [
                ("/work/app", "codex", "finished"),
                ("/work/both", "claude", "thinking"),
                ("/work/old", "", "thinking"),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Ticket 16: sessions outside tmux report by session_id and cwd, end on SessionEnd or
    /// when their process is gone, and light the most specific matching folder.
    #[test]
    fn outside_sessions_light_their_most_specific_folder() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hook = |id: &str, cwd: &str| {
            format!(r#"{{"session_id": "{id}", "cwd": "{cwd}", "hook_event_name": "Stop"}}"#)
        };
        session_report(&dir, &hook("a1", "/work/app/src"), "thinking", 0, "").unwrap();
        session_report(&dir, &hook("b2", "/work/app"), "needs-input", 4242, "").unwrap();
        session_report(&dir, &hook("c3", "/work/apple"), "finished", 7, "").unwrap(); // not in /work/app
        session_report(&dir, &hook("d4", "/home/me"), "thinking", 0, "").unwrap();
        assert!(session_report(&dir, &hook("../x", "/w"), "thinking", 0, "").is_err());
        assert!(session_report(&dir, &hook("e5", "/w"), "busy", 0, "").is_err());
        session_report(&dir, "{}", "thinking", 0, "").unwrap(); // not a hook payload: ignored
        session_report(&dir, "", "thinking", 0, "").unwrap();
        // The process of b2 is gone: dropped (and its file removed).
        let alive = |pid: u32| pid != 4242;
        let mut found = sessions_in(&dir, alive);
        found.sort_by(|a, b| a.cwd.cmp(&b.cwd));
        let cwds = found.iter().map(|s| s.cwd.as_str()).collect::<Vec<_>>();
        assert_eq!(cwds, ["/home/me", "/work/app/src", "/work/apple"]);
        assert!(!dir.join("session-b2").exists());
        // SessionEnd ("ended" reaches here as idle) removes a session.
        session_report(&dir, &hook("d4", "/home/me"), "idle", 0, "").unwrap();
        assert_eq!(sessions_in(&dir, alive).len(), 2);
        let light = |octave: i8| octave;
        let folders = vec![
            ("/work".to_owned(), light(2)),
            ("/work/app/".to_owned(), light(3)),
        ];
        let sessions = vec![
            Session {
                state: "thinking",
                cwd: "/work/app/src".into(),
                kind: "",
            },
            Session {
                state: "needs-input",
                cwd: "/work/app".into(),
                kind: "",
            },
            Session {
                state: "finished",
                cwd: "/work/apple".into(),
                kind: "",
            },
            Session {
                state: "thinking",
                cwd: "/home/me".into(),
                kind: "",
            },
            Session {
                state: "idle",
                cwd: "/work".into(),
                kind: "",
            },
        ];
        // /work/app wins over /work; its two sessions combine to the most urgent.
        assert_eq!(
            source_states(&sessions, &folders, None),
            [
                ("/work/app/".to_owned(), "needs-input", light(3)),
                ("/work".to_owned(), "finished", light(2)),
            ]
        );
        // Every other session catches what no folder matched, only when on.
        let other = source_states(&sessions, &folders, Some(light(-2)));
        assert_eq!(other[2], ("/home/me".to_owned(), "thinking", light(-2)));
        assert!(source_states(&sessions, &[], None).is_empty());
        assert!(within("/a/b", "/a/b") && within("/a/b/c", "/a/b/") && !within("/a/bc", "/a/b"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Tickets 14 and 17: the default pattern table, priorities, the once-a-second turns and
    /// the half-second alternate ±, and edited patterns.
    #[test]
    fn agent_states_map_to_status_lights_by_pattern_and_priority() {
        let config = crate::mappings::Config::default();
        let (claude, codex) = (config.agents[0].octave, config.agents[1].octave);
        assert_eq!((claude, codex), (1, -1)); // spec defaults
        let patterns = Patterns::default();
        let light = |claude_state, codex_state, millis| {
            lights(
                &[(claude_state, claude), (codex_state, codex)],
                &patterns,
                millis,
            )
        };
        let set = |octave, tempo, arp| Settings {
            octave: Some(octave),
            tempo,
            arp: Some(arp),
        };
        assert_eq!(light("idle", "idle", 0), None); // program feedback shows
        assert_eq!(light("thinking", "idle", 0), Some(set(0, Some(50), true)));
        assert_eq!(
            light("needs-input", "idle", 0),
            Some(set(1, Some(240), true))
        );
        assert_eq!(
            light("idle", "needs-input", 1000),
            Some(set(-1, Some(240), true))
        );
        // Needs input beats thinking for the tempo; the steady octave beats a finished blink.
        assert_eq!(
            light("thinking", "needs-input", 0),
            Some(set(-1, Some(240), true))
        );
        assert_eq!(
            light("finished", "needs-input", 0),
            Some(set(-1, Some(240), true))
        );
        // One finished agent blinks its octave against 0; arpeggiator off, tempo untouched.
        assert_eq!(light("finished", "idle", 0), Some(set(1, None, false)));
        assert_eq!(light("finished", "idle", 1000), Some(set(0, None, false)));
        assert_eq!(light("finished", "idle", 2000), Some(set(1, None, false)));
        // Two finished, or two needing input: their own octaves take turns once a second.
        for both in ["finished", "needs-input"] {
            assert_eq!(light(both, both, 0).unwrap().octave, Some(1));
            assert_eq!(light(both, both, 1000).unwrap().octave, Some(-1));
        }
        // Thinking plus a finished agent: slow blink and the finished octave blinking.
        assert_eq!(
            light("thinking", "finished", 0),
            Some(set(-1, Some(50), true))
        );
        assert_eq!(
            light("thinking", "finished", 1000),
            Some(set(0, Some(50), true))
        );
        // Ticket 17: a decision flips the agent's octave twice a second and beats needs input.
        assert_eq!(
            light("needs-decision", "idle", 0),
            Some(set(1, Some(240), true))
        );
        assert_eq!(
            light("needs-decision", "idle", 500),
            Some(set(-1, Some(240), true))
        );
        assert_eq!(
            light("needs-decision", "idle", 1000),
            Some(set(1, Some(240), true))
        );
        assert_eq!(
            light("needs-decision", "needs-input", 500).unwrap().octave,
            Some(-1)
        );
        assert_eq!(light("idle", "needs-decision", 0).unwrap().octave, Some(-1));
        assert_eq!(
            light("idle", "needs-decision", 500).unwrap().octave,
            Some(1)
        );
        // Edited patterns: thinking names its agent, finished blinks the tempo fast.
        let mut edited = Patterns::default();
        edited.states[2] = (Tempo::Slow, Octave::Steady);
        edited.states[3] = (Tempo::Fast, Octave::Off);
        edited.slow = 90;
        assert_eq!(
            lights(&[("thinking", 4)], &edited, 0),
            Some(set(4, Some(90), true))
        );
        assert_eq!(
            lights(&[("finished", 4)], &edited, 0),
            Some(set(0, Some(240), true))
        );
        // The most urgent state with a tempo sets it: finished (fast) loses to thinking (slow).
        assert_eq!(
            lights(&[("thinking", 1), ("finished", 2)], &edited, 0)
                .unwrap()
                .tempo,
            Some(90)
        );
        assert_eq!(lights(&[], &patterns, 0), None);
        assert!(valid_octave(0).is_err() && valid_octave(5).is_err() && valid_octave(-4).is_ok());
        for (word, tempo) in TEMPOS {
            assert_eq!(tempo_word(tempo), word);
        }
        for (word, octave) in OCTAVES {
            assert_eq!(octave_word(octave), word);
        }
    }

    /// v7 ticket 113: the keyboard LED follows the most urgent busy state's pattern, and a
    /// pattern is lit or dark by time.
    #[test]
    fn the_keyboard_light_follows_the_most_urgent_state() {
        let mut patterns = DevicePatterns::default();
        assert!(!patterns.keyboard_on, "off until turned on");
        patterns.keyboard = [Light::Steady, Light::FastBlink, Light::Off, Light::Blink];
        assert_eq!(
            keyboard_light(&["thinking", "needs-decision"], &patterns),
            Some(Light::Steady)
        );
        assert_eq!(keyboard_light(&["finished"], &patterns), Some(Light::Blink));
        assert_eq!(keyboard_light(&["idle"], &patterns), None);
        assert!(light_on(Light::Steady, 123) && !light_on(Light::Off, 0));
        assert!(light_on(Light::Blink, 100) && !light_on(Light::Blink, 600));
        assert!(light_on(Light::FastBlink, 10) && !light_on(Light::FastBlink, 200));
        assert!(light_on(Light::SlowPulse, 900) && !light_on(Light::SlowPulse, 1500));
    }
}
