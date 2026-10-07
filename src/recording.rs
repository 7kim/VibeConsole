use crate::{Result, keyboard, mappings};
use evdev::{EventType, KeyCode, raw_stream::RawDevice};
use std::collections::HashSet;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Chord {
    down: HashSet<(usize, KeyCode)>,
    armed: bool,
    keys: Vec<String>,
    releasing: bool,
}
impl Chord {
    fn event(&mut self, device: usize, code: KeyCode, value: i32) -> Result<Option<Vec<String>>> {
        if value == 2 {
            return Ok(None);
        } // autorepeat
        let identity = (device, code);
        match value {
            1 if self.down.insert(identity) => {
                if !self.armed {
                    return Ok(None);
                }
                let name = mappings::key_groups()
                    .into_iter()
                    .flat_map(|(_, keys)| keys)
                    .find(|name| keyboard::key_code(name).ok() == Some(code))
                    .ok_or_else(|| format!("unsupported physical key {code:?}; use Select keys"))?;
                if self.releasing {
                    return Err("multiple overlapping shortcuts detected; record one chord or use Select keys".into());
                }
                if !self.keys.contains(&name) {
                    self.keys.push(name);
                }
            }
            0 => {
                self.down.remove(&identity);
                if self.armed && !self.keys.is_empty() {
                    self.releasing = true;
                }
                if self.down.is_empty() {
                    if !self.keys.is_empty() {
                        return Ok(Some(mappings::parse_shortcut(&self.keys.join("+"))?));
                    }
                    self.armed = true;
                }
            }
            _ => {}
        }
        Ok(None)
    }
}

fn physical_path(path: &std::path::Path) -> bool {
    !path.starts_with("/sys/devices/virtual")
}

fn has_key(bits: &str, code: KeyCode) -> bool {
    let width = std::mem::size_of::<libc::c_ulong>() * 8;
    bits.split_whitespace()
        .rev()
        .nth(code.0 as usize / width)
        .and_then(|word| u64::from_str_radix(word, 16).ok())
        .is_some_and(|word| word & (1 << (code.0 as usize % width)) != 0)
}

fn devices() -> Result<Vec<RawDevice>> {
    let mut devices = Vec::new();
    for entry in std::fs::read_dir("/sys/class/input")? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| name.starts_with("event")) else {
            continue;
        };
        let path = std::fs::canonicalize(entry.path().join("device"))?;
        // Physical Linux input paths exclude uinput, including KeyAI and other virtual keyboards.
        if !physical_path(&path) {
            continue;
        }
        let bits = std::fs::read_to_string(path.join("capabilities/key"))?;
        if ![KeyCode::KEY_A, KeyCode::KEY_Z, KeyCode::KEY_LEFTSHIFT]
            .iter()
            .all(|code| has_key(&bits, *code))
        {
            continue;
        }
        let node = format!("/dev/input/{name}");
        let device = RawDevice::open(&node).map_err(|error| format!("cannot record from physical keyboard {node}: {error}; grant input read access or use Select keys"))?;
        if device.name() == Some("KeyAI virtual keyboard") {
            continue;
        }
        // SAFETY: borrowed device fd; only change its nonblocking status.
        unsafe {
            let flags = libc::fcntl(device.as_raw_fd(), libc::F_GETFL);
            if flags < 0
                || libc::fcntl(device.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        devices.push(device);
    }
    if devices.is_empty() {
        return Err("no readable physical keyboard found; use Select keys".into());
    }
    Ok(devices)
}
pub fn permissions() -> Result<usize> {
    Ok(devices()?.len())
}

pub struct Capture {
    devices: Vec<RawDevice>,
    chord: Chord,
    deadline: Instant,
}
impl Capture {
    pub fn open() -> Result<Self> {
        let mut devices = devices()?;
        let mut chord = Chord::default();
        for (index, device) in devices.iter_mut().enumerate() {
            // Discard pre-recording traffic before the current-key snapshot.
            loop {
                match device.fetch_events() {
                    Ok(events) => {
                        if events.count() == 0 {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error.into()),
                }
            }
            chord
                .down
                .extend(device.get_key_state()?.iter().map(|key| (index, key)));
        }
        chord.armed = chord.down.is_empty();
        Ok(Self {
            devices,
            chord,
            deadline: Instant::now() + Duration::from_secs(60),
        })
    }
    pub fn poll(&mut self) -> Result<Option<Vec<String>>> {
        if Instant::now() >= self.deadline {
            return Err(
                "recording timed out (60 seconds); mapping unchanged; use Select keys or retry"
                    .into(),
            );
        }
        let mut pending = Vec::new();
        for (index, device) in self.devices.iter_mut().enumerate() {
            match device.fetch_events() {
                Ok(events) => pending.extend(events.map(|event| (index, event))),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    return Err(format!("physical keyboard capture ended: {error}").into());
                }
            }
        }
        // Linux evdev timestamps order a chord spanning more than one physical keyboard.
        pending.sort_by_key(|(_, event)| event.timestamp());
        for (index, event) in pending {
            if event.event_type() == EventType::SYNCHRONIZATION && event.code() == 3 {
                return Err("physical input overflowed; retry recording".into());
            }
            if event.event_type() == EventType::KEY {
                if let Some(keys) = self
                    .chord
                    .event(index, KeyCode(event.code()), event.value())?
                {
                    return Ok(Some(keys));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chords_start_clean_finish_on_release_reject_sequences_and_unsupported_keys() {
        let mut chord = Chord {
            down: HashSet::from([(0, KeyCode::KEY_ENTER)]),
            ..Chord::default()
        };
        assert!(chord.event(0, KeyCode::KEY_ENTER, 0).unwrap().is_none());
        assert!(chord.event(0, KeyCode::KEY_LEFTSHIFT, 1).unwrap().is_none());
        assert!(chord.event(0, KeyCode::KEY_LEFTSHIFT, 2).unwrap().is_none());
        assert!(chord.event(0, KeyCode::KEY_LEFTSHIFT, 1).unwrap().is_none());
        assert_eq!(
            chord.event(0, KeyCode::KEY_LEFTSHIFT, 0).unwrap().unwrap(),
            ["Shift"]
        );
        let mut chord = Chord {
            armed: true,
            ..Chord::default()
        };
        for key in [KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTALT, KeyCode::KEY_K] {
            assert!(chord.event(0, key, 1).unwrap().is_none());
        }
        for key in [KeyCode::KEY_K, KeyCode::KEY_LEFTALT] {
            assert!(chord.event(0, key, 0).unwrap().is_none());
        }
        assert_eq!(
            chord.event(0, KeyCode::KEY_LEFTCTRL, 0).unwrap().unwrap(),
            ["Ctrl", "Alt", "K"]
        );
        let mut chord = Chord {
            armed: true,
            ..Chord::default()
        };
        assert!(chord.event(0, KeyCode::KEY_VOLUMEUP, 1).is_err());
        let mut chord = Chord {
            armed: true,
            ..Chord::default()
        };
        chord.event(0, KeyCode::KEY_LEFTCTRL, 1).unwrap();
        chord.event(0, KeyCode::KEY_K, 1).unwrap();
        chord.event(0, KeyCode::KEY_K, 0).unwrap();
        assert!(chord.event(0, KeyCode::KEY_C, 1).is_err());
        assert!(!physical_path(std::path::Path::new(
            "/sys/devices/virtual/input/input99"
        )));
        assert!(physical_path(std::path::Path::new(
            "/sys/devices/pci0000:00/usb3/input4"
        )));
        assert!(has_key("1", KeyCode(0)));
        assert!(!has_key("1", KeyCode(1)));
    }
}
