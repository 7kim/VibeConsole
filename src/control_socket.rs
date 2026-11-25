use crate::Result;
use std::collections::VecDeque;
use std::fs::{self, DirBuilder, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const LINE: usize = 256;
const CLIENTS: usize = 32;
const ACCEPTS: usize = 64;
const SECOND: Duration = Duration::from_secs(1);

struct Client {
    stream: UnixStream,
    bytes: Vec<u8>,
    expires: Instant,
}

impl Client {
    fn line(&mut self, now: Instant) -> Option<Result<&str>> {
        if now >= self.expires {
            return Some(Err("request timed out".into()));
        }
        let mut buf = [0; LINE + 1];
        let n = match self.stream.read(&mut buf) {
            Ok(0) => return Some(Err("expected a newline-terminated request".into())),
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return None;
            }
            Err(e) => return Some(Err(format!("read request: {e}").into())),
        };
        self.bytes.extend_from_slice(&buf[..n]);
        let Some(end) = self.bytes.iter().position(|&b| b == b'\n') else {
            return (self.bytes.len() > LINE).then(|| Err("request is too long".into()));
        };
        Some(if end > LINE {
            Err("request is too long".into())
        } else if end + 1 != self.bytes.len() {
            Err("expected one request per connection".into())
        } else {
            std::str::from_utf8(&self.bytes[..end])
                .map(|line| line.strip_suffix('\r').unwrap_or(line))
                .map_err(|_| "request must be UTF-8".into())
        })
    }
}

pub struct Server {
    path: PathBuf,
    identity: (u64, u64),
    listener: UnixListener,
    clients: Vec<Client>,
    requests: VecDeque<Instant>,
    /// A bounded drain may need more than one turn. It must finish before any new action.
    refusing: Option<&'static str>,
}

impl Server {
    pub fn bind(dir: &Path) -> Result<Self> {
        let runtime = dir
            .parent()
            .filter(|p| p.is_absolute())
            .ok_or("invalid runtime directory")?;
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let parent = fs::symlink_metadata(runtime)?;
        if !parent.is_dir() || parent.uid() != uid || parent.mode() & 0o077 != 0 {
            return Err("XDG_RUNTIME_DIR must be a private directory owned by this user".into());
        }
        match DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let meta = fs::symlink_metadata(dir)?;
        if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o022 != 0 {
            return Err(
                "VibeConsole runtime directory must be owned and writable only by this user".into(),
            );
        }
        let path = dir.join("control.sock");
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_socket() && meta.uid() == uid => {
                match UnixStream::connect(&path) {
                    Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                        fs::remove_file(&path)?
                    }
                    _ => return Err("control.sock is already in use".into()),
                }
            }
            Ok(_) => return Err("control.sock exists and is not an owned socket".into()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = UnixListener::bind(&path)?;
        let meta = fs::symlink_metadata(&path)?;
        let server = Self {
            path,
            identity: (meta.dev(), meta.ino()),
            listener,
            clients: Vec::new(),
            requests: VecDeque::new(),
            refusing: None,
        };
        fs::set_permissions(&server.path, Permissions::from_mode(0o600))?;
        server.listener.set_nonblocking(true)?;
        Ok(server)
    }

    pub fn refuse(&mut self, reason: &'static str) {
        self.refusing = Some(reason);
        for client in self.clients.drain(..) {
            refuse(client.stream, reason);
        }
        self.drain_refused();
    }

    fn drain_refused(&mut self) {
        let Some(reason) = self.refusing else {
            return;
        };
        for _ in 0..ACCEPTS {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if stream.set_nonblocking(true).is_ok() {
                        refuse(stream, reason);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if e.kind() == io::ErrorKind::WouldBlock {
                        self.refusing = None;
                    }
                    break;
                }
            }
        }
    }

    pub fn poll(&mut self, mut handle: impl FnMut(&str, &Path) -> Result<String>) {
        if self.refusing.is_some() {
            self.drain_refused();
            return;
        }
        let now = Instant::now();
        for _ in 0..ACCEPTS {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    if self.clients.len() == CLIENTS {
                        refuse(stream, "too many open requests");
                        continue;
                    }
                    self.clients.push(Client {
                        stream,
                        bytes: Vec::new(),
                        expires: now + SECOND,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let dir = self.path.parent().expect("bound in a directory");
        let mut pending = Vec::new();
        for mut client in self.clients.drain(..) {
            let now = Instant::now();
            let Some(line) = client.line(now) else {
                pending.push(client);
                continue;
            };
            while self
                .requests
                .front()
                .is_some_and(|&then| now.duration_since(then) >= SECOND)
            {
                self.requests.pop_front();
            }
            let result = if self.requests.len() == 10 {
                Err("rate limit: 10 requests per second".into())
            } else {
                self.requests.push_back(now);
                line.and_then(|line| handle(line, dir))
            };
            reply(client.stream, result);
        }
        self.clients = pending;
    }
}

fn refuse(mut stream: UnixStream, reason: &'static str) {
    // Consume a normal pending line before closing so the client can read the refusal.
    let _ = stream.read(&mut [0; LINE + 1]);
    reply(stream, Err(reason.into()));
}

fn reply(mut stream: UnixStream, result: Result<String>) {
    let response = match result {
        Ok(line) => line,
        Err(e) => format!("error {e}"),
    };
    let mut line: String = response
        .chars()
        .take(512)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    line.push('\n');
    let _ = stream.write_all(line.as_bytes());
}

impl Drop for Server {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|m| (m.dev(), m.ino()) == self.identity) {
            let _ = fs::remove_file(&self.path);
        }
    }
}
