//! Agent status: `vibeconsole agent-status` writes one small file per agent; the session reads them.
use crate::Result;
use crate::feedback::Settings;
use std::path::{Path, PathBuf};

pub const STATES: [&str; 4] = ["idle", "thinking", "needs-input", "finished"];
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
    let hook = agent == "-";
    let agent = if hook {
        match std::env::var(AGENT_ENV) {
            Ok(name) if !name.is_empty() => name,
            _ => return Ok(()),
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
        return Ok(());
    }
    write(&agent, state)
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

/// Ticket 14 (V5 spec decision 7): agent states to tempo/arpeggiator/octave settings.
/// None when every agent is idle, so the program's own feedback shows. `second` counts whole
/// seconds and drives the octave alternation.
/// - Tempo/arpeggiator: needs input (fast) > thinking (slow) > off.
/// - Octave: needs input holds that agent's octave steady (several alternate between them);
///   otherwise one finished agent blinks between its octave and 0, several alternate.
// ponytail: one-second alternation and spec-default tempos, not yet checked on hardware
// (tests/v5/07 gate unrecorded); a faster interval waits for the gate's measured limit.
pub fn lights(agents: &[(&str, &crate::mappings::Agent)], second: u64) -> Option<Settings> {
    let with = |state: &str| {
        agents
            .iter()
            .filter(|(s, _)| *s == state)
            .map(|(_, agent)| *agent)
            .collect::<Vec<_>>()
    };
    let (input, thinking, finished) = (with("needs-input"), with("thinking"), with("finished"));
    if input.is_empty() && thinking.is_empty() && finished.is_empty() {
        return None;
    }
    let tempo = input
        .first()
        .map(|a| a.input_bpm)
        .or(thinking.first().map(|a| a.thinking_bpm));
    let cycle = match (input.as_slice(), finished.as_slice()) {
        ([], [one]) => vec![one.octave, 0],
        ([], many) | (many, _) => many.iter().map(|a| a.octave).collect(),
    };
    Some(Settings {
        octave: Some(
            cycle
                .get(second as usize % cycle.len().max(1))
                .copied()
                .unwrap_or(0),
        ),
        tempo,
        arp: Some(tempo.is_some()),
    })
}

/// (event, matcher, state) per agent tool; both the shown hooks and the installer use them.
type Events = [(&'static str, Option<&'static str>, &'static str)];
const CLAUDE: &Events = &[
    ("UserPromptSubmit", None, "thinking"),
    (
        "Notification",
        Some("permission_prompt|elicitation_dialog"),
        "needs-input",
    ),
    ("Stop", None, "finished"),
];
const CODEX: &Events = &[
    ("UserPromptSubmit", None, "thinking"),
    ("PermissionRequest", None, "needs-input"),
    ("Stop", None, "finished"),
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
        assert!(claude.contains("\"matcher\": \"permission_prompt|elicitation_dialog\""));
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
            "permission_prompt|elicitation_dialog"
        );
        // An existing file keeps its keys, order, other hooks and permissions.
        let existing = r#"{"theme": "dark", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "notify-send done"}]}]}, "tui": "fullscreen"}"#;
        std::fs::write(&path, existing).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            status_in(&path, CLAUDE).unwrap(),
            format!(
                "missing UserPromptSubmit, Notification, Stop in {}; Agents can install them",
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

    /// Ticket 14: the spec-default table, priorities and the once-a-second alternation.
    #[test]
    fn agent_states_map_to_status_lights_by_priority() {
        let config = crate::mappings::Config::default();
        let (claude, codex) = (&config.agents[0], &config.agents[1]);
        assert_eq!((claude.octave, codex.octave), (1, -1)); // spec defaults
        let light = |claude_state, codex_state, second| {
            lights(&[(claude_state, claude), (codex_state, codex)], second)
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
            light("idle", "needs-input", 1),
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
        assert_eq!(light("finished", "idle", 1), Some(set(0, None, false)));
        assert_eq!(light("finished", "idle", 2), Some(set(1, None, false)));
        // Both finished, or both needing input: + and - alternate.
        for both in ["finished", "needs-input"] {
            assert_eq!(light(both, both, 0).unwrap().octave, Some(1));
            assert_eq!(light(both, both, 1).unwrap().octave, Some(-1));
        }
        // Thinking plus a finished agent: slow blink and the finished octave blinking.
        assert_eq!(
            light("thinking", "finished", 0),
            Some(set(-1, Some(50), true))
        );
        assert_eq!(
            light("thinking", "finished", 1),
            Some(set(0, Some(50), true))
        );
        // Per-agent settings are used.
        let mut tuned = claude.clone();
        (tuned.octave, tuned.thinking_bpm) = (4, 90);
        assert_eq!(
            lights(&[("thinking", &tuned)], 0),
            Some(set(0, Some(90), true))
        );
        assert_eq!(
            lights(&[("needs-input", &tuned)], 0).unwrap().octave,
            Some(4)
        );
        assert_eq!(lights(&[], 0), None);
    }
}
