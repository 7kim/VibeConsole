//! Agent status: `vibeconsole agent-status` writes one small file per agent; the session reads them.
use crate::Result;
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
    let agent = if agent == "-" {
        match std::env::var(AGENT_ENV) {
            Ok(name) if !name.is_empty() => name,
            _ => return Ok(()),
        }
    } else {
        agent
    };
    let config = crate::mappings::load_config(&crate::mappings::config_path()?)?;
    if !config.agents.iter().any(|a| a.name == agent) {
        return Err(format!("unknown agent {agent:?}").into());
    }
    write(&agent, state)
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

pub fn claude_hooks() -> String {
    hooks(&[
        ("UserPromptSubmit", None, "thinking"),
        (
            "Notification",
            Some("permission_prompt|elicitation_dialog"),
            "needs-input",
        ),
        ("Stop", None, "finished"),
    ])
}

pub fn codex_hooks() -> String {
    hooks(&[
        ("UserPromptSubmit", None, "thinking"),
        ("PermissionRequest", None, "needs-input"),
        ("Stop", None, "finished"),
    ])
}

fn hooks(events: &[(&str, Option<&str>, &str)]) -> String {
    let groups = events
        .iter()
        .map(|(event, matcher, state)| {
            let matcher = matcher.map_or(String::new(), |m| format!("\"matcher\": \"{m}\", "));
            format!(
                "    \"{event}\": [{{{matcher}\"hooks\": [{{\"type\": \"command\", \"command\": \"vibeconsole agent-status - {state}\"}}]}}]"
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
        assert!(command(&["claude".into()]).is_err());
        assert!(command(&["a".into(), "b".into(), "c".into(), "d".into()]).is_err());
        let claude = claude_hooks();
        assert!(claude.contains("\"matcher\": \"permission_prompt|elicitation_dialog\""));
        assert!(claude.contains("agent-status - finished"));
        assert!(codex_hooks().contains("\"PermissionRequest\""));
    }
}
