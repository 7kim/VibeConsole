use crate::mappings::{Behavior, Mapping};
use crate::{Detector, Event, Pad, Result, input_port, midi};
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, uinput::VirtualDevice};
use std::collections::HashSet;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

pub fn key_code(name: &str) -> Result<KeyCode> {
    let linux_name = match name {
        "Ctrl" => "LEFTCTRL",
        "Shift" => "LEFTSHIFT",
        "Alt" => "LEFTALT",
        "Meta" => "LEFTMETA",
        "Escape" => "ESC",
        "LeftBracket" => "LEFTBRACE",
        "RightBracket" => "RIGHTBRACE",
        "Apostrophe" => "APOSTROPHE",
        other => &other.to_ascii_uppercase(),
    };
    format!("KEY_{linux_name}")
        .parse()
        .map_err(|_| format!("key {name:?} is unsupported by the Linux input backend").into())
}

fn modifier(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::KEY_LEFTCTRL
            | KeyCode::KEY_RIGHTCTRL
            | KeyCode::KEY_LEFTSHIFT
            | KeyCode::KEY_RIGHTSHIFT
            | KeyCode::KEY_LEFTALT
            | KeyCode::KEY_RIGHTALT
            | KeyCode::KEY_LEFTMETA
            | KeyCode::KEY_RIGHTMETA
    )
}

struct Assignment {
    pad: Pad,
    keys: Vec<KeyCode>,
    behavior: Behavior,
    active: bool,
}

struct Keyboard {
    assignments: Vec<Assignment>,
    down: HashSet<Pad>,
    // Includes attempted key-downs: emit can fail after the kernel received the key.
    held: HashSet<KeyCode>,
}

impl Keyboard {
    fn new(mappings: &[Mapping]) -> Result<Self> {
        let mut seen = HashSet::new();
        let mut assignments = Vec::new();
        for mapping in mappings {
            if Mapping::new(
                mapping.pad,
                &mapping.keys.join("+"),
                &mapping.behavior.to_string(),
            )? != *mapping
                || !seen.insert(mapping.pad)
            {
                return Err("invalid or duplicate assignment".into());
            }
            let keys = mapping
                .keys
                .iter()
                .map(|key| key_code(key))
                .collect::<Result<_>>()?;
            assignments.push(Assignment {
                pad: mapping.pad,
                keys,
                behavior: mapping.behavior.clone(),
                active: false,
            });
        }
        Ok(Self {
            assignments,
            down: HashSet::new(),
            held: HashSet::new(),
        })
    }

    fn observe(
        &mut self,
        event: Event,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<Option<(Pad, bool)>> {
        let (pad, pressed) = match event {
            Event::Press(pad) if self.down.insert(pad) => (pad, true),
            Event::Release(pad, _) if self.down.remove(&pad) => (pad, false),
            _ => return Ok(None),
        };
        let Some(index) = self.assignments.iter().position(|item| item.pad == pad) else {
            return Ok(None);
        };
        let assignment = &self.assignments[index];
        let active = match assignment.behavior {
            Behavior::Hold => pressed,
            Behavior::Toggle if pressed => !assignment.active,
            Behavior::Toggle => return Ok(None),
        };
        if active == assignment.active {
            return Ok(None);
        }
        let mut keys = assignment.keys.clone();
        if !active {
            keys.reverse();
        }
        for key in keys {
            // ponytail: scan at most 16 assignments; use counts if the supported control set grows.
            let shared = self
                .assignments
                .iter()
                .enumerate()
                .any(|(other, item)| other != index && item.active && item.keys.contains(&key));
            if shared {
                continue;
            }
            if active {
                self.held.insert(key);
                emit(key, true)?;
            } else {
                emit(key, false)?;
                self.held.remove(&key);
            }
        }
        self.assignments[index].active = active;
        Ok(Some((pad, active)))
    }

    fn cleanup(
        &mut self,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut keys: Vec<_> = self.held.iter().copied().collect();
        keys.sort_by_key(|key| (modifier(*key), key.0));
        let mut failure = None;
        for key in keys {
            match emit(key, false) {
                Ok(()) => {
                    self.held.remove(&key);
                }
                Err(error) => {
                    failure = Some(error);
                }
            }
        }
        self.down.clear();
        for assignment in &mut self.assignments {
            assignment.active = false;
        }
        failure.map_or(Ok(()), Err)
    }
}

pub fn run(mappings: &[Mapping]) -> Result<()> {
    let mut keyboard = Keyboard::new(mappings)?;
    if mappings.is_empty() {
        return Err("no mappings to run; use keyai configure first".into());
    }
    let port = input_port()?;
    println!(
        "Desktop session: {} / {}",
        std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
        std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default()
    );
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGQUIT,
    ] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }
    // MIDI node identity also catches unplug/replug when amidi hasn't reported EOF yet.
    let fields: Vec<_> = port
        .strip_prefix("hw:")
        .ok_or("unexpected ALSA port")?
        .split(',')
        .collect();
    let [card, device, _] = fields.as_slice() else {
        return Err("unexpected ALSA port".into());
    };
    let node = format!("/dev/snd/midiC{card}D{device}");
    let metadata = std::fs::metadata(&node)?;
    let identity = (metadata.dev(), metadata.ino(), metadata.rdev());

    // Advertise a standard keyboard range so modifier-only mappings are classified as keyboards.
    let mut supported = AttributeSet::<KeyCode>::new();
    for code in 1..=KeyCode::KEY_RIGHTMETA.0 {
        supported.insert(KeyCode(code));
    }
    let mut output = VirtualDevice::builder()
        .and_then(|builder| {
            builder
                .name("KeyAI virtual keyboard")
                .with_keys(&supported)?
                .build()
        })
        .map_err(|error| {
            format!(
                "cannot create keyboard through /dev/uinput: {error}; see README.md for permissions"
            )
        })?;
    // ponytail: allow one second for desktop device discovery; replace with readiness detection if needed.
    thread::sleep(Duration::from_secs(1));
    let mut command = midi::command(&port);
    let parent = std::process::id();
    // SAFETY: only async-signal-safe Linux syscalls run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() as u32 != parent {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("piped MIDI input");
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        for message in midi::messages(stdout) {
            let failed = message.is_err();
            if sender.send(message).is_err() || failed {
                break;
            }
        }
    });
    let mut emit = |key: KeyCode, down: bool| {
        output.emit(&[InputEvent::new(EventType::KEY.0, key.0, i32::from(down))])
    };
    let result = (|| -> Result<()> {
        let mut detector = Detector::default();
        println!(
            "Ready: all mappings inactive. Hold pads release on lift; toggles release on the next press."
        );
        println!(
            "Active means synthetic keys held, not application listening. LED control inconclusive; terminal feedback only. Ctrl+C exits."
        );
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let connected = std::fs::metadata(&node)
                .is_ok_and(|meta| (meta.dev(), meta.ino(), meta.rdev()) == identity);
            if !connected {
                return Err("MIDI device disconnected; restart after reconnecting".into());
            }
            let incoming = receiver.recv_timeout(Duration::from_millis(100));
            // Ctrl+C can close amidi's pipe while we wait; requested shutdown wins over EOF.
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            match incoming {
                Ok(message) => {
                    if let Some(event) = detector.observe(message?) {
                        if let Some((pad, active)) = keyboard.observe(event, &mut emit)? {
                            use std::io::Write;
                            writeln!(
                                io::stdout().lock(),
                                "Bank {} Pad {} | {}",
                                pad.bank,
                                pad.number,
                                if active {
                                    "ACTIVE: keys held"
                                } else {
                                    "INACTIVE: ownership released"
                                }
                            )?;
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("MIDI input ended; restart after checking the device/port".into());
                }
            }
        }
    })();
    // Release before waiting for any child/thread, even on read/output failures.
    let cleanup = keyboard.cleanup(&mut emit);
    if let Err(error) = &cleanup {
        eprintln!(
            "Key release failed: {error}; destroying the virtual keyboard. Desktop recovery is unverified."
        );
    } else {
        println!("Stopped: all synthetic keys released; pad state cleared.");
    }
    drop(output);
    let _ = child.kill();
    let waited = child.wait();
    let joined = reader.join();
    result?;
    cleanup?;
    waited?;
    joined.map_err(|_| "MIDI reader stopped unexpectedly")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReleaseType;

    #[test]
    fn stream_release_arrives_during_silence_before_eof() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let pad = Pad::from_note(10, 36).unwrap();
        let mut keyboard = Keyboard::new(&[Mapping::new(pad, "Shift", "hold").unwrap()]).unwrap();
        let (mut writer, reader) = UnixStream::pair().unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut detector = Detector::default();
            let mut emit = |key, down| {
                sender.send((key, down)).unwrap();
                Ok(())
            };
            for message in crate::midi::messages(reader) {
                if let Some(event) = detector.observe(message.unwrap()) {
                    keyboard.observe(event, &mut emit).unwrap();
                }
            }
        });
        writer
            .write_all(&[0x99, 36, 32, 0xA9, 36, 64, 0x89, 36, 0])
            .unwrap();
        let press = receiver.recv_timeout(Duration::from_secs(1));
        let release = receiver.recv_timeout(Duration::from_secs(1));
        // Close only after checking: EOF must not be what delivers the release.
        drop(writer);
        worker.join().unwrap();
        assert_eq!(press.unwrap(), (KeyCode::KEY_LEFTSHIFT, true));
        assert_eq!(
            release.expect("release must arrive without a later message or EOF"),
            (KeyCode::KEY_LEFTSHIFT, false)
        );
    }

    #[test]
    fn holds_toggles_overlap_order_cleanup_and_failed_output() {
        let a = Pad::from_note(10, 36).unwrap();
        let b = Pad::from_note(10, 37).unwrap();
        let c = Pad::from_note(10, 44).unwrap();
        let mappings = vec![
            Mapping::new(a, "Ctrl", "hold").unwrap(),
            Mapping::new(b, "C+Ctrl", "toggle").unwrap(),
            Mapping::new(c, "Shift+Ctrl", "toggle").unwrap(),
        ];
        let mut keyboard = Keyboard::new(&mappings).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        let mut detector = Detector::default();
        for bytes in [
            [0x99, 36, 20],
            [0x99, 37, 20],
            [0x99, 37, 30],
            [0xA9, 37, 40],
            [0x89, 37, 0],
            [0x99, 44, 20],
            [0x99, 44, 0],
            [0x89, 36, 0],
            [0x99, 37, 20],
            [0x89, 37, 0],
        ] {
            if let Some(event) = detector.observe(bytes) {
                keyboard.observe(event, &mut emit).unwrap();
            }
        }
        assert_eq!(
            keyboard
                .assignments
                .iter()
                .map(|item| item.active)
                .collect::<Vec<_>>(),
            [false, false, true]
        );
        keyboard.cleanup(&mut emit).unwrap();
        assert!(keyboard.held.is_empty());
        assert!(keyboard.down.is_empty());
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_C, true),
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_C, false),
                (KeyCode::KEY_LEFTCTRL, false),
                (KeyCode::KEY_LEFTSHIFT, false)
            ]
        );

        // A fresh engine starts inactive and releases nonmodifiers before modifiers.
        let mut keyboard =
            Keyboard::new(&[Mapping::new(a, "K+Shift+Ctrl", "hold").unwrap()]).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Pressure(a), &mut emit).unwrap();
        keyboard
            .observe(Event::Release(a, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.cleanup(&mut emit).unwrap();
        assert_eq!(
            recorded[..6],
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_K, true),
                (KeyCode::KEY_K, false),
                (KeyCode::KEY_LEFTCTRL, false),
                (KeyCode::KEY_LEFTSHIFT, false)
            ]
        );
        assert_eq!(recorded[9].0, KeyCode::KEY_K);

        // Failed key-down remains a cleanup candidate, with no successful activation reported.
        let mut fail = |_, _| Err(io::Error::other("output failed"));
        assert!(keyboard.observe(Event::Press(a), &mut fail).is_err());
        assert!(!keyboard.assignments[0].active);
        let mut released = Vec::new();
        keyboard
            .cleanup(&mut |key, down| {
                released.push((key, down));
                Ok(())
            })
            .unwrap();
        assert_eq!(released, [(KeyCode::KEY_LEFTSHIFT, false)]);

        // Two holds share Ctrl until its last owner lifts.
        let mut keyboard = Keyboard::new(&[
            Mapping::new(a, "Ctrl", "hold").unwrap(),
            Mapping::new(b, "Ctrl+C", "hold").unwrap(),
        ])
        .unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        for event in [
            Event::Press(a),
            Event::Press(b),
            Event::Release(b, ReleaseType::NoteOff),
            Event::Release(a, ReleaseType::NoteOff),
        ] {
            keyboard.observe(event, &mut emit).unwrap();
        }
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_C, true),
                (KeyCode::KEY_C, false),
                (KeyCode::KEY_LEFTCTRL, false)
            ]
        );

        // Cleanup attempts every key even if the first release fails, then permits a retry.
        keyboard
            .observe(Event::Press(b), &mut |_, _| Ok(()))
            .unwrap();
        let mut attempts = Vec::new();
        assert!(
            keyboard
                .cleanup(&mut |key, _| {
                    attempts.push(key);
                    if key == KeyCode::KEY_C {
                        Err(io::Error::other("release failed"))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
        assert_eq!(attempts, [KeyCode::KEY_C, KeyCode::KEY_LEFTCTRL]);
        assert_eq!(keyboard.held, HashSet::from([KeyCode::KEY_C]));
        keyboard.cleanup(&mut |_, _| Ok(())).unwrap();
    }
}
