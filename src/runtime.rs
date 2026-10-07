use crate::Result;
use std::{
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::Command,
};

pub struct Owner {
    _file: File,
}
impl Owner {
    fn at(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        // SAFETY: borrowed descriptor; flock releases automatically when the owner closes it.
        if file.metadata()?.uid() != unsafe { libc::geteuid() } {
            return Err(
                "runtime lock belongs to another user; check XDG_RUNTIME_DIR permissions".into(),
            );
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("another VibeConsole session owns MIDI/keyboard/configuration; keep that session for editing, or explicitly stop it first (vibeconsole startup-stop). No keyboard was replaced".into());
        }
        Ok(Self { _file: file })
    }
    pub fn acquire() -> Result<Self> {
        if unsafe { libc::geteuid() } == 0 {
            return Err("run VibeConsole as your desktop user, not root".into());
        }
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or("XDG_RUNTIME_DIR is unavailable; run in your desktop session")?;
        Self::at(&dir.join("vibeconsole.lock"))
    }
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
fn service() -> &'static str {
    "# Managed by VibeConsole\n[Unit]\nDescription=VibeConsole controller shortcuts\nAfter=graphical-session-pre.target pipewire.service wireplumber.service\nPartOf=graphical-session.target\n\n[Service]\nType=simple\nExecStart=\"%h/.local/bin/vibeconsole\" daemon\nRestart=no\n# amidi must outlive SIGTERM so handled shutdown can restore controller feedback\nKillMode=mixed\nTimeoutStopSec=15\n\n[Install]\nWantedBy=graphical-session.target\n"
}
fn managed(path: &Path) -> Result<()> {
    if path.exists() && !fs::read_to_string(path)?.starts_with("# Managed by VibeConsole\n") {
        return Err(format!("refusing to replace/remove unmanaged {}", path.display()).into());
    }
    Ok(())
}
fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()?;
    if !status.success() {
        return Err(format!("systemctl --user {} failed ({status})", args.join(" ")).into());
    }
    Ok(())
}
pub fn command(command: &str, flags: &[String]) -> Result<()> {
    if flags
        .iter()
        .any(|s| s != "--feedback" && s != "--start-flow")
        || (!flags.is_empty() && command != "startup-enable")
    {
        return Err("flags are supported only by startup-enable: --feedback, --start-flow".into());
    }
    let binary = home()?.join(".local/bin/vibeconsole");
    match command {
        "install" => {
            let _owner = Owner::acquire()?;
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
        }
        "startup-enable" => {
            if !binary.is_file() {
                return Err("run vibeconsole install first".into());
            }
            let path = unit()?;
            managed(&path)?;
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(&path, service())?;
            systemctl(&["daemon-reload"])?;
            systemctl(&["enable", "vibeconsole.service"])?;
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
        "startup-status" => systemctl(&["status", "vibeconsole.service", "--no-pager"])?,
        "startup-disable" => systemctl(&["disable", "--now", "vibeconsole.service"])?,
        "uninstall" => {
            if unit()?.exists() {
                managed(&unit()?)?;
                systemctl(&["disable", "--now", "vibeconsole.service"])?;
                fs::remove_file(unit()?)?;
                systemctl(&["daemon-reload"])?;
            }
            let _owner = Owner::acquire()?;
            if binary.exists() {
                fs::remove_file(&binary)?;
            }
            println!("Removed VibeConsole executable/service; saved mappings preserved.");
        }
        "doctor" => {
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
            println!("Agent terminals: tmux attach -t vibe");
        }
        _ => return Err("unknown setup command".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_owner_and_opt_in_unit_do_not_mutate_desktop() {
        let path = std::env::temp_dir().join(format!("vibeconsole-owner-{}", std::process::id()));
        let owner = Owner::at(&path).unwrap();
        assert!(Owner::at(&path).is_err());
        drop(owner);
        drop(Owner::at(&path).unwrap());
        fs::remove_file(path).unwrap();
        assert!(service().contains("\" daemon\n"));
        assert!(!service().contains("--feedback") && !service().contains("--start-flow"));
        assert!(service().contains("Restart=no"));
        assert!(service().contains("KillMode=mixed"));
    }
}
