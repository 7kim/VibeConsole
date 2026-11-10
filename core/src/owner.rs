//! One VibeConsole owns the controllers, the keyboard and the config at a time (v7 ticket
//! 10). Both editions take this lock; the holder writes who it is, so a second copy can
//! say which one to stop.
use crate::Result;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

pub struct Owner {
    file: File,
}
impl Drop for Owner {
    /// Clear the name while the lock is still held, so the next refusal never names a
    /// holder that is gone.
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
    }
}
impl Owner {
    /// `who` describes this copy, e.g. "terminal session" or "login service".
    pub fn at(path: &Path, who: &str) -> Result<Self> {
        let mut file = OpenOptions::new()
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
            let mut holder = String::new();
            let _ = file.read_to_string(&mut holder);
            // Trust the recorded holder only while its process is alive (a killed owner
            // can't clear it).
            let holder = holder
                .lines()
                .next()
                .filter(|l| !l.is_empty() && !l.chars().any(char::is_control))
                .filter(|l| {
                    l.rsplit_once("(pid ")
                        .and_then(|(_, pid)| pid.strip_suffix(')'))
                        .is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists())
                })
                .unwrap_or("another VibeConsole");
            return Err(format!("Startup: {holder} owns MIDI/keyboard/configuration; keep it for editing, or stop it first (vibeconsole startup-stop for the login service). No keyboard was replaced").into());
        }
        file.set_len(0)?;
        file.rewind()?;
        writeln!(file, "the {who} (pid {})", std::process::id())?;
        Ok(Self { file })
    }
    pub fn acquire(who: &str) -> Result<Self> {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return Err("run VibeConsole as your desktop user, not root".into());
        }
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or("XDG_RUNTIME_DIR is unavailable; run in your desktop session")?;
        Self::at(&dir.join("vibeconsole.lock"), who)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_copy_names_the_owner_and_starts_once_it_is_gone() {
        let path = std::env::temp_dir().join(format!("vibeconsole-owner-{}", std::process::id()));
        let owner = Owner::at(&path, "login service").unwrap();
        let refused = Owner::at(&path, "terminal session")
            .err()
            .unwrap()
            .to_string();
        assert!(
            refused.starts_with("Startup: the login service (pid "),
            "{refused}"
        );
        assert!(refused.contains("No keyboard was replaced"));
        drop(owner);
        // A child another test thread is spawning shares the lock's open file until it
        // execs, so the release can lag by a few milliseconds.
        let again = (0..40).find_map(|_| {
            Owner::at(&path, "terminal session").ok().or_else(|| {
                std::thread::sleep(std::time::Duration::from_millis(25));
                None
            })
        });
        assert!(
            again.is_some(),
            "the lock comes free once its owner is gone"
        );
        std::fs::remove_file(path).unwrap();
    }
}
