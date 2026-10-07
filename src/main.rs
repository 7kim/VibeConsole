mod actions;
use std::collections::HashSet;
use std::process::Command;

mod feedback;
mod keyboard;
mod mappings;
mod midi;
mod motion;
mod recording;
mod runtime;
mod screen;
mod status;
mod terminal;
use mappings::{config_path, load};

const PAD_CHANNEL: u8 = 10;
const DEVICE: &str = "09e8:1049";
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Message {
    Note,
    Cc,
    Bend,
}
impl Message {
    fn text(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Cc => "cc",
            Self::Bend => "bend",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Control {
    channel: u8,
    id: u8,
    message: Message,
    direction: i8,
}

impl Control {
    fn from_note(channel: u8, note: u8) -> Option<Self> {
        (channel == PAD_CHANNEL && (36..=51).contains(&note)).then_some(Self {
            channel,
            id: note,
            message: Message::Note,
            direction: 0,
        })
    }
    fn note(channel: u8, note: u8) -> Option<Self> {
        ((1..=16).contains(&channel) && note <= 127).then_some(Self {
            channel,
            id: note,
            message: Message::Note,
            direction: 0,
        })
    }
    fn validate(self) -> Result<()> {
        if !(1..=16).contains(&self.channel)
            || self.id > 127
            || match self.message {
                Message::Note => self.direction != 0,
                Message::Cc => ![-1, 1].contains(&self.direction),
                Message::Bend => self.id != 0 || ![-1, 1].contains(&self.direction),
            }
        {
            return Err("invalid control identity".into());
        }
        Ok(())
    }
    fn label(self) -> String {
        if self.message != Message::Note {
            return format!(
                "{} channel {} id {} direction {}",
                self.message.text(),
                self.channel,
                self.id,
                self.direction
            );
        }
        if Self::from_note(self.channel, self.id).is_some() {
            let bank = if self.id < 44 { 'A' } else { 'B' };
            format!("Bank {bank} Pad {}", (self.id - 36) % 8 + 1)
        } else {
            format!("Note channel {} note {}", self.channel, self.id)
        }
    }
}

#[derive(Debug, PartialEq)]
enum Event {
    Press(Control),
    Release(Control, ReleaseType),
    DuplicatePress(Control),
    UnmatchedRelease(Control),
    Pressure(Control),
    Pulse(Control),
}

#[derive(Debug, PartialEq)]
enum ReleaseType {
    NoteOff,
    NoteOnVelocityZero,
    Neutral,
    PulseComplete,
}

#[derive(Default)]
struct Detector {
    down: HashSet<Control>,
    pressure_seen: HashSet<Control>,
}

impl Detector {
    fn observe(&mut self, bytes: [u8; 3]) -> Option<Event> {
        let [status, note, value] = bytes;
        let kind = status & 0xF0;
        let channel = (status & 0x0F) + 1;
        let pad = Control::note(channel, note)?;

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
            "Usage: vibeconsole [--paused|detect|list|get|configure|run|feedback-read]\n\
                  no subcommand: detect the current controller program with running intent\n\
                  --paused: open the session explicitly paused\n\
                  detect: show live input bulbs and MIDI events in the terminal\n\
                  list: display saved assignments without opening MIDI\n\
                  get: inspect a Note control in original context (use /prog-select in the session for others)\n\
                  configure: press a pad, inspect, then assign or replace it\n\
                  run: start mappings in the interactive session; Ctrl+C releases and exits\n\
                  feedback-read: validate Program 0 snapshot without changing settings\n\
                  agent-status <agent|-> <idle|thinking|needs-input|finished>: report agent state (for hooks)\n\
                  setup: install, uninstall, doctor; startup-enable/start/stop/status/disable
\
                  startup-enable: opt into login startup (old flags are accepted and ignored)\n\
                  daemon: native user service (old flags are accepted and ignored)
\
                  The default session, Run and opted-in daemon run mappings; selected Flow actions create a paused keyboard."
        );
        return Ok(());
    }
    // Called from agent hooks while the session or service runs: no MIDI, keyboard, or owner lock.
    if args.first().is_some_and(|s| s == "agent-status") {
        return status::command(&args[1..]);
    }
    if let Some(command) = args.first().filter(|s| {
        matches!(
            s.as_str(),
            "install"
                | "uninstall"
                | "startup-enable"
                | "startup-start"
                | "startup-stop"
                | "startup-disable"
                | "startup-status"
                | "doctor"
        )
    }) {
        return runtime::command(command, &args[1..]);
    }
    if args.first().is_some_and(|s| s == "daemon") {
        if args[1..]
            .iter()
            .any(|s| s != "--feedback" && s != "--start-flow")
        {
            return Err("daemon flags: --feedback, --start-flow".into());
        }
        if args.len() > 1 {
            eprintln!(
                "Legacy daemon flags ignored; saved Flow setting and verified program now control startup."
            );
        }
        let _owner = runtime::Owner::acquire()?;
        let path = config_path()?;
        return terminal::daemon(&path, load(&path)?);
    }
    if args.len() > 1
        || args.first().is_some_and(|arg| {
            !matches!(
                arg.as_str(),
                "--paused" | "detect" | "list" | "get" | "configure" | "run" | "feedback-read"
            )
        })
    {
        return Err("unknown command; use vibeconsole --help".into());
    }
    let path = config_path()?;
    let mappings = load(&path)?;
    let _owner = if args == ["list"] {
        None
    } else {
        Some(runtime::Owner::acquire()?)
    };
    if args.is_empty() || args == ["configure"] || args == ["--paused"] || args == ["detect"] {
        return terminal::start(
            &path,
            mappings,
            args.first().map(String::as_str).unwrap_or("session"),
        );
    }
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
        "feedback-read" => feedback::read_only(),
        "detect" => unreachable!("handled by terminal session"),
        "run" => terminal::start(&path, mappings, "run"),
        "get" => {
            let pad = learn_pad()?;
            match mappings
                .iter()
                .find(|mapping| mapping.context == 0 && mapping.control == pad)
            {
                Some(mapping) => println!("Current assignment: {mapping}"),
                None => println!(
                    "{} has no assignment.",
                    mappings::control_name(pad, None, 0, None)
                ),
            }
            Ok(())
        }
        _ => unreachable!("validated command"),
    }
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

    Ok(port)
}

fn learn_pad() -> Result<Control> {
    let port = input_port()?;
    println!("Press a pad in either bank to learn it. No shortcuts are generated; Ctrl+C cancels.");
    let mut child = midi::command(&port).spawn()?;
    let stdout = child.stdout.take().expect("piped MIDI input");
    let result = (|| -> Result<Control> {
        let mut detector = Detector::default();
        for message in midi::messages(stdout) {
            if let Some(Event::Press(pad)) = detector.observe(message?) {
                println!(
                    "Learned: {}",
                    describe_event(&Event::Press(pad), |c| mappings::control_name(
                        c, None, 0, None
                    ))
                );
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

fn describe_event(event: &Event, name: impl Fn(Control) -> String) -> String {
    let (pad, action) = match event {
        Event::Press(pad) => (pad, "PRESS: Note On"),
        Event::Release(pad, ReleaseType::NoteOff) => (pad, "RELEASE: Note Off"),
        Event::Release(pad, ReleaseType::NoteOnVelocityZero) => {
            (pad, "RELEASE: Note On velocity 0")
        }
        Event::Release(pad, ReleaseType::Neutral) => {
            (pad, "RELEASE: joystick neutral region / prior direction")
        }
        Event::Release(pad, ReleaseType::PulseComplete) => {
            (pad, "RELEASE: shortcut pulse complete")
        }
        Event::DuplicatePress(pad) => (pad, "REPEAT IGNORED: pad already down"),
        Event::UnmatchedRelease(pad) => (pad, "RELEASE IGNORED: pad was not down"),
        Event::Pulse(pad) => (pad, "PULSE: movement step"),
        Event::Pressure(pad) => (pad, "PRESSURE IGNORED: does not change pad state"),
    };
    format!(
        "{action} | {} | MIDI {} channel {} | id {} (0x{:02X})",
        name(*pad),
        pad.message.text(),
        pad.channel,
        pad.id,
        pad.id
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
                assert_eq!(pad.label(), "Bank B Pad 1");
            }
            event => panic!("expected Bank B Pad 1 press, got {event:?}"),
        }
    }
}
