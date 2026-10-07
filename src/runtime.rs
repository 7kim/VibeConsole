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
            return Err("another KeyAI session owns MIDI/keyboard/configuration; keep that session for editing, or explicitly stop it first (keyai startup-stop). No keyboard was replaced".into());
        }
        Ok(Self { _file: file })
    }
    pub fn acquire() -> Result<Self> {
        if unsafe { libc::geteuid() } == 0 {
            return Err("run KeyAI as your desktop user, not root".into());
        }
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or("XDG_RUNTIME_DIR is unavailable; run in your desktop session")?;
        Self::at(&dir.join("keyai.lock"))
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
    Ok(base.join("systemd/user/keyai.service"))
}
fn service(feedback: bool, flow: bool) -> String {
    format!(
        "# Managed by KeyAI\n[Unit]\nDescription=KeyAI controller shortcuts\nAfter=graphical-session-pre.target pipewire.service wireplumber.service\nPartOf=graphical-session.target\n\n[Service]\nType=simple\nExecStart=\"%h/.local/bin/keyai\" daemon{}{}\nRestart=no\n# amidi must outlive SIGTERM so handled shutdown can restore controller feedback\nKillMode=mixed\nTimeoutStopSec=15\n\n[Install]\nWantedBy=graphical-session.target\n",
        if feedback { " --feedback" } else { "" },
        if flow { " --start-flow" } else { "" }
    )
}
fn managed(path: &Path) -> Result<()> {
    if path.exists() && !fs::read_to_string(path)?.starts_with("# Managed by KeyAI\n") {
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
    let binary = home()?.join(".local/bin/keyai");
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
                return Err("run keyai install first".into());
            }
            let path = unit()?;
            managed(&path)?;
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(
                &path,
                service(
                    flags.iter().any(|s| s == "--feedback"),
                    flags.iter().any(|s| s == "--start-flow"),
                ),
            )?;
            systemctl(&["daemon-reload"])?;
            systemctl(&["enable", "keyai.service"])?;
            println!(
                "Login startup enabled. Start explicitly with keyai startup-start; journalctl --user -u keyai.service shows errors. --feedback explicitly confirms the original Program 1 setup; --start-flow opts into ordered Flow startup."
            );
        }
        "startup-start" => systemctl(&["start", "keyai.service"])?,
        "startup-stop" => systemctl(&["stop", "keyai.service"])?,
        "startup-status" => systemctl(&["status", "keyai.service", "--no-pager"])?,
        "startup-disable" => systemctl(&["disable", "--now", "keyai.service"])?,
        "uninstall" => {
            if unit()?.exists() {
                managed(&unit()?)?;
                systemctl(&["disable", "--now", "keyai.service"])?;
                fs::remove_file(unit()?)?;
                systemctl(&["daemon-reload"])?;
            }
            let _owner = Owner::acquire()?;
            if binary.exists() {
                fs::remove_file(&binary)?;
            }
            println!("Removed KeyAI executable/service; saved mappings preserved.");
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
            for program in ["gio", "wpctl", "systemctl"] {
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
        let path = std::env::temp_dir().join(format!("keyai-owner-{}", std::process::id()));
        let owner = Owner::at(&path).unwrap();
        assert!(Owner::at(&path).is_err());
        drop(owner);
        drop(Owner::at(&path).unwrap());
        fs::remove_file(path).unwrap();
        assert!(!service(false, false).contains("--start-flow"));
        assert!(service(true, true).contains("daemon --feedback --start-flow"));
        assert!(service(false, false).contains("Restart=no"));
        assert!(service(false, false).contains("KillMode=mixed"));
    }
}
