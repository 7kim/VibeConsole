use crate::Result;
use std::{
    collections::HashSet,
    fs,
    io::{self, Read},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Shortcut,
    Application(String),
    Volume { up: bool, step: u8 },
    OutputMute,
    MicrophoneMute,
    FlowStart,
    FlowRepair,
}
impl Action {
    pub fn flow(&self) -> bool {
        matches!(self, Self::FlowStart | Self::FlowRepair)
    }
    pub fn audio(&self) -> bool {
        matches!(
            self,
            Self::Volume { .. } | Self::OutputMute | Self::MicrophoneMute
        )
    }
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Application(path)
                if !Path::new(path).is_absolute()
                    || !path.ends_with(".desktop")
                    || path.chars().any(char::is_control)
                    || path.len() > 4096 =>
            {
                Err("invalid installed desktop entry path".into())
            }
            Self::Volume { step, .. } if !(1..=100).contains(step) => {
                Err("volume step must be 1..100 percent".into())
            }
            _ => Ok(()),
        }
    }
    pub fn encode(&self) -> String {
        match self {
            Self::Shortcut => "shortcut".into(),
            Self::Application(path) => format!("app:{path}"),
            Self::Volume { up, step } => {
                format!("volume-{}:{step}", if *up { "up" } else { "down" })
            }
            Self::OutputMute => "output-mute".into(),
            Self::MicrophoneMute => "microphone-mute".into(),
            Self::FlowStart => "flow-start".into(),
            Self::FlowRepair => "flow-repair".into(),
        }
    }
    pub fn parse(value: &str) -> Result<Self> {
        let action = match value {
            "shortcut" => Self::Shortcut,
            "output-mute" => Self::OutputMute,
            "microphone-mute" => Self::MicrophoneMute,
            "flow-start" => Self::FlowStart,
            "flow-repair" => Self::FlowRepair,
            _ if value.starts_with("app:") => Self::Application(value[4..].into()),
            _ if value.starts_with("volume-up:") => Self::Volume {
                up: true,
                step: value[10..].parse()?,
            },
            _ if value.starts_with("volume-down:") => Self::Volume {
                up: false,
                step: value[12..].parse()?,
            },
            _ => return Err("unsupported action".into()),
        };
        action.validate()?;
        Ok(action)
    }
    pub fn description(&self) -> String {
        match self {
            Self::Application(path) => format!(
                "Launch {}",
                Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
            Self::Volume { up, step } => format!(
                "Default output volume {} {step}% (100% limit)",
                if *up { "up" } else { "down" }
            ),
            Self::OutputMute => "Toggle default output mute".into(),
            Self::MicrophoneMute => "Toggle default microphone mute".into(),
            Self::FlowStart => "Start Wispr Flow after keyboard registration".into(),
            Self::FlowRepair => "Repair/reopen Wispr Flow; pause and release first".into(),
            Self::Shortcut => "Keyboard shortcut".into(),
        }
    }
}

fn roots() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")));
    let dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(std::env::split_paths(&dirs))
        .filter(|p| p.is_absolute())
        .map(|p| p.join("applications"))
        .collect()
}
fn desktop(text: &str, desktops: &[&str]) -> Option<String> {
    let mut entry = false;
    let mut values = std::collections::HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            entry = line == "[Desktop Entry]";
        } else if entry && !line.starts_with('#') {
            if let Some((k, v)) = line.split_once('=') {
                values.insert(k.trim(), v.trim());
            }
        }
    }
    let get = |key| values.get(key).copied().unwrap_or("");
    let matches = |value: &str| {
        value
            .split(';')
            .any(|s| !s.is_empty() && desktops.contains(&s))
    };
    if get("Type") != "Application"
        || get("Hidden") == "true"
        || get("NoDisplay") == "true"
        || (!get("OnlyShowIn").is_empty() && !matches(get("OnlyShowIn")))
        || matches(get("NotShowIn"))
        || (get("Exec").is_empty() && get("DBusActivatable") != "true")
    {
        return None;
    }
    let try_exec = get("TryExec");
    if !try_exec.is_empty() {
        use std::os::unix::fs::PermissionsExt;
        let executable = |p: PathBuf| {
            fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        };
        if !(if Path::new(try_exec).is_absolute() {
            executable(try_exec.into())
        } else {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .any(|dir| executable(dir.join(try_exec)))
        }) {
            return None;
        }
    }
    let name = get("Name")
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect::<String>();
    (!name.is_empty()).then_some(name)
}
fn catalog_at(roots: &[PathBuf], desktops: &[&str]) -> Result<Vec<(String, String)>> {
    fn files(dir: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
        for item in fs::read_dir(dir)? {
            let item = item?;
            if item.file_type()?.is_dir() {
                files(&item.path(), found)?;
            } else if item.path().extension().is_some_and(|s| s == "desktop") {
                found.push(item.path());
            }
        }
        Ok(())
    }
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        let mut paths = Vec::new();
        files(root, &mut paths)?;
        paths.sort();
        for path in paths {
            let id = path.strip_prefix(root)?.to_string_lossy().replace('/', "-");
            if !seen.insert(id) {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            if let Some(name) = desktop(&text, desktops) {
                let path = path
                    .to_str()
                    .ok_or("non-UTF8 desktop entry path")?
                    .to_owned();
                Action::Application(path.clone()).validate()?;
                result.push((name, path));
            }
        }
    }
    result.sort_by(|a, b| {
        a.0.to_lowercase()
            .cmp(&b.0.to_lowercase())
            .then(a.1.cmp(&b.1))
    });
    Ok(result)
}
pub fn catalog() -> Result<Vec<(String, String)>> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    catalog_at(&roots(), &desktop.split(':').collect::<Vec<_>>())
}

// Only child dispatch requests are cancellable. Never kill the resulting application.
fn command(program: &str, args: &[String], cancelled: &impl Fn() -> bool) -> Result<String> {
    if cancelled() {
        return Err("action cancelled before dispatch".into());
    }
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    for fd in [out.as_raw_fd(), err.as_raw_fd()] {
        // SAFETY: owned pipe descriptor, preserve flags and enable nonblocking reads.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::last_os_error().into());
        }
    }
    let started = Instant::now();
    let mut data = Vec::new();
    let mut errors = Vec::new();
    let drain = |reader: &mut dyn Read, data: &mut Vec<u8>| -> io::Result<()> {
        let mut bytes = [0; 4096];
        for _ in 0..16 {
            match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => {
                    if data.len() + n > 65536 {
                        return Err(io::Error::other("action output limit reached"));
                    }
                    data.extend_from_slice(&bytes[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    };
    let result = (|| -> Result<String> {
        loop {
            drain(&mut out, &mut data)?;
            drain(&mut err, &mut errors)?;
            if cancelled() {
                return Err(
                    "action cancelled; completed application/audio effects are preserved".into(),
                );
            }
            if started.elapsed() > Duration::from_secs(5) {
                return Err(format!("{program} dispatch timed out").into());
            }
            if let Some(status) = child.try_wait()? {
                drain(&mut out, &mut data)?;
                drain(&mut err, &mut errors)?;
                if !status.success() {
                    return Err(format!(
                        "{program}: {status}: {}",
                        String::from_utf8_lossy(&errors).trim()
                    )
                    .into());
                }
                return Ok(String::from_utf8_lossy(&data).trim().to_string());
            }
            thread::sleep(Duration::from_millis(20));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}
fn audio(action: &Action, mut run: impl FnMut(&[String]) -> Result<String>) -> Result<String> {
    let default = if *action == Action::MicrophoneMute {
        "@DEFAULT_AUDIO_SOURCE@"
    } else {
        "@DEFAULT_AUDIO_SINK@"
    };
    // Resolve anew for each activation, then read back the exact object we changed.
    let info = run(&strings(&["inspect", default]))?;
    let id = info
        .split_whitespace()
        .skip_while(|s| *s != "id")
        .nth(1)
        .ok_or("wpctl did not resolve a default audio target")?
        .trim_end_matches(',')
        .parse::<u32>()?
        .to_string();
    let request = match action {
        Action::Volume { up, step } => vec![
            "set-volume".into(),
            id.clone(),
            format!("{step}%{}", if *up { "+" } else { "-" }),
            "--limit".into(),
            "1.0".into(),
        ],
        Action::OutputMute | Action::MicrophoneMute => {
            vec!["set-mute".into(), id.clone(), "toggle".into()]
        }
        _ => return Err("not an audio action".into()),
    };
    run(&request)?;
    let state = run(&["get-volume".into(), id])?;
    let volume = state
        .strip_prefix("Volume: ")
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<f64>().ok());
    if !volume.is_some_and(|v| v.is_finite() && v >= 0.0) {
        return Err("audio request sent but read-back was invalid".into());
    }
    Ok(format!("{}: {state}", action.description()))
}
fn launch_request(
    path: &str,
    apps: &[(String, String)],
    session: bool,
    mut run: impl FnMut(&[String]) -> Result<String>,
) -> Result<String> {
    if !apps.iter().any(|(_, p)| p == path) {
        return Err(format!("saved application is missing or no longer visible: {path}; reselect it in configuration").into());
    }
    if !session {
        return Err("desktop session unavailable for launch".into());
    }
    run(&strings(&["launch", path]))?;
    Ok("Native launch request accepted; window/readiness not verified".into())
}
fn launch(path: &str, cancelled: &impl Fn() -> bool) -> Result<String> {
    let session = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        && (std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some());
    launch_request(path, &catalog()?, session, |args| {
        command("gio", args, cancelled)
    })
}

struct Job {
    action: Action,
    node: Option<PathBuf>,
    epoch: u64,
}
pub struct Dispatcher {
    requests: mpsc::SyncSender<Job>,
    notices: mpsc::Receiver<(u64, bool, String)>,
    epoch: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Dispatcher {
    pub(crate) fn with(
        mut run: impl FnMut(&Action, Option<&Path>, &dyn Fn() -> bool) -> Result<String>
        + Send
        + 'static,
    ) -> Self {
        let (requests, receiver) = mpsc::sync_channel::<Job>(8);
        let (tx, notices) = mpsc::sync_channel(16);
        let epoch = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let e = epoch.clone();
        let s = stop.clone();
        let worker = thread::spawn(move || {
            while !s.load(Ordering::Acquire) {
                let job = match receiver.recv_timeout(Duration::from_millis(20)) {
                    Ok(j) => j,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                };
                let cancelled =
                    || s.load(Ordering::Acquire) || e.load(Ordering::Acquire) != job.epoch;
                if cancelled() {
                    continue;
                }
                let result = run(&job.action, job.node.as_deref(), &cancelled);
                if !cancelled() {
                    let _ = tx.try_send((
                        job.epoch,
                        job.action.flow(),
                        result.unwrap_or_else(|e| format!("Action failed: {e}")),
                    ));
                }
            }
        });
        Self {
            requests,
            notices,
            epoch,
            stop,
            worker: Some(worker),
        }
    }
    pub fn new() -> Self {
        Self::with(|action, node, cancelled| match action {
            Action::Application(path) => launch(path, &cancelled),
            Action::FlowStart | Action::FlowRepair => flow(
                action,
                node.ok_or("keyboard has not been registered")?,
                &cancelled,
            ),
            Action::Shortcut => Err("shortcut belongs to keyboard output".into()),
            _ => audio(action, |args| command("wpctl", args, &cancelled)),
        })
    }
    pub fn submit(&self, action: Action, node: Option<PathBuf>) -> Result<()> {
        action.validate()?;
        self.requests
            .try_send(Job {
                action,
                node,
                epoch: self.epoch.load(Ordering::Acquire),
            })
            .map_err(|_| "action queue full/unavailable; request skipped".into())
    }
    pub fn notice(&self) -> Option<(bool, String)> {
        while let Ok((epoch, flow, notice)) = self.notices.try_recv() {
            if epoch == self.epoch.load(Ordering::Acquire) {
                return Some((flow, notice));
            }
        }
        None
    }
    pub fn cancel(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
}
impl Drop for Dispatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.cancel();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

const FLOW_APP: &str = "/usr/lib/wispr-flow/wispr-flow";
const FLOW_HELPER: &str = "/usr/lib/wispr-flow/resources/Release/wispr-flow-linux-helper";
fn flow_main_args(args: &[u8]) -> bool {
    !args
        .split(|b| *b == 0 || b.is_ascii_whitespace())
        .any(|s| s.starts_with(b"--type="))
}
fn flow_processes() -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut main = Vec::new();
    let mut helper = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let p = entry?.path();
        if !p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .chars()
            .all(|c| c.is_ascii_digit())
        {
            continue;
        }
        if !fs::metadata(&p).is_ok_and(|m| m.uid() == unsafe { libc::geteuid() }) {
            continue;
        }
        let Ok(exe) = fs::read_link(p.join("exe")) else {
            continue;
        };
        if exe == Path::new(FLOW_HELPER) {
            helper.push(p);
        } else if exe == Path::new(FLOW_APP) {
            let args = fs::read(p.join("cmdline"))?;
            if flow_main_args(&args) {
                main.push(p);
            }
        }
    }
    if main.len() > 1 {
        return Err("multiple Flow owners; resolve manually before repair".into());
    }
    Ok((main, helper))
}
fn capture(node: &Path, helpers: &[PathBuf]) -> Result<bool> {
    let keyboard = fs::metadata(node)?;
    for helper in helpers {
        let Ok(fds) = fs::read_dir(helper.join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if fs::metadata(fd.path()).is_ok_and(|m| {
                m.dev() == keyboard.dev()
                    && m.ino() == keyboard.ino()
                    && m.rdev() == keyboard.rdev()
            }) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
fn wait_until(
    cancelled: &impl Fn() -> bool,
    mut ready: impl FnMut() -> Result<bool>,
) -> Result<()> {
    let started = Instant::now();
    loop {
        if cancelled() {
            return Err("Flow operation cancelled; output remains paused".into());
        }
        if ready()? {
            return Ok(());
        }
        if started.elapsed() >= Duration::from_secs(8) {
            return Err("Flow stage timed out; output remains paused".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum FlowStep {
    Quit,
    WaitStopped,
    Launch,
    Capture,
}
fn flow_lifecycle(
    repair: bool,
    running: bool,
    registered: bool,
    mut stage: impl FnMut(FlowStep) -> Result<()>,
) -> Result<()> {
    if !registered {
        return Err("current keyboard device is unavailable".into());
    }
    if repair && running {
        stage(FlowStep::Quit).map_err(|e| format!("Flow graceful quit request: {e}"))?;
        stage(FlowStep::WaitStopped).map_err(|e| format!("Flow wait for app/helper exit: {e}"))?;
    }
    if repair || !running {
        stage(FlowStep::Launch).map_err(|e| format!("Flow launch request: {e}"))?;
    }
    stage(FlowStep::Capture).map_err(|e| format!("Flow current keyboard capture: {e}"))?;
    Ok(())
}
fn flow(action: &Action, node: &Path, cancelled: &impl Fn() -> bool) -> Result<String> {
    let result = (|| -> Result<String> {
        let path = catalog()?
            .into_iter()
            .find(|(_, p)| {
                Path::new(p)
                    .file_name()
                    .is_some_and(|n| n == "wispr-flow.desktop")
            })
            .ok_or("installed Flow desktop launcher is unavailable")?
            .1;
        let (main, helper) = flow_processes()?;
        if *action == Action::FlowStart && main.is_empty() && !helper.is_empty() {
            return Err("stale Flow helper; select explicit repair or resolve manually".into());
        }
        flow_lifecycle(
            *action == Action::FlowRepair,
            !main.is_empty() || !helper.is_empty(),
            node.exists(),
            |stage| {
                match stage {
                    FlowStep::Quit => {
                        // Installed Electron second-instance handler supports app.quit via --quit-app.
                        // ponytail: exact inspected package paths; recheck adapter when Flow packaging changes.
                        command("/usr/bin/wispr-flow", &strings(&["--quit-app"]), cancelled)?;
                    }
                    FlowStep::WaitStopped => wait_until(cancelled, || {
                        let (m, h) = flow_processes()?;
                        Ok(m.is_empty() && h.is_empty())
                    })?,
                    FlowStep::Launch => {
                        launch(&path, cancelled)?;
                    }
                    FlowStep::Capture => wait_until(cancelled, || {
                        let (_, h) = flow_processes()?;
                        capture(node, &h)
                    })?,
                }
                Ok(())
            },
        )?;
        Ok("Flow helper opened the current keyboard; dictation remains unverified. Output is paused: test physical Shift, then explicitly /resume.".into())
    })();
    result.map_err(|e| format!("{e}. Manual recovery: fully Quit Wispr Flow from its tray; keep KeyAI's keyboard ready; reopen Wispr Flow; test physical Shift; then /resume. No forced stop or settings reset was attempted.").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flow_order_explicit_repair_failure_and_exact_targets_are_recorded_only() {
        use crate::{Control, Event, keyboard::Keyboard, mappings::Mapping};
        let pad = Control::from_note(10, 36).unwrap();
        let mut keyboard = Keyboard::new(&[Mapping::new(pad, "Shift", "hold").unwrap()]).unwrap();
        let mut keys = Vec::new();
        keyboard
            .observe(Event::Press(pad), &mut |k, d| {
                keys.push((k, d));
                Ok(())
            })
            .unwrap();
        keyboard
            .pause(&mut |k, d| {
                keys.push((k, d));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            keys,
            [
                (evdev::KeyCode::KEY_LEFTSHIFT, true),
                (evdev::KeyCode::KEY_LEFTSHIFT, false)
            ]
        );
        let mut stages = Vec::new();
        flow_lifecycle(false, false, true, |s| {
            stages.push(s);
            Ok(())
        })
        .unwrap();
        assert_eq!(stages, [FlowStep::Launch, FlowStep::Capture]);
        stages.clear();
        flow_lifecycle(false, true, true, |s| {
            stages.push(s);
            Ok(())
        })
        .unwrap();
        assert_eq!(stages, [FlowStep::Capture]); // no implicit restart
        stages.clear();
        flow_lifecycle(true, true, true, |s| {
            stages.push(s);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            stages,
            [
                FlowStep::Quit,
                FlowStep::WaitStopped,
                FlowStep::Launch,
                FlowStep::Capture
            ]
        );
        stages.clear();
        assert!(
            flow_lifecycle(true, true, false, |s| {
                stages.push(s);
                Ok(())
            })
            .is_err()
        );
        assert!(stages.is_empty());
        assert!(
            flow_lifecycle(true, true, true, |s| {
                stages.push(s);
                if s == FlowStep::WaitStopped {
                    Err("timeout".into())
                } else {
                    Ok(())
                }
            })
            .unwrap_err()
            .to_string()
            .contains("wait for app/helper exit")
        );
        assert_eq!(stages, [FlowStep::Quit, FlowStep::WaitStopped]); // no reopen after stop failure
        assert!(flow_main_args(
            b"/usr/lib/wispr-flow/wispr-flow --class=Wispr Flow\0"
        ));
        assert!(!flow_main_args(
            b"/usr/lib/wispr-flow/wispr-flow --type=zygote\0"
        ));
        assert!(!flow_main_args(
            b"/usr/lib/wispr-flow/wispr-flow\0--type=renderer\0"
        ));
        assert_eq!(FLOW_APP, "/usr/lib/wispr-flow/wispr-flow");
        assert_eq!(
            FLOW_HELPER,
            "/usr/lib/wispr-flow/resources/Release/wispr-flow-linux-helper"
        );
    }
    #[test]
    fn native_catalog_audio_targets_limits_and_dispatch_are_recorded_only() {
        assert!(
            desktop(
                "[Desktop Entry]\nType=Application\nName=Hidden\nExec=unsafe %U\nHidden=true",
                &["GNOME"]
            )
            .is_none()
        );
        assert_eq!(
            desktop(
                "[Desktop Entry]\nType=Application\nName=Visible\nExec=never execute this",
                &["GNOME"]
            ),
            Some("Visible".into())
        );
        assert!(
            Action::Volume {
                up: true,
                step: 101
            }
            .validate()
            .is_err()
        );
        let mut requests = Vec::new();
        for (id, action) in [
            (42, Action::Volume { up: true, step: 5 }),
            (51, Action::OutputMute),
            (60, Action::MicrophoneMute),
        ] {
            let result = audio(&action, |args| {
                requests.push(args.to_vec());
                Ok(match args[0].as_str() {
                    "inspect" => format!("id {id}, type PipeWire:Interface:Node"),
                    "get-volume" => "Volume: 1.00 [MUTED]".into(),
                    _ => String::new(),
                })
            })
            .unwrap();
            assert!(result.contains("Volume:"));
        }
        assert_eq!(
            requests[1],
            strings(&["set-volume", "42", "5%+", "--limit", "1.0"])
        );
        assert_eq!(requests[4], strings(&["set-mute", "51", "toggle"]));
        assert_eq!(requests[6], strings(&["inspect", "@DEFAULT_AUDIO_SOURCE@"]));
        assert!(audio(&Action::OutputMute, |_| Err("missing target".into())).is_err());
        let (entered, rx) = mpsc::channel();
        let dispatch = Dispatcher::with(move |_, _, cancelled| {
            entered.send(()).unwrap();
            while !cancelled() {
                thread::sleep(Duration::from_millis(1));
            }
            Err("cancelled slow request".into())
        });
        dispatch.submit(Action::OutputMute, None).unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        for _ in 0..8 {
            dispatch.submit(Action::OutputMute, None).unwrap();
        }
        assert!(dispatch.submit(Action::OutputMute, None).is_err());
        let started = Instant::now();
        dispatch.cancel();
        drop(dispatch);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
