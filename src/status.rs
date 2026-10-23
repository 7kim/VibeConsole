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

fn dir() -> Result<PathBuf> {
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
    let config = crate::mappings::load_config(&crate::mappings::config_path()?)?;
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
    session_report(&dir()?, &input, state, agent_process())
}

fn session_report(dir: &Path, input: &str, state: &str, pid: u32) -> Result<()> {
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
    crate::actions::write_atomic(&file, &format!("{state}\t{pid}\t{cwd}\n"))
}

/// The Claude Code or Codex process this hook runs under (it runs hooks through a shell), or
/// 0 when none is found; a session whose process is gone is dropped.
fn agent_process() -> u32 {
    // SAFETY: getppid has no preconditions.
    let mut pid = unsafe { libc::getppid() } as u32;
    for _ in 0..8 {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return 0;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            return 0;
        };
        if matches!(&stat[open + 1..close], "claude" | "codex") {
            return pid;
        }
        match stat[close + 1..]
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse().ok())
        {
            Some(parent) if parent > 1 => pid = parent,
            _ => return 0,
        }
    }
    0
}

/// A session outside VibeConsole's tmux: its state and working folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Session {
    pub state: &'static str,
    pub cwd: String,
}

/// Recorded sessions, dropping those whose agent process is gone.
pub fn sessions() -> Vec<Session> {
    dir()
        .map(|d| sessions_in(&d, |pid| Path::new(&format!("/proc/{pid}")).exists()))
        .unwrap_or_default()
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
        let mut fields = text.trim_end_matches('\n').splitn(3, '\t');
        let (Some(state), Some(pid), Some(cwd)) = (fields.next(), fields.next(), fields.next())
        else {
            let _ = std::fs::remove_file(entry.path());
            continue;
        };
        let state = STATES.into_iter().find(|s| *s == state);
        let pid = pid.parse::<u32>().ok();
        // ponytail: no process found (pid 0) relies on SessionEnd; a crash then leaves the
        // state until the next prompt in that session. Add an age limit if that bites.
        match (state, pid) {
            (Some(state), Some(pid)) if pid == 0 || alive(pid) => found.push(Session {
                state,
                cwd: cwd.to_owned(),
            }),
            _ => {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    found
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
pub fn tempo_word(tempo: Tempo) -> &'static str {
    TEMPOS
        .iter()
        .find(|(_, t)| *t == tempo)
        .map_or("off", |(w, _)| w)
}
pub fn octave_word(octave: Octave) -> &'static str {
    OCTAVES
        .iter()
        .find(|(_, o)| *o == octave)
        .map_or("off", |(w, _)| w)
}
/// Busy states, most urgent first: the order that decides the shared tempo light.
pub const BUSY: [&str; 4] = ["needs-decision", "needs-input", "thinking", "finished"];

/// Ticket 17: what each busy state shows, in `BUSY` order, and the two tempos.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Patterns {
    pub slow: u16,
    pub fast: u16,
    pub states: [(Tempo, Octave); 4],
}
impl Default for Patterns {
    fn default() -> Self {
        Self {
            slow: 50,
            fast: 240,
            states: [
                (Tempo::Fast, Octave::Alternate), // needs a decision
                (Tempo::Fast, Octave::Steady),    // needs input (permission)
                (Tempo::Slow, Octave::Off),       // thinking
                (Tempo::Off, Octave::Blink),      // finished
            ],
        }
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
// (tests/v5/07 gate unrecorded); retune from its measured limit.
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

    /// Ticket 16: sessions outside tmux report by session_id and cwd, end on SessionEnd or
    /// when their process is gone, and light the most specific matching folder.
    #[test]
    fn outside_sessions_light_their_most_specific_folder() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hook = |id: &str, cwd: &str| {
            format!(r#"{{"session_id": "{id}", "cwd": "{cwd}", "hook_event_name": "Stop"}}"#)
        };
        session_report(&dir, &hook("a1", "/work/app/src"), "thinking", 0).unwrap();
        session_report(&dir, &hook("b2", "/work/app"), "needs-input", 4242).unwrap();
        session_report(&dir, &hook("c3", "/work/apple"), "finished", 7).unwrap(); // not in /work/app
        session_report(&dir, &hook("d4", "/home/me"), "thinking", 0).unwrap();
        assert!(session_report(&dir, &hook("../x", "/w"), "thinking", 0).is_err());
        assert!(session_report(&dir, &hook("e5", "/w"), "busy", 0).is_err());
        session_report(&dir, "{}", "thinking", 0).unwrap(); // not a hook payload: ignored
        session_report(&dir, "", "thinking", 0).unwrap();
        // The process of b2 is gone: dropped (and its file removed).
        let alive = |pid: u32| pid != 4242;
        let mut found = sessions_in(&dir, alive);
        found.sort_by(|a, b| a.cwd.cmp(&b.cwd));
        let cwds = found.iter().map(|s| s.cwd.as_str()).collect::<Vec<_>>();
        assert_eq!(cwds, ["/home/me", "/work/app/src", "/work/apple"]);
        assert!(!dir.join("session-b2").exists());
        // SessionEnd ("ended" reaches here as idle) removes a session.
        session_report(&dir, &hook("d4", "/home/me"), "idle", 0).unwrap();
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
            },
            Session {
                state: "needs-input",
                cwd: "/work/app".into(),
            },
            Session {
                state: "finished",
                cwd: "/work/apple".into(),
            },
            Session {
                state: "thinking",
                cwd: "/home/me".into(),
            },
            Session {
                state: "idle",
                cwd: "/work".into(),
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
}
