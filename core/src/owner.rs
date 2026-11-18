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
    /// v7.7 ticket 252: held for the process's life; the kernel frees it on exit, even a crash.
    _user: std::os::unix::net::UnixListener,
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
        Self::at_named(path, &user_lock(), who)
    }
    /// `at`, with the per-user lock's name given (tests use their own, never the real one).
    pub fn at_named(path: &Path, name: &str, who: &str) -> Result<Self> {
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
                        .is_some_and(|pid| {
                            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                                .is_ok_and(|comm| comm.starts_with("vibeconsole"))
                        })
                })
                .unwrap_or("another VibeConsole");
            return Err(format!("Startup: {holder} owns MIDI/keyboard/configuration; keep it for editing, or stop it first (vibeconsole startup-stop for the login service). No keyboard was replaced").into());
        }
        // v7.7 ticket 252: one owner per user, whatever XDG_RUNTIME_DIR says: a copy with
        // its own runtime folder (a sandbox, a script) is refused here, so it never opens the
        // Akai while another VibeConsole owns it.
        let user = bind_user_lock(name)?;
        file.set_len(0)?;
        file.rewind()?;
        writeln!(file, "the {who} (pid {})", std::process::id())?;
        Ok(Self { file, _user: user })
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

/// The per-user lock: an abstract unix socket named from the effective user ID. Abstract
/// names don't live in any folder, so no environment variable moves it.
fn user_lock() -> String {
    // SAFETY: geteuid has no preconditions.
    format!("vibeconsole-owner-{}", unsafe { libc::geteuid() })
}
fn bind_user_lock(name: &str) -> Result<std::os::unix::net::UnixListener> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
    let address = SocketAddr::from_abstract_name(name)?;
    match UnixListener::bind_addr(&address) {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            // Name the holder from the kernel's record of who bound it, never from a file.
            let holder = UnixStream::connect_addr(&address)
                .ok()
                .and_then(|stream| peer_pid(&stream))
                .filter(|pid| {
                    std::fs::read_to_string(format!("/proc/{pid}/comm"))
                        .is_ok_and(|comm| comm.starts_with("vibeconsole"))
                })
                .map_or("another VibeConsole".to_owned(), |pid| {
                    format!("another VibeConsole (pid {pid}, its own runtime folder)")
                });
            Err(format!("Startup: {holder} owns MIDI/keyboard/configuration for this user; stop it first (vibeconsole startup-stop for the login service). No keyboard was replaced").into())
        }
        Err(error) => Err(format!("Startup: per-user lock {name:?}: {error}").into()),
    }
}
/// The pid that bound the other end, from SO_PEERCRED.
fn peer_pid(stream: &std::os::unix::net::UnixStream) -> Option<i32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: a valid socket and a ucred-sized buffer with its length.
    let ok = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    } == 0;
    (ok && cred.pid > 0).then_some(cred.pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_copy_names_the_owner_and_starts_once_it_is_gone() {
        let path = std::env::temp_dir().join(format!("vibeconsole-owner-{}", std::process::id()));
        let name = format!("vibeconsole-test-owner-a-{}", std::process::id());
        let owner = Owner::at_named(&path, &name, "login service").unwrap();
        let refused = Owner::at_named(&path, &name, "terminal session")
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
            Owner::at_named(&path, &name, "terminal session")
                .ok()
                .or_else(|| {
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

    /// v7.7 ticket 252: two copies with different runtime folders (two lock files): the
    /// second is refused by the per-user lock, naming the first's pid; once the first is
    /// gone, the second starts. A test name, never the real per-user lock.
    #[test]
    fn a_copy_with_its_own_runtime_folder_is_refused_too() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let (a, b) = (
            dir.join(format!("vibeconsole-owner-a-{pid}")),
            dir.join(format!("vibeconsole-owner-b-{pid}")),
        );
        let name = format!("vibeconsole-test-owner-b-{pid}");
        let service = Owner::at_named(&a, &name, "login service").unwrap();
        let refused = Owner::at_named(&b, &name, "terminal session")
            .err()
            .unwrap()
            .to_string();
        assert!(
            refused.starts_with("Startup: another VibeConsole"),
            "{refused}"
        );
        assert!(refused.contains("No keyboard was replaced"));
        // The test binary's process name starts with "vibeconsole" too, so it is named.
        assert!(
            refused.contains(&format!("(pid {pid}, its own runtime folder)")),
            "{refused}"
        );
        drop(service);
        assert!(Owner::at_named(&b, &name, "terminal session").is_ok());
        for path in [a, b] {
            std::fs::remove_file(path).unwrap();
        }
    }
}
