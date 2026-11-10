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
    "# Managed by VibeConsole\n[Unit]\nDescription=VibeConsole controller shortcuts\nAfter=graphical-session-pre.target pipewire.service wireplumber.service\nPartOf=graphical-session.target\n\n[Service]\nType=simple\nExecStart=\"%h/.local/bin/vibeconsole\" daemon\nRestart=no\n# SIGTERM goes to the main process only, so its handled shutdown restores controller feedback\nKillMode=mixed\nTimeoutStopSec=15\n\n[Install]\nWantedBy=graphical-session.target\n"
}
fn managed(path: &Path) -> Result<()> {
    if path.exists() && !fs::read_to_string(path)?.starts_with("# Managed by VibeConsole\n") {
        return Err(format!("refusing to replace/remove unmanaged {}", path.display()).into());
    }
    Ok(())
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
    let path = unit()?;
    managed(&path)?;
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, service())?;
    systemctl(&["daemon-reload"])
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
                .status()?;
            if !status.success() {
                return Err(format!("systemctl --user status {UNIT} failed ({status})").into());
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
                Ok(pad) => match pad.connected().as_slice() {
                    [] => println!("Xbox controllers: none connected (USB)"),
                    names => println!("Xbox controllers: {}", names.join(", ")),
                },
                Err(e) => println!("Xbox controllers: {e}"),
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
            println!(
                "Configuration: {} valid mappings",
                crate::mappings::load(&crate::mappings::config_path()?)?.len()
            );
            println!("{}", login_doctor(unit()?.is_file(), systemctl));
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
        let owner = Owner::at(&path, "test").unwrap();
        assert!(Owner::at(&path, "test").is_err());
        drop(owner);
        drop(Owner::at(&path, "test").unwrap());
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
}
