use std::collections::HashSet;
use std::io::{self, Write};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

mod keyboard;
mod mappings;
mod midi;
use mappings::{Mapping, config_path, load, save};

const PAD_CHANNEL: u8 = 10;
const DEVICE: &str = "09e8:1049";
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Pad {
    channel: u8,
    note: u8,
    bank: char,
    number: u8,
}

impl Pad {
    fn from_note(channel: u8, note: u8) -> Option<Self> {
        if channel != PAD_CHANNEL {
            return None;
        }
        let (bank, number) = match note {
            0x24..=0x2B => ('A', note - 0x24 + 1),
            0x2C..=0x33 => ('B', note - 0x2C + 1),
            _ => return None,
        };
        Some(Self {
            channel,
            note,
            bank,
            number,
        })
    }
}

#[derive(Debug, PartialEq)]
enum Event {
    Press(Pad),
    Release(Pad, ReleaseType),
    DuplicatePress(Pad),
    UnmatchedRelease(Pad),
    Pressure(Pad),
}

#[derive(Debug, PartialEq)]
enum ReleaseType {
    NoteOff,
    NoteOnVelocityZero,
}

#[derive(Default)]
struct Detector {
    down: HashSet<Pad>,
    pressure_seen: HashSet<Pad>,
}

impl Detector {
    fn observe(&mut self, bytes: [u8; 3]) -> Option<Event> {
        let [status, note, value] = bytes;
        let kind = status & 0xF0;
        let channel = (status & 0x0F) + 1;
        let pad = Pad::from_note(channel, note)?;

        match kind {
            0x90 if value > 0 => {
                if self.down.insert(pad) {
                    Some(Event::Press(pad))
                } else {
                    Some(Event::DuplicatePress(pad))
                }
            }
            0x80 => {
                self.pressure_seen.remove(&pad);
                if self.down.remove(&pad) {
                    Some(Event::Release(pad, ReleaseType::NoteOff))
                } else {
                    Some(Event::UnmatchedRelease(pad))
                }
            }
            0x90 if value == 0 => {
                self.pressure_seen.remove(&pad);
                if self.down.remove(&pad) {
                    Some(Event::Release(pad, ReleaseType::NoteOnVelocityZero))
                } else {
                    Some(Event::UnmatchedRelease(pad))
                }
            }
            0xA0 if self.down.contains(&pad) && self.pressure_seen.insert(pad) => {
                Some(Event::Pressure(pad))
            }
            _ => None,
        }
    }
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] || args == ["help"] {
        println!(
            "Usage: keyai [detect|list|get|configure|run]\n\
                  detect: display MIDI events (default)\n\
                  list: display saved assignments without opening MIDI\n\
                  get: press a pad to inspect its assignment\n\
                  configure: press a pad, inspect, then assign or replace it\n\
                  run: operate saved hold/toggle shortcuts; Ctrl+C releases and exits\n\
                  Only run generates keyboard input."
        );
        return Ok(());
    }
    if args.len() > 1
        || args.first().is_some_and(|arg| {
            !matches!(
                arg.as_str(),
                "detect" | "list" | "get" | "configure" | "run"
            )
        })
    {
        return Err("unknown command; use keyai --help".into());
    }
    let path = config_path()?;
    let mut mappings = load(&path)?;
    println!("Configuration: {}", path.display());
    if mappings.is_empty() {
        println!("No saved assignments.");
    } else {
        for mapping in &mappings {
            println!("{mapping}");
        }
    }
    match args.first().map(String::as_str).unwrap_or("detect") {
        "list" => Ok(()),
        "detect" => detect(),
        "run" => keyboard::run(&mappings),
        command => {
            let pad = learn_pad()?;
            match mappings.iter().find(|mapping| mapping.pad == pad) {
                Some(mapping) => println!("Current assignment: {mapping}"),
                None => println!("Bank {} Pad {} has no assignment.", pad.bank, pad.number),
            }
            if command == "get" {
                return Ok(());
            }
            let shortcut = prompt("Shortcut (e.g. Shift or Ctrl+Alt+K; blank cancels): ")?;
            if shortcut.is_empty() {
                println!("Cancelled; configuration unchanged.");
                return Ok(());
            }
            let keys = mappings::parse_shortcut(&shortcut)?;
            let behavior = prompt("Behavior (hold or toggle): ")?;
            let mapping = Mapping::new(pad, &keys.join("+"), &behavior)?;
            mappings.retain(|existing| existing.pad != pad);
            mappings.push(mapping.clone());
            mappings.sort_by_key(|mapping| (mapping.pad.channel, mapping.pad.note));
            save(&path, &mappings)?;
            println!("Saved: {mapping}");
            Ok(())
        }
    }
}

fn prompt(message: &str) -> Result<String> {
    print!("{message}");
    io::stdout().flush()?;
    let mut input = String::new();
    if io::stdin().read_line(&mut input)? == 0 {
        return Err("terminal input ended; configuration unchanged".into());
    }
    Ok(input.trim().to_owned())
}

fn input_port() -> Result<String> {
    let usb = Command::new("lsusb").args(["-d", DEVICE]).output()?;
    if !usb.status.success() {
        return Err("AKAI MPK mini 3 (USB 09e8:1049) is unavailable; check lsusb".into());
    }
    let usb = String::from_utf8(usb.stdout)?;
    if usb.trim().is_empty() {
        return Err("AKAI MPK mini 3 (USB 09e8:1049) is not connected".into());
    }

    let listing = Command::new("amidi").arg("--list-devices").output()?;
    if !listing.status.success() {
        return Err("amidi could not enumerate ALSA MIDI ports".into());
    }
    let listing = String::from_utf8(listing.stdout)?;
    let matches: Vec<_> = listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let direction = fields.next()?;
            if !direction.contains('I') {
                return None;
            }
            let port = fields.next()?;
            let name = fields.collect::<Vec<_>>().join(" ");
            name.to_lowercase()
                .contains("mpk mini 3")
                .then_some(port.to_owned())
        })
        .collect();
    let port = match matches.as_slice() {
        [port] => port.clone(),
        [] => {
            return Err(
                "MPK mini 3 is connected, but no matching ALSA MIDI input is available".into(),
            );
        }
        _ => {
            return Err(
                "multiple MPK mini 3 MIDI inputs found; refusing ambiguous selection".into(),
            );
        }
    };

    println!("Device: AKAI MPK mini 3 (USB 09e8:1049)");
    println!("MIDI input: {port}");
    Ok(port)
}

fn learn_pad() -> Result<Pad> {
    let port = input_port()?;
    println!("Press a pad in either bank to learn it. No shortcuts are generated; Ctrl+C cancels.");
    let mut child = midi::command(&port).spawn()?;
    let stdout = child.stdout.take().expect("piped MIDI input");
    let result = (|| -> Result<Pad> {
        let mut detector = Detector::default();
        for message in midi::messages(stdout) {
            if let Some(Event::Press(pad)) = detector.observe(message?) {
                println!("Learned: {}", describe(Event::Press(pad)));
                return Ok(pad);
            }
        }
        Err("MIDI input ended before a pad was learned; configuration unchanged".into())
    })();
    // Close the input before terminal editing, including on read failures.
    let _ = child.kill();
    child.wait()?;
    result
}

fn detect() -> Result<()> {
    let port = input_port()?;
    println!("Pad mode observed: MIDI Note messages on channel 10; Ctrl+C exits.");
    println!(
        "Press, hold, vary pressure, and release pads in either bank. No shortcuts are generated."
    );

    let mut child = midi::command(&port).spawn()?;
    let stdout = child.stdout.take().expect("piped MIDI input");
    let reader = thread::spawn(move || -> io::Result<Detector> {
        let mut detector = Detector::default();
        let started = Instant::now();
        for message in midi::messages(stdout) {
            let bytes = message?;
            if let Some(event) = detector.observe(bytes) {
                println!(
                    "+{:.3}s {:02X} {:02X} {:02X} | {}",
                    started.elapsed().as_secs_f64(),
                    bytes[0],
                    bytes[1],
                    bytes[2],
                    describe(event)
                );
            }
        }
        Ok(detector)
    });

    thread::sleep(Duration::from_millis(100));
    if let Some(status) = child.try_wait()? {
        let _ = reader.join();
        return Err(
            format!("MIDI input could not start; amidi exited with status {status}").into(),
        );
    }
    println!("Listening. Stop with Ctrl+C.");

    let status = child.wait()?;
    let detector = reader
        .join()
        .map_err(|_| "MIDI input reader stopped unexpectedly")??;
    if !status.success() {
        if !detector.down.is_empty() {
            eprintln!(
                "Input ended with {} pad(s) still down; resetting input state. Reconnect the device and restart.",
                detector.down.len()
            );
        }
        return Err(format!("MIDI input ended with status {status}").into());
    }
    Ok(())
}

fn describe(event: Event) -> String {
    let (pad, action) = match event {
        Event::Press(pad) => (pad, "PRESS: Note On"),
        Event::Release(pad, ReleaseType::NoteOff) => (pad, "RELEASE: Note Off"),
        Event::Release(pad, ReleaseType::NoteOnVelocityZero) => {
            (pad, "RELEASE: Note On velocity 0")
        }
        Event::DuplicatePress(pad) => (pad, "REPEAT IGNORED: pad already down"),
        Event::UnmatchedRelease(pad) => (pad, "RELEASE IGNORED: pad was not down"),
        Event::Pressure(pad) => (pad, "PRESSURE IGNORED: does not change pad state"),
    };
    format!(
        "{action} | Bank {} Pad {} | MIDI channel {} | note {} (0x{:02X})",
        pad.bank, pad.number, pad.channel, pad.note, pad.note
    )
}

#[cfg(test)]
mod tests {
    use super::{Detector, Event};

    #[test]
    fn press_pressure_duplicate_and_velocity_zero_release() {
        let mut detector = Detector::default();
        let press = detector.observe([0x99, 0x24, 0x20]).unwrap();
        assert!(matches!(press, Event::Press(_)));
        assert!(matches!(
            detector.observe([0x99, 0x24, 0x30]),
            Some(Event::DuplicatePress(_))
        ));
        assert!(matches!(
            detector.observe([0xA9, 0x24, 0x40]),
            Some(Event::Pressure(_))
        ));
        assert!(detector.observe([0xA9, 0x24, 0x50]).is_none());
        assert!(matches!(
            detector.observe([0x99, 0x24, 0]),
            Some(Event::Release(_, super::ReleaseType::NoteOnVelocityZero))
        ));
        assert!(detector.down.is_empty());

        match detector.observe([0x99, 0x2C, 0x20]) {
            Some(Event::Press(pad)) => {
                assert_eq!(pad.bank, 'B');
                assert_eq!(pad.number, 1);
            }
            event => panic!("expected Bank B Pad 1 press, got {event:?}"),
        }
    }
}
