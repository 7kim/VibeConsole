use crate::Result;
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

pub use vibeconsole_core::owner::Owner;

const UNIT: &str = "vibeconsole.service";
/// Start the login service when this session closes: set by a handover (ticket 20) or by
/// Settings -> Login service (ticket 21).
pub static RUN_ON_QUIT: AtomicBool = AtomicBool::new(false);

/// The session's owner lock. Dropping it releases the lock first, then starts the login
/// service if `restart` asks for it, so the service can take the lock.
pub struct Session<O, F: FnMut(&[&str]) -> Result<()>> {
    owner: Option<O>,
    restart: &'static AtomicBool,
    systemctl: F,
}
impl<O, F: FnMut(&[&str]) -> Result<()>> Drop for Session<O, F> {
    fn drop(&mut self) {
        self.owner.take();
        if self.restart.swap(false, Ordering::SeqCst) {
            match (self.systemctl)(&["start", UNIT]) {
                Ok(()) => eprintln!("Login service started again."),
                Err(e) => {
                    eprintln!("Login service not started: {e}; run vibeconsole startup-start")
                }
            }
        }
    }
}
/// Take the owner lock. If the login service holds it, stop the service for this session
/// and start it again on quit (ticket 20); any other lock holder is still refused.
fn take_over<O, F: FnMut(&[&str]) -> Result<()>>(
    acquire: impl Fn() -> Result<O>,
    mut systemctl: F,
    restart: &'static AtomicBool,
) -> Result<Session<O, F>> {
    let locked = match acquire() {
        Ok(owner) => {
            return Ok(Session {
                owner: Some(owner),
                restart,
                systemctl,
            });
        }
        Err(error) => error,
    };
    if systemctl(&["is-active", "--quiet", UNIT]).is_err() {
        return Err(locked);
    }
    systemctl(&["stop", UNIT])
        .map_err(|e| format!("could not pause the login service ({e}); no session opened"))?;
    println!("Login service paused for this session; it starts again when you quit.");
    restart.store(true, Ordering::SeqCst);
    let mut session = Session {
        owner: None,
        restart,
        systemctl,
    };
    session.owner = Some(acquire()?);
    Ok(session)
}
pub fn session() -> Result<Session<Owner, fn(&[&str]) -> Result<()>>> {
    take_over(
        || Owner::acquire("terminal session"),
        systemctl,
        &RUN_ON_QUIT,
    )
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| "HOME must be absolute".into())
}
fn unit() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or(home()?.join(".config"));
    if !base.is_absolute() {
        return Err("XDG_CONFIG_HOME must be absolute".into());
    }
    Ok(base.join("systemd/user/vibeconsole.service"))
}
const EXTENSION_UUID: &str = "vibeconsole@vibeconsole.local";
const EXTENSION_FILES: [(&str, &str); 2] = [
    ("metadata.json", include_str!("../extension/metadata.json")),
    ("extension.js", include_str!("../extension/extension.js")),
];
/// The window-control Shell extension that launcher pads use for minimize/close/restore.
fn extension_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or(home()?.join(".local/share"));
    if !base.is_absolute() {
        return Err("XDG_DATA_HOME must be absolute".into());
    }
    Ok(base.join("gnome-shell/extensions").join(EXTENSION_UUID))
}
fn service() -> &'static str {
    "# Managed by VibeConsole\n[Unit]\nDescription=VibeConsole controller shortcuts\nAfter=graphical-session.target pipewire.service wireplumber.service\nPartOf=graphical-session.target\n\n[Service]\nType=simple\nExecStart=\"%h/.local/bin/vibeconsole\" daemon\nRestart=no\n# SIGTERM goes to the main process only, so its handled shutdown restores controller feedback\nKillMode=mixed\nTimeoutStopSec=15\n\n[Install]\nWantedBy=graphical-session.target\n"
}
fn managed(path: &Path) -> Result<()> {
    if path.exists() && !fs::read_to_string(path)?.starts_with("# Managed by VibeConsole\n") {
        return Err(format!("refusing to replace/remove unmanaged {}", path.display()).into());
    }
    Ok(())
}
/// v7.7 ticket 203: journald's native socket. A terminal run's status lines go there, under
/// the login service's name, so `journalctl --user -t vibeconsole` shows both.
pub const JOURNAL: &str = "/run/systemd/journal/socket";
/// One line to the journal; with no journal it is dropped and the run goes on. Tests never
/// call it (they keep the lines in their script).
#[cfg_attr(test, allow(dead_code))]
pub fn journal(line: &str) {
    let message = format!(
        "SYSLOG_IDENTIFIER=vibeconsole\nPRIORITY=6\nMESSAGE={}\n",
        line.replace('\n', " | ")
    );
    if let Ok(socket) = std::os::unix::net::UnixDatagram::unbound() {
        let _ = socket.send_to(message.as_bytes(), JOURNAL);
    }
}
/// Output is captured: Settings calls this inside the full-screen session.
fn systemctl(args: &[&str]) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "systemctl --user {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}
fn binary() -> Result<PathBuf> {
    Ok(home()?.join(".local/bin/vibeconsole"))
}
/// Write (or refresh) the managed user unit; it needs the installed binary.
fn write_unit() -> Result<()> {
    if !binary()?.is_file() {
        return Err("run vibeconsole install first".into());
    }
    write_unit_at(&unit()?, systemctl)
}
fn write_unit_at(path: &Path, mut systemctl: impl FnMut(&[&str]) -> Result<()>) -> Result<()> {
    managed(path)?;
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(path, service())?;
    systemctl(&["daemon-reload"])
}
/// v8 ticket 117: `install` brings an installed managed unit up to date. It never enables or
/// disables it, and leaves a missing or hand-written unit alone. True when it rewrote one.
fn refresh_installed_unit(
    path: &Path,
    systemctl: impl FnMut(&[&str]) -> Result<()>,
) -> Result<bool> {
    if !path.is_file() || managed(path).is_err() {
        return Ok(false);
    }
    write_unit_at(path, systemctl)
        .map_err(|e| format!("install: updating the login service unit failed: {e}"))?;
    Ok(true)
}
/// Settings -> Login service (ticket 21).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Login {
    Enabled,
    Enable,
    Disable,
    RunOnQuit,
    SetRunOnQuit(bool),
}
pub fn login(request: Login) -> Result<bool> {
    match request {
        Login::Enabled => Ok(systemctl(&["is-enabled", "--quiet", UNIT]).is_ok()),
        Login::Enable => {
            write_unit()?;
            systemctl(&["enable", UNIT])?;
            Ok(true)
        }
        Login::Disable => systemctl(&["disable", UNIT]).map(|()| false),
        Login::RunOnQuit => Ok(RUN_ON_QUIT.load(Ordering::SeqCst)),
        Login::SetRunOnQuit(on) => {
            if on {
                write_unit()?;
            }
            RUN_ON_QUIT.store(on, Ordering::SeqCst);
            Ok(on)
        }
    }
}
/// v7.7 ticket 187: one environment VibeConsole runs in: its PATH and the variables it
/// relies on, as `NAME=value` pairs (a terminal: this process; the service: the user
/// manager's `systemctl --user show-environment`).
#[derive(Clone, Debug, Default)]
pub struct ModeEnv {
    pub path: String,
    pub vars: Vec<(String, String)>,
}
impl ModeEnv {
    /// `systemctl --user show-environment`'s `NAME=value` lines.
    pub fn parse(text: &str) -> Self {
        let vars = text
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(n, v)| (n.to_owned(), v.to_owned()))
            .collect::<Vec<_>>();
        let path = vars
            .iter()
            .find(|(n, _)| n == "PATH")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        Self { path, vars }
    }
    pub fn here() -> Self {
        let vars = std::env::vars().collect::<Vec<_>>();
        Self {
            path: std::env::var("PATH").unwrap_or_default(),
            vars,
        }
    }
    fn has(&self, name: &str) -> bool {
        self.vars.iter().any(|(n, v)| n == name && !v.is_empty())
    }
}
/// v7.7 ticket 187: one feature as each mode has it, why they differ, and how it should be.
#[derive(Clone, Debug, PartialEq)]
pub struct ModeRow {
    pub feature: String,
    pub terminal: String,
    pub service: String,
    pub why: String,
    pub should: String,
}
impl ModeRow {
    pub fn differs(&self) -> bool {
        self.terminal != self.service
    }
}
/// v7.7 ticket 187: the variables VibeConsole needs, and what each serves.
const MODE_VARS: [(&str, &str); 4] = [
    ("HOME", "config, Orca's CLI and status file, skills"),
    ("XDG_RUNTIME_DIR", "agent status hooks"),
    ("WAYLAND_DISPLAY", "the focused app and window control"),
    (
        "DBUS_SESSION_BUS_ADDRESS",
        "the window extension and app launchers",
    ),
];
/// v7.7 ticket 187: the programs VibeConsole runs by name, and what each serves.
const MODE_TOOLS: [(&str, &str); 8] = [
    ("tmux", "tmux agents, terminal commands, tmux jumps"),
    ("herdr", "herdr agents and their lights"),
    ("gio", "app launchers"),
    ("gdbus", "the window extension (focus, minimize)"),
    ("wpctl", "volume and mute"),
    ("pw-dump", "the Xbox headset"),
    ("timeout", "Ask <agent> (3 minutes at most)"),
    ("git", "Ask how to improve VibeConsole"),
];
/// v7.7 ticket 187: the same probe list for the terminal and the service: what differs by
/// design, then every variable and program VibeConsole relies on, found or not in each
/// environment. Read-only: nothing is written, started or sent.
pub fn compare_modes(terminal: &ModeEnv, service: &ModeEnv, agent: (&str, &str)) -> Vec<ModeRow> {
    let row = |feature: String, terminal: &str, service: &str, why: &str, should: &str| ModeRow {
        feature,
        terminal: terminal.into(),
        service: service.into(),
        why: why.into(),
        should: should.into(),
    };
    let mut rows = vec![
        row(
            "Screen: tabs, menus, Find, AI asks, $EDITOR".into(),
            "works",
            "no screen",
            "the service has no terminal",
            "by design: run `vibeconsole` in a terminal; the service pauses for that session",
        ),
        row(
            "Piano mappings".into(),
            "after Run is confirmed",
            "inactive",
            "their safety confirmation needs someone at a terminal",
            "by design: confirm Run in a terminal session",
        ),
        row(
            "Akai agent status lights".into(),
            "works",
            "works",
            "",
            "the same in both (ticket 186)",
        ),
        row(
            "Xbox button light and rumble for agents".into(),
            "works",
            "works",
            "",
            "the same in both: they follow running output, not the terminal",
        ),
        row(
            "Keyboard lights for agents (lock LED, Razer RGB)".into(),
            "works",
            "works",
            "",
            "the same in both: they follow running output, not the terminal",
        ),
        row(
            "App profiles follow the focused app".into(),
            "works",
            "works",
            "",
            "the same in both, when WAYLAND_DISPLAY and DBUS_SESSION_BUS_ADDRESS are set (below)",
        ),
    ];
    let found = |env: &ModeEnv, tool: &str| {
        if crate::mappings::executable_on_path(&env.path, tool) {
            "found"
        } else {
            "not on PATH"
        }
    };
    let set = |env: &ModeEnv, var: &str| if env.has(var) { "set" } else { "not set" };
    for (var, serves) in MODE_VARS {
        let (t, s) = (set(terminal, var), set(service, var));
        rows.push(row(
            format!("{var} ({serves})"),
            t,
            s,
            if t == s { "" } else { "the service's environment differs from the terminal's" },
            if t == s {
                "the same in both"
            } else {
                "the same in both: set it for the user manager (`systemctl --user import-environment`, or `~/.config/environment.d/`), then restart the service"
            },
        ));
    }
    let tools = MODE_TOOLS
        .iter()
        .map(|(tool, serves)| (tool.to_string(), serves.to_string()))
        .chain([(
            agent.1.to_string(),
            format!("the AI tab's default agent, {}", agent.0),
        )]);
    for (tool, serves) in tools {
        let (t, s) = (found(terminal, &tool), found(service, &tool));
        rows.push(row(
            format!("{tool} ({serves})"),
            t,
            s,
            &if t == s {
                String::new()
            } else {
                format!("{tool} isn't on the PATH of the mode that lacks it")
            },
            &if t == s {
                "the same in both".to_string()
            } else {
                format!("the same in both: put {tool}'s folder on the service's PATH (`~/.config/environment.d/`), then restart the service")
            },
        ));
    }
    rows
}
/// v7.7 ticket 187: the comparison as `vibeconsole compare-modes` prints it; `≠` marks a
/// row that differs.
pub fn compare_text(rows: &[ModeRow]) -> String {
    let mut text = String::from(
        "Terminal session vs login service (read-only; nothing was started or written)\n\n",
    );
    for r in rows {
        text += &format!(
            "{} {}\n    terminal: {} · service: {}\n",
            if r.differs() { "≠" } else { " " },
            r.feature,
            r.terminal,
            r.service
        );
        if r.differs() {
            text += &format!("    why: {}\n    should: {}\n", r.why, r.should);
        }
    }
    let n = rows.iter().filter(|r| r.differs()).count();
    text += &format!("\n{n} differences.\n");
    text
}
/// v7.7 ticket 158: the VibeConsole skills (ten, and since ticket 184 one per Ask), embedded at build time: (path under
/// `~/.claude/skills`, text). A test keeps this list equal to the repo's `.claude/skills`.
pub const SKILLS: [(&str, &str); 29] = [
    (
        "vibeconsole-agents/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-agents/SKILL.md"),
    ),
    (
        "vibeconsole-doctor/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-doctor/SKILL.md"),
    ),
    (
        "vibeconsole-hardware-test/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-hardware-test/SKILL.md"),
    ),
    (
        "vibeconsole-lights/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-lights/SKILL.md"),
    ),
    (
        "vibeconsole-profiles/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-profiles/SKILL.md"),
    ),
    (
        "vibeconsole-recipes/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-recipes/SKILL.md"),
    ),
    (
        "vibeconsole-release/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-release/SKILL.md"),
    ),
    (
        "vibeconsole-safety-review/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-safety-review/SKILL.md"),
    ),
    (
        "vibeconsole-why-silent/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-why-silent/SKILL.md"),
    ),
    (
        "vibeconsole/SKILL.md",
        include_str!("../.claude/skills/vibeconsole/SKILL.md"),
    ),
    (
        "vibeconsole/examples/browser-tabs.profile",
        include_str!("../.claude/skills/vibeconsole/examples/browser-tabs.profile"),
    ),
    (
        "vibeconsole/examples/editor.profile",
        include_str!("../.claude/skills/vibeconsole/examples/editor.profile"),
    ),
    (
        "vibeconsole/examples/orca.profile",
        include_str!("../.claude/skills/vibeconsole/examples/orca.profile"),
    ),
    (
        "vibeconsole/reference/actions.md",
        include_str!("../.claude/skills/vibeconsole/reference/actions.md"),
    ),
    (
        "vibeconsole/reference/controls.md",
        include_str!("../.claude/skills/vibeconsole/reference/controls.md"),
    ),
    (
        "vibeconsole/reference/formats.md",
        include_str!("../.claude/skills/vibeconsole/reference/formats.md"),
    ),
    (
        "vibeconsole/reference/safety-rules.md",
        include_str!("../.claude/skills/vibeconsole/reference/safety-rules.md"),
    ), // v7.7 ticket 184: one skill for each Ask.
    (
        "vibeconsole-ask-about/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-ask-about/SKILL.md"),
    ),
    (
        "vibeconsole-ask-family/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-ask-family/SKILL.md"),
    ),
    (
        "vibeconsole-ask-improve/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-ask-improve/SKILL.md"),
    ),
    (
        "vibeconsole-ask-map-agents/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-ask-map-agents/SKILL.md"),
    ),
    (
        "vibeconsole-ask-mappings/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-ask-mappings/SKILL.md"),
    ),
    (
        "vibeconsole-ask-preset/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-ask-preset/SKILL.md"),
    ),
    (
        "vibeconsole-ask-profile/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-ask-profile/SKILL.md"),
    ),
    (
        "vibeconsole-ask-prompt/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-ask-prompt/SKILL.md"),
    ),
    (
        "vibeconsole-ask-recommendations/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-ask-recommendations/SKILL.md"),
    ),
    (
        "vibeconsole-ask-sequence/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-ask-sequence/SKILL.md"),
    ),
    (
        "vibeconsole-ask-setup/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-ask-setup/SKILL.md"),
    ),
    (
        "vibeconsole-ask-setup-agents/SKILL.md",
        include_str!("../.claude/skills/vibeconsole-ask-setup-agents/SKILL.md"),
    ),
];
/// v7.7 ticket 158: Install /skills into `home`'s `.claude/skills`: missing files are
/// written, a file that differs is backed up first (`<file>.bak-vibeconsole-<time>`) and
/// replaced, an equal one is left; nothing is ever deleted. Each write is atomic. Returns
/// what was done.
pub fn install_skills(home: &Path) -> Result<String> {
    let root = home.join(".claude/skills");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let (mut written, mut backups) = (0, Vec::new());
    for (path, text) in SKILLS {
        let target = root.join(path);
        match fs::read(&target) {
            Ok(old) if old == text.as_bytes() => continue,
            Ok(_) => {
                let backup = PathBuf::from(format!("{}.bak-vibeconsole-{stamp}", target.display()));
                fs::copy(&target, &backup)?;
                backups.push(backup.display().to_string());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("install skills: {}: {e}", target.display()).into()),
        }
        let folder = target.parent().ok_or("install skills: no folder")?;
        fs::create_dir_all(folder)?;
        let temporary = folder.join(format!(
            ".{}.vibeconsole-tmp",
            target.file_name().unwrap_or_default().to_string_lossy()
        ));
        fs::write(&temporary, text)?;
        fs::rename(&temporary, &target).inspect_err(|_| {
            let _ = fs::remove_file(&temporary);
        })?;
        written += 1;
    }
    Ok(match (written, backups.is_empty()) {
        (0, _) => "Install /skills: already installed; nothing changed.".into(),
        (n, true) => format!(
            "Install /skills: {n} files written into {}.",
            root.display()
        ),
        (n, false) => format!(
            "Install /skills: {n} files written into {}; backed up first: {}",
            root.display(),
            backups.join(", ")
        ),
    })
}
/// v7.7 ticket 158: installed means every skill file is there and equal.
pub fn skills_installed(home: &Path) -> bool {
    SKILLS.iter().all(|(path, text)| {
        fs::read(home.join(".claude/skills").join(path)).is_ok_and(|old| old == text.as_bytes())
    })
}

/// v7.7 ticket 162: where the user's own status line command is kept once VibeConsole's
/// wrapper takes `statusLine` over.
pub fn statusline_saved() -> Result<PathBuf> {
    Ok(crate::mappings::config_path()?.with_file_name("statusline-command"))
}
/// v7.7 ticket 162: `vibeconsole statusline`, Claude Code's `statusLine` command: the JSON on
/// stdin becomes the context file of the agent in `$VIBECONSOLE_AGENT` (outside VibeConsole's
/// agents nothing is written), then the user's own status line command, if they had one,
/// runs with the same input and its output is the status line. Never fails the line.
pub fn statusline() -> Result<()> {
    use std::io::{Read, Write};
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    if let (Ok(agent), Ok(dir)) = (
        std::env::var(crate::status::AGENT_ENV),
        crate::status::state_dir(),
    ) {
        let _ = crate::status::write_context(&input, &agent, &dir);
    }
    let Ok(theirs) = fs::read_to_string(statusline_saved()?) else {
        return Ok(());
    };
    // Their command is the one Claude Code ran as a shell line before; it runs the same way.
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(theirs.trim())
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    child.wait()?;
    Ok(())
}

/// v7.7 ticket 157: a problem `doctor` reports, with its fix.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    /// What it is about (`uinput`, `prerequisite`, `window extension`).
    pub stage: &'static str,
    pub message: String,
    pub fix: Fix,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Fix {
    /// A fixed program with fixed arguments (never a shell line), run after `ask` is
    /// answered yes.
    Run { args: Vec<String>, ask: String },
    /// What to run yourself.
    Show(String),
}
/// What a missing prerequisite's Fedora package is called.
const PACKAGES: [(&str, &str); 5] = [
    ("gio", "glib2"),
    ("wpctl", "wireplumber"),
    ("systemctl", "systemd"),
    ("tmux", "tmux"),
    ("wl-copy", "wl-clipboard"),
];
/// v7.7 ticket 157: doctor's problems that have a fix, from what was probed: `/dev/uinput`
/// (`None` writable, else whether it exists), the prerequisites on PATH, the window
/// extension answering, and the user's name (an identifier, never a shell word).
pub fn problems(
    uinput: Option<bool>,
    on_path: impl Fn(&str) -> bool,
    extension: bool,
    user: &str,
) -> Vec<Problem> {
    let mut found = Vec::new();
    let named = !user.is_empty()
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    match uinput {
        None => {}
        Some(false) => found.push(Problem {
            stage: "uinput",
            message: "uinput missing: no /dev/uinput, so no keys can be sent".into(),
            fix: Fix::Run {
                args: ["pkexec", "modprobe", "uinput"].map(String::from).to_vec(),
                ask: "Load the uinput module? It asks for your password (pkexec modprobe uinput).".into(),
            },
        }),
        Some(true) if named => found.push(Problem {
            stage: "uinput",
            message: "uinput not writable: VibeConsole can't send keys".into(),
            fix: Fix::Run {
                args: ["pkexec", "setfacl", "-m", &format!("u:{user}:rw"), "/dev/uinput"]
                    .map(String::from)
                    .to_vec(),
                ask: format!(
                    "Let {user} write /dev/uinput? It asks for your password (pkexec setfacl) and lasts until the next reboot; the README has the permanent rule."
                ),
            },
        }),
        Some(true) => found.push(Problem {
            stage: "uinput",
            message: "uinput not writable: VibeConsole can't send keys".into(),
            fix: Fix::Show("See the README's permission commands for /dev/uinput.".into()),
        }),
    }
    for (program, package) in PACKAGES {
        if !on_path(program) {
            found.push(Problem {
                stage: "prerequisite",
                message: format!("{program} missing"),
                fix: Fix::Show(format!("Install it: sudo dnf install {package}")),
            });
        }
    }
    if !extension {
        found.push(Problem {
            stage: "window extension",
            message: "Window extension off: launcher pads can't minimize or close windows".into(),
            fix: Fix::Run {
                args: ["gnome-extensions", "enable", EXTENSION_UUID]
                    .map(String::from)
                    .to_vec(),
                ask: format!(
                    "Turn on the window extension? (gnome-extensions enable {EXTENSION_UUID}; after vibeconsole install it needs one logout and login first)"
                ),
            },
        });
    }
    found
}
/// v7.7 ticket 192: the status lights doctor checks, by their doctor label, each with
/// whether Status light patterns has it on (the master Status lights switch included).
pub fn light_switches(
    status_lights: bool,
    p: &crate::status::DevicePatterns,
) -> [(&'static str, bool); 7] {
    [
        ("Xbox button light", p.on[1]),
        ("Keyboard light", p.keyboard_on),
        ("Keyboard M light", p.keyboard_on && p.keyboard_leds[3]),
        ("RGB keyboard", p.rgb_devices[0].on),
        ("Speakers RGB", p.rgb_devices[1].on),
        ("Mousepad RGB", p.rgb_devices[2].on),
        ("Mouse light (Logitech G502)", p.rgb_devices[3].on),
    ]
    .map(|(label, on)| (label, on && status_lights))
}
/// A light's doctor line (what its probe found) with its switch; switched on but
/// unavailable is also a problem.
pub fn light_check(
    label: &'static str,
    on: Option<bool>,
    found: std::result::Result<String, String>,
) -> (String, Option<Problem>) {
    let switch = match on {
        Some(true) => "on in Status light patterns",
        Some(false) => "off in Status light patterns",
        None => "switch unknown: the config didn't load",
    };
    let line = |text: &str| format!("{text} · {switch}");
    match found {
        Ok(text) => (line(&text), None),
        Err(text) => (
            line(&text),
            (on == Some(true)).then(|| light_problem(label, text)),
        ),
    }
}
/// v7.7 ticket 192: a light switched on in Status light patterns that isn't available.
pub fn light_problem(label: &'static str, message: String) -> Problem {
    Problem {
        stage: label,
        message,
        fix: Fix::Show(format!(
            "{label} is switched on in Status light patterns but isn't available. Fix the cause above, or switch it off there."
        )),
    }
}
/// `/dev/uinput` for [`problems`]: `None` when it opens for writing, else whether it exists.
pub fn uinput_state() -> Option<bool> {
    match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/uinput")
    {
        Ok(_) => None,
        Err(e) => Some(e.kind() != std::io::ErrorKind::NotFound),
    }
}
/// Whether `program` is a file in a PATH folder (looked up, never run).
pub fn on_path(program: &str) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|p| p.join(program).is_file())
}
/// The gdbus call that asks whether the window extension answers (run through a runner).
pub const EXTENSION_PROBE: [&str; 10] = [
    "gdbus",
    "call",
    "--session",
    "--dest",
    "org.freedesktop.DBus",
    "--object-path",
    "/org/freedesktop/DBus",
    "--method",
    "org.freedesktop.DBus.NameHasOwner",
    "org.vibeconsole.Windows",
];

/// `doctor`'s login-service line (v7 ticket 01). Nothing in VibeConsole disables the unit
/// on its own; `startup-disable` (e.g. a test guide's cleanup) does, so doctor says so.
fn login_doctor(installed: bool, mut systemctl: impl FnMut(&[&str]) -> Result<()>) -> String {
    if !installed {
        "Login service: not installed (vibeconsole startup-enable starts VibeConsole at login)"
            .into()
    } else if systemctl(&["is-enabled", "--quiet", UNIT]).is_ok() {
        "Login service: enabled (starts at login)".into()
    } else {
        "Login service: WARNING installed but disabled, so VibeConsole won't start at login; turn it on with: vibeconsole startup-enable".into()
    }
}
/// v7.7 ticket 224: the service's run state from the file it writes on each change; read
/// only. A file that can't be read says why, never guessed.
fn run_state_line(file: std::result::Result<&Path, String>) -> String {
    let read =
        file.and_then(|f| fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display())));
    match read.as_deref().map(str::trim) {
        Ok(state) if !state.is_empty() => format!("Run state: {state}"),
        Ok(_) => "Run state: run state unavailable: the service wrote an empty state".into(),
        Err(why) => format!("Run state: run state unavailable: {why}"),
    }
}
/// v7 ticket 95: what `systemctl --user status`'s exit code means. 0 (running) and 3 (not
/// running) are answers, already printed by systemctl; anything else (4: no such unit) or a
/// kill by a signal is an error naming its stage.
fn status_outcome(code: Option<i32>) -> Result<()> {
    match code {
        Some(0 | 3) => Ok(()),
        Some(code) => Err(format!(
            "startup-status: systemctl --user status {UNIT} failed (exit status {code})"
        )
        .into()),
        None => Err("startup-status: systemctl was stopped by a signal".into()),
    }
}
/// Setup and login-service commands `main` hands to `command`.
pub const COMMANDS: [&str; 8] = [
    "install",
    "uninstall",
    "doctor",
    "startup-enable",
    "startup-start",
    "startup-stop",
    "startup-status",
    "startup-disable",
];
pub fn command(command: &str, flags: &[String]) -> Result<()> {
    if flags
        .iter()
        .any(|s| s != "--feedback" && s != "--start-flow")
        || (!flags.is_empty() && command != "startup-enable")
    {
        return Err("flags are supported only by startup-enable: --feedback, --start-flow".into());
    }
    let binary = binary()?;
    match command {
        "install" => {
            let _owner = Owner::acquire("installer")?;
            fs::create_dir_all(binary.parent().unwrap())?;
            let source = std::env::current_exe()?;
            if source != binary {
                let temp = binary.with_extension("new");
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o755)
                    .open(&temp)?;
                let result = (|| -> Result<()> {
                    std::io::copy(&mut File::open(source)?, &mut output)?;
                    output.sync_all()?;
                    fs::set_permissions(&temp, fs::Permissions::from_mode(0o755))?;
                    fs::rename(&temp, &binary)?;
                    Ok(())
                })();
                if result.is_err() {
                    let _ = fs::remove_file(&temp);
                }
                result?;
            }
            println!(
                "Installed {}. Installation does not enable/start login startup. Add ~/.local/bin to PATH if needed.",
                binary.display()
            );
            if refresh_installed_unit(&unit()?, systemctl)? {
                println!("Login service unit updated; enabled/disabled is unchanged.");
            }
            println!("{}", login_doctor(unit()?.is_file(), systemctl));
            let extension = extension_dir()?;
            fs::create_dir_all(&extension)?;
            for (name, text) in EXTENSION_FILES {
                fs::write(extension.join(name), text)?;
            }
            println!(
                "Window control extension installed in {}. Launcher pads minimize/close only once it is enabled: log out and in once (GNOME Wayland loads new extensions at login), then run: gnome-extensions enable {EXTENSION_UUID}",
                extension.display()
            );
        }
        "startup-enable" => {
            login(Login::Enable)?;
            println!(
                "Login startup enabled. Start explicitly with vibeconsole startup-start; journalctl --user -u vibeconsole.service shows program, Flow, and output status."
            );
            if !flags.is_empty() {
                println!(
                    "Legacy startup-enable flags ignored; saved Flow setting and verified program now control startup."
                );
            }
        }
        "startup-start" => systemctl(&["start", "vibeconsole.service"])?,
        "startup-stop" => systemctl(&["stop", "vibeconsole.service"])?,
        "startup-status" => {
            let status = Command::new("systemctl")
                .args(["--user", "status", UNIT, "--no-pager"])
                .status()
                .map_err(|e| format!("startup-status: systemctl unavailable: {e}"))?;
            status_outcome(status.code())?;
            // v7.7 ticket 224: whether output runs or is paused, as the service reports it.
            if status.code() == Some(0) {
                let file = crate::status::state_dir().map(|d| d.join(crate::terminal::RUN_STATE));
                println!(
                    "{}",
                    run_state_line(file.as_deref().map_err(|e| e.to_string()))
                );
            }
        }
        "startup-disable" => {
            systemctl(&["disable", "--now", "vibeconsole.service"])?;
            println!(
                "Login startup disabled: VibeConsole won't start at login. Turn it back on with: vibeconsole startup-enable"
            );
        }
        "uninstall" => {
            if unit()?.exists() {
                managed(&unit()?)?;
                systemctl(&["disable", "--now", "vibeconsole.service"])?;
                fs::remove_file(unit()?)?;
                systemctl(&["daemon-reload"])?;
            }
            let _owner = Owner::acquire("installer")?;
            if binary.exists() {
                fs::remove_file(&binary)?;
            }
            let extension = extension_dir()?;
            if extension.exists() {
                fs::remove_dir_all(&extension)?;
            }
            println!(
                "Removed VibeConsole executable/service and window extension; saved mappings preserved."
            );
        }
        "doctor" => {
            println!("{}", crate::version());
            println!(
                "Session: {} / {}",
                std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
                std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default()
            );
            match OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open("/dev/uinput")
            {
                Ok(_) => println!("uinput read/write: ready (no keyboard created)"),
                Err(e) => println!(
                    "uinput permission/device failure: {e}; see README permission commands"
                ),
            }
            match crate::input_port() {
                Ok(p) => println!("MIDI identity: {p} (no input opened)"),
                Err(e) => println!("MIDI unavailable: {e}"),
            }
            match crate::gamepad::Pad::open() {
                Ok(pad) => match pad.connections().as_slice() {
                    [] => println!("Xbox controllers: none connected (USB or Bluetooth)"),
                    names => println!("Xbox controllers: {}", names.join(", ")),
                },
                Err(e) => println!("Xbox controllers: {e}"),
            }
            println!("{}", crate::gamepad::headset());
            // v7.7 ticket 214: the login service's note that Flow can't hear its keyboard.
            let flow = crate::status::state_dir().and_then(|d| {
                Ok(std::fs::read_to_string(
                    d.join(crate::terminal::FLOW_MARKER),
                )?)
            });
            if let Ok(text) = flow {
                println!("Flow: {}", text.trim());
            }
            // v7.7 ticket 192: each light's probe, then its switch in Status light patterns.
            let mut found: Vec<std::result::Result<String, String>> = Vec::new();
            found.push(
                crate::gamepad::ButtonLight::open(Path::new(crate::gamepad::LEDS))
                    .map(|light| light.describe()),
            );
            // v7 ticket 113: the keyboard's lock-key LED (opened only, nothing sent) and RGB.
            found.push(match crate::keyboard::LockLights::open() {
                Ok(lights) => Ok(format!(
                    "Keyboard light: available ({})",
                    lights.names().join(", ")
                )),
                Err(e) => Err(format!("Keyboard light: {e}")),
            });
            // v7 ticket 128: a Razer keyboard's M and G lights, and RGB.
            let razer = std::path::Path::new(crate::keyboard::RAZER);
            found.push(match crate::keyboard::macro_leds(razer).as_slice() {
                [] => Err(
                    "Keyboard M light: unavailable: OpenRazer's razerkbd driver isn't loaded, or its macro_led_state isn't writable (VibeConsole never installs it)".into()
                ),
                leds => Ok(format!(
                    "Keyboard M light: available ({})",
                    leds.iter()
                        .map(|l| l.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            });
            // v7 ticket 128: opened only (brightness read), nothing sent.
            found.push(
                crate::keyboard::RgbKeyboards::open(razer, crate::status::Rgb::SPECTRUM)
                    .map(|rgb| format!(
                        "RGB keyboard: available ({}; on pause and quit it goes back to the resting effect you pick in Status light patterns)",
                        rgb.names().join(", ")
                    ))
                    .map_err(|e| e.to_string()),
            );
            // v7.7 ticket 189: the Razer speakers and mousepad, opened only, nothing sent: each
            // device and the effects it can show.
            let accessory = std::path::Path::new(crate::keyboard::RAZER_ACCESSORY);
            for (_, label, model) in crate::status::ACCESSORIES {
                found.push(
                    crate::keyboard::RgbKeyboards::open_accessory(
                        accessory,
                        crate::status::Rgb::SPECTRUM,
                        label,
                        model,
                    )
                    .map(|rgb| {
                        format!(
                            "{label}: available ({}; its brightness is left as it is)",
                            rgb.abilities().join("; ")
                        )
                    })
                    .map_err(|e| e.to_string()),
                );
            }
            // v7.7 ticket 190: the G502 logo through logirgb; only `logirgb profiles` runs.
            found.push(
                crate::keyboard::MouseLight::open(crate::keyboard::logirgb_runner())
                    .map(|_| "Mouse light (Logitech G502): available (logo: off, static, breathe, spectrum as cycle; a state is a logirgb preview, never stored in the mouse)".into())
                    .map_err(|e| e.to_string()),
            );
            let lights = crate::mappings::load_config(&crate::mappings::config_path()?)
                .ok()
                .map(|c| light_switches(c.status_lights, &c.device_lights));
            let mut light_problems = Vec::new();
            for (index, found) in found.into_iter().enumerate() {
                let (label, on) = match &lights {
                    Some(switches) => (switches[index].0, Some(switches[index].1)),
                    None => (light_switches(false, &Default::default())[index].0, None),
                };
                let (line, problem) = light_check(label, on, found);
                println!("{line}");
                light_problems.extend(problem);
                if label == "Keyboard M light" {
                    println!(
                        "Keyboard G light: unavailable: lighting it turns on Razer game mode, which turns off the Super key"
                    );
                }
            }
            for problem in &light_problems {
                println!("Problem · {}: {}", problem.stage, problem.message);
            }
            match crate::recording::permissions() {
                Ok(n) => {
                    println!("Physical-key recording: {n} readable keyboard(s); no events captured")
                }
                Err(e) => println!("Recording unavailable: {e}; Select keys remains available"),
            }
            for program in ["gio", "wpctl", "systemctl", "tmux", "wl-copy"] {
                let present = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                    .any(|p| p.join(program).is_file());
                println!(
                    "{program}: {}",
                    if present {
                        "available"
                    } else {
                        "missing prerequisite"
                    }
                );
            }
            println!(
                "Applications: {} visible installed entries (none launched)",
                crate::actions::catalog()?.len()
            );
            // v7.7 ticket 197: each config file and whether it loaded (reads only).
            for line in vibeconsole_core::config::report(&crate::mappings::config_path()?) {
                println!("{line}");
            }
            println!("{}", login_doctor(unit()?.is_file(), systemctl));
            // v7.7 ticket 203: where status lines go (checked, nothing written).
            println!(
                "{}",
                if Path::new(JOURNAL).exists() {
                    "Status log: the login service and a terminal run write their status changes to the journal: journalctl --user -t vibeconsole"
                } else {
                    "Status log: unavailable: no journal (no /run/systemd/journal/socket); a terminal run shows its status on screen only"
                }
            );
            // v7.7 ticket 208: whether a terminal run can tell its own terminal is focused.
            println!(
                "{}",
                match crate::actions::focused_title(crate::actions::gdbus_output) {
                    Ok(_) => "Own terminal focus: available (window extension); Shift, Ctrl, Alt or Super alone are held back while VibeConsole's terminal is focused".to_owned(),
                    Err(e) => format!("{e}; Shift, Ctrl, Alt or Super alone stay silent in menus. After vibeconsole install, log out and in once"),
                }
            );
            println!("Agent terminals: tmux attach -t vibe");
            for codex in [false, true] {
                println!("{}", crate::status::hooks_status(codex));
            }
            let answering = Command::new("gdbus")
                .args([
                    "call",
                    "--session",
                    "--dest",
                    "org.freedesktop.DBus",
                    "--object-path",
                    "/org/freedesktop/DBus",
                    "--method",
                    "org.freedesktop.DBus.NameHasOwner",
                    "org.vibeconsole.Windows",
                ])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("true"));
            println!(
                "Window control extension: {}",
                if answering {
                    "answering (launcher minimize/close available)".to_string()
                } else {
                    format!(
                        "not answering; after vibeconsole install, log out/in and run gnome-extensions enable {EXTENSION_UUID}"
                    )
                }
            );
        }
        _ => return Err("unknown setup command".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// v7.7 ticket 187: the same probes for both modes; a program missing from the
    /// service's PATH is a differing row with its cause and how it should be; the by-design
    /// rows are there; reading the service's environment parses `show-environment`.
    #[test]
    fn compare_modes_names_each_difference_and_its_cause() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("vibeconsole-modes-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (both, extra) = (dir.join("both"), dir.join("extra"));
        for (d, names) in [
            (&both, &["tmux", "gio", "claude"][..]),
            (&extra, &["herdr"][..]),
        ] {
            fs::create_dir_all(d).unwrap();
            for n in names {
                fs::write(d.join(n), "").unwrap();
                fs::set_permissions(d.join(n), fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let terminal = ModeEnv {
            path: format!("{}:{}", both.display(), extra.display()),
            vars: vec![
                ("HOME".into(), "/h".into()),
                ("WAYLAND_DISPLAY".into(), "wayland-0".into()),
            ],
        };
        let service = ModeEnv::parse(&format!("HOME=/h\nPATH={}\n", both.display()));
        assert_eq!(service.path, both.display().to_string());
        let rows = compare_modes(&terminal, &service, ("Claude Code", "claude"));
        let row = |f: &str| {
            rows.iter()
                .find(|r| r.feature.starts_with(f))
                .unwrap_or_else(|| panic!("{f}"))
        };
        let herdr = row("herdr ");
        assert_eq!(
            (herdr.terminal.as_str(), herdr.service.as_str()),
            ("found", "not on PATH")
        );
        assert!(herdr.why.contains("PATH") && herdr.should.contains("environment.d"));
        assert!(!row("tmux ").differs() && !row("claude ").differs());
        assert!(row("WAYLAND_DISPLAY").differs());
        assert!(row("Screen").differs() && row("Piano").differs());
        assert!(!row("Akai agent status lights").differs());
        let text = compare_text(&rows);
        assert!(text.contains("≠ herdr (herdr agents and their lights)\n    terminal: found · service: not on PATH\n    why: "));
        fs::remove_dir_all(dir).unwrap();
    }
    /// v7.7 ticket 158: Install /skills writes the missing files into a home, backs up a
    /// differing one before replacing it, deletes nothing, and reports installed only when
    /// every file is there and equal; the embedded list is the repo's `.claude/skills`.
    #[test]
    fn install_skills_writes_missing_backs_up_differing_and_deletes_nothing() {
        let home = std::env::temp_dir().join(format!("vibeconsole-skills-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let skills = home.join(".claude/skills");
        fs::create_dir_all(skills.join("vibeconsole")).unwrap();
        fs::write(skills.join("vibeconsole/SKILL.md"), "my own edit").unwrap();
        fs::create_dir_all(skills.join("mine")).unwrap();
        fs::write(skills.join("mine/SKILL.md"), "someone else's skill").unwrap();
        assert!(!skills_installed(&home));
        let note = install_skills(&home).unwrap();
        assert!(
            note.contains("29 files written") && note.contains("backed up first"),
            "{note}"
        );
        assert!(skills_installed(&home));
        let backups = fs::read_dir(skills.join("vibeconsole"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains(".bak-vibeconsole-")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            fs::read_to_string(backups[0].path()).unwrap(),
            "my own edit"
        );
        assert_eq!(
            fs::read_to_string(skills.join("mine/SKILL.md")).unwrap(),
            "someone else's skill",
            "nothing else touched"
        );
        assert!(install_skills(&home).unwrap().contains("already installed"));
        fs::remove_dir_all(&home).unwrap();

        // The embedded list is every vibeconsole* file in the repo's .claude/skills.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(".claude/skills");
        let mut on_disk = Vec::new();
        let mut folders = vec![root.clone()];
        while let Some(folder) = folders.pop() {
            for entry in fs::read_dir(folder).unwrap().filter_map(|e| e.ok()) {
                let path = entry.path();
                let rel = path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if !rel.starts_with("vibeconsole") {
                    continue;
                }
                if path.is_dir() {
                    folders.push(path);
                } else {
                    on_disk.push(rel);
                }
            }
        }
        on_disk.sort();
        let mut embedded = SKILLS
            .iter()
            .map(|(p, _)| p.to_string())
            .collect::<Vec<_>>();
        embedded.sort();
        assert_eq!(embedded, on_disk);
    }
    /// Ticket 20: a running service is paused for the session and started again on quit.
    #[test]
    fn session_takes_over_a_running_service_and_restarts_it() {
        use std::{cell::RefCell, rc::Rc};
        fn run(
            free: bool,
            active: bool,
            stop_ok: bool,
            flag: &'static AtomicBool,
        ) -> (Result<()>, Vec<String>) {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let log = calls.clone();
            let tries = RefCell::new(0);
            let acquire = || {
                *tries.borrow_mut() += 1;
                if free || *tries.borrow() > 1 {
                    Ok(())
                } else {
                    Err("locked".into())
                }
            };
            let systemctl = move |args: &[&str]| -> Result<()> {
                log.borrow_mut().push(args[0].to_string());
                match args[0] {
                    "is-active" if !active => Err("inactive".into()),
                    "stop" if !stop_ok => Err("stop failed".into()),
                    _ => Ok(()),
                }
            };
            let result = take_over(acquire, systemctl, flag).map(drop);
            let calls = calls.borrow().clone();
            (result, calls)
        }
        static A: AtomicBool = AtomicBool::new(false);
        let (result, calls) = run(false, true, true, &A);
        assert!(result.is_ok());
        assert_eq!(calls, ["is-active", "stop", "start"]);
        static B: AtomicBool = AtomicBool::new(false);
        let (result, calls) = run(false, false, true, &B);
        assert_eq!(result.unwrap_err().to_string(), "locked");
        assert_eq!(calls, ["is-active"]);
        static C: AtomicBool = AtomicBool::new(false);
        let (result, calls) = run(false, true, false, &C);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no session opened")
        );
        assert_eq!(calls, ["is-active", "stop"]);
        static D: AtomicBool = AtomicBool::new(false);
        let (result, calls) = run(true, true, true, &D);
        assert!(result.is_ok() && calls.is_empty());
        // Ticket 21: Settings asked for the service on quit, with no handover.
        static E: AtomicBool = AtomicBool::new(true);
        let (_, calls) = run(true, false, true, &E);
        assert_eq!(calls, ["start"]);
    }

    /// v7.7 ticket 192: doctor's light lines say whether each light is on or off as saved
    /// in Status light patterns (the master switch off turns every one off); a light switched
    /// on that its probe found unavailable is a problem named by its stage, one switched off
    /// is not.
    #[test]
    fn doctor_light_lines_say_on_or_off_as_saved() {
        use crate::mappings;
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-doctor-lights-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mappings.tsv");
        mappings::save(&path, &[]).unwrap();
        let mut config = mappings::load_config(&path).unwrap();
        config.status_lights = true;
        config.device_lights.on[1] = true; // Xbox
        config.device_lights.keyboard_on = false;
        config.device_lights.rgb_devices[0].on = true; // RGB keyboard
        config.device_lights.rgb_devices[1].on = false; // speakers
        config.device_lights.rgb_devices[3].on = true; // mouse
        mappings::save_config(&path, &config).unwrap();
        let saved = mappings::load_config(&path).unwrap();
        let switches = light_switches(saved.status_lights, &saved.device_lights);
        let on = switches
            .iter()
            .filter(|(_, on)| *on)
            .map(|(l, _)| *l)
            .collect::<Vec<_>>();
        assert_eq!(
            on,
            [
                "Xbox button light",
                "RGB keyboard",
                "Mouse light (Logitech G502)"
            ]
        );
        let lines = switches.map(|(label, on)| {
            let found = if label.starts_with("Mouse") || label.starts_with("Speakers") {
                Err(format!("{label}: unavailable: logirgb missing"))
            } else {
                Ok(format!("{label}: available"))
            };
            light_check(label, Some(on), found)
        });
        assert_eq!(
            lines[0].0,
            "Xbox button light: available · on in Status light patterns"
        );
        assert_eq!(
            lines[1].0,
            "Keyboard light: available · off in Status light patterns"
        );
        assert_eq!(
            lines[4].0,
            "Speakers RGB: unavailable: logirgb missing · off in Status light patterns"
        );
        let problems = lines
            .iter()
            .filter_map(|(_, p)| p.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(problems.len(), 1, "only the switched-on one: {problems:?}");
        assert_eq!(problems[0].stage, "Mouse light (Logitech G502)");
        assert!(matches!(&problems[0].fix, Fix::Show(t) if t.contains("switch it off")));
        // The master Status lights switch off: every light is off.
        let off = light_switches(false, &saved.device_lights);
        assert!(off.iter().all(|(_, on)| !on));
        // No config: the line says so, and nothing is a problem.
        let (line, problem) = light_check("RGB keyboard", None, Err("RGB keyboard: x".into()));
        assert!(line.ends_with("switch unknown: the config didn't load") && problem.is_none());
        fs::remove_dir_all(dir).unwrap();
    }
    /// v7.7 ticket 224: startup-status shows the run state the service wrote, and says why
    /// when it can't be read; it only reads.
    #[test]
    fn startup_status_reads_the_run_state_or_says_why_not() {
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-startup-status-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(crate::terminal::RUN_STATE);
        fs::write(&file, "running (Program 1 verified)\n").unwrap();
        assert_eq!(
            run_state_line(Ok(&file)),
            "Run state: running (Program 1 verified)"
        );
        fs::write(
            &file,
            "paused: Flow work cancelled; output resumes once it has stopped\n",
        )
        .unwrap();
        assert_eq!(
            run_state_line(Ok(&file)),
            "Run state: paused: Flow work cancelled; output resumes once it has stopped"
        );
        fs::write(&file, "").unwrap();
        assert!(run_state_line(Ok(&file)).ends_with("the service wrote an empty state"));
        fs::remove_file(&file).unwrap();
        let missing = run_state_line(Ok(&file));
        assert!(
            missing.starts_with("Run state: run state unavailable: ")
                && missing.contains("run-state"),
            "{missing}"
        );
        assert_eq!(
            run_state_line(Err("XDG_RUNTIME_DIR is not set".into())),
            "Run state: run state unavailable: XDG_RUNTIME_DIR is not set"
        );
        fs::remove_dir_all(dir).unwrap();
    }
    /// v7 ticket 01: doctor warns when the unit is installed but won't start at login.
    #[test]
    fn doctor_warns_when_login_service_is_installed_but_disabled() {
        let line = |installed, enabled: bool| {
            login_doctor(installed, |args: &[&str]| {
                assert_eq!(args, ["is-enabled", "--quiet", UNIT]);
                if enabled {
                    Ok(())
                } else {
                    Err("disabled".into())
                }
            })
        };
        let off = line(true, false);
        assert!(off.starts_with("Login service: WARNING"), "{off}");
        assert!(off.contains("vibeconsole startup-enable"), "{off}");
        assert_eq!(line(true, true), "Login service: enabled (starts at login)");
        let missing = line(false, false);
        assert!(!missing.contains("WARNING"), "{missing}");
    }

    #[test]
    fn one_owner_and_opt_in_unit_do_not_mutate_desktop() {
        let path = std::env::temp_dir().join(format!("vibeconsole-owner-{}", std::process::id()));
        // v7.7 ticket 252: a test name, never the real per-user lock.
        let name = format!("vibeconsole-test-runtime-{}", std::process::id());
        let owner = Owner::at_named(&path, &name, "test").unwrap();
        assert!(Owner::at_named(&path, &name, "test").is_err());
        drop(owner);
        drop(Owner::at_named(&path, &name, "test").unwrap());
        fs::remove_file(path).unwrap();
        assert!(service().contains("\" daemon\n"));
        assert!(!service().contains("--feedback") && !service().contains("--start-flow"));
        assert!(service().contains("Restart=no"));
        assert!(service().contains("KillMode=mixed"));
        // The installed extension's directory name must match its metadata uuid.
        assert!(
            EXTENSION_FILES[0]
                .1
                .contains(&format!("\"uuid\": \"{EXTENSION_UUID}\""))
        );
        assert!(EXTENSION_FILES[1].1.contains("org.vibeconsole.Windows"));
    }

    /// v8 ticket 117: the unit starts after the desktop session is up, and reinstalling
    /// rewrites an old managed unit without enabling or disabling it.
    #[test]
    fn reinstall_updates_the_unit_to_wait_for_the_desktop_and_keeps_enabled_state() {
        use std::cell::RefCell;
        assert!(service().contains("\nAfter=graphical-session.target "));
        assert!(!service().contains("graphical-session-pre"));
        assert!(service().contains("\nPartOf=graphical-session.target\n"));
        assert!(service().contains("\nWantedBy=graphical-session.target\n"));
        let dir = std::env::temp_dir().join(format!("vibeconsole-unit-{}", std::process::id()));
        let path = dir.join("systemd/user/vibeconsole.service");
        let calls = RefCell::new(Vec::<String>::new());
        let runner = |args: &[&str]| -> Result<()> {
            calls.borrow_mut().push(args.join(" "));
            Ok(())
        };
        // Not installed: nothing is written.
        assert!(!refresh_installed_unit(&path, runner).unwrap());
        assert!(!path.exists());
        // An old managed unit is rewritten; only daemon-reload runs.
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old = service().replace(
            "After=graphical-session.target",
            "After=graphical-session-pre.target",
        );
        fs::write(&path, &old).unwrap();
        assert!(refresh_installed_unit(&path, runner).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), service());
        assert_eq!(*calls.borrow(), ["daemon-reload"]);
        // A hand-written unit is left alone.
        fs::write(&path, "[Unit]\n").unwrap();
        assert!(!refresh_installed_unit(&path, runner).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "[Unit]\n");
        assert_eq!(calls.borrow().len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    /// v7 ticket 95: a stopped service is an answer, not an error; a missing unit or a
    /// killed systemctl is an error naming its stage.
    #[test]
    fn startup_status_reports_a_stopped_service_without_an_error() {
        assert!(status_outcome(Some(0)).is_ok(), "running");
        assert!(status_outcome(Some(3)).is_ok(), "installed but not running");
        let missing = status_outcome(Some(4)).unwrap_err().to_string();
        assert!(
            missing.starts_with("startup-status: ") && missing.contains("exit status 4"),
            "{missing}"
        );
        assert!(
            status_outcome(None)
                .unwrap_err()
                .to_string()
                .starts_with("startup-status: ")
        );
    }
}
