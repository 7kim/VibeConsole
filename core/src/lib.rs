//! VibeConsole core: devices, the keyboard state machine, mappings and config, actions,
//! status sources and Akai feedback. Both editions (cli, app) call it.
pub mod actions;
pub mod config;
pub mod feedback;
pub mod gamepad;
pub mod keyboard;
pub mod mappings;
pub mod midi;
pub mod motion;
pub mod owner;
pub mod recording;
pub mod screen;
pub mod status;
pub mod usage;
pub mod wake;

/// v7.8 ticket 01: the folder of an emulated run (`vibeconsole --emulated <dir>`), set once
/// at startup. While set, only emulator devices are picked, keys, clicks and scrolls go to
/// `<dir>/output.log`, external programs are recorded in `<dir>/tools.log` instead of run,
/// and nothing is written to a real device.
static EMULATED: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
pub fn set_emulated(dir: std::path::PathBuf) {
    let _ = EMULATED.set(dir);
}
pub fn emulated() -> Option<&'static std::path::Path> {
    EMULATED.get().map(std::path::PathBuf::as_path)
}
/// The name every emulated device carries (an ALSA port, a uinput gamepad).
pub const EMULATOR: &str = "VibeConsole emulator";
pub fn is_emulator(name: &str) -> bool {
    name.to_lowercase().contains(&EMULATOR.to_lowercase())
}
/// A device this run may use: emulators in an emulated run, never one otherwise.
pub fn ours(name: &str) -> bool {
    emulated().is_some() == is_emulator(name)
}
/// v7.8 ticket 01: every external program VibeConsole starts. In an emulated run it is this
/// binary's own recorder (`emulated-tool`), which writes the call to `<dir>/tools.log` and
/// runs nothing.
pub fn program(name: impl AsRef<std::ffi::OsStr>) -> Command {
    match emulated() {
        None => Command::new(name),
        Some(dir) => {
            let mut command = Command::new("/proc/self/exe");
            command.arg("emulated-tool").arg(dir).arg(name);
            command
        }
    }
}
/// One emulated-run output line appended to `<dir>/<file>`; outside an emulated run, nothing.
pub fn emulated_log(file: &str, line: &str) {
    use std::io::Write;
    if let Some(dir) = emulated() {
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(file))
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(error) = written {
            eprintln!("emulated run: {file}: {error}");
        }
    }
}

use std::{collections::HashSet, process::Command};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub const PAD_CHANNEL: u8 = 10;
pub const DEVICE: &str = "09e8:1049";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Message {
    Note,
    Cc,
    Bend,
    /// An Xbox controller button (v7 ticket 07); `id` indexes `gamepad::BUTTONS`.
    Button,
}
impl Message {
    pub fn text(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Cc => "cc",
            Self::Bend => "bend",
            Self::Button => "button",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Control {
    pub channel: u8,
    pub id: u8,
    pub message: Message,
    pub direction: i8,
}

impl Control {
    pub fn from_note(channel: u8, note: u8) -> Option<Self> {
        (channel == PAD_CHANNEL && (36..=51).contains(&note)).then_some(Self {
            channel,
            id: note,
            message: Message::Note,
            direction: 0,
        })
    }
    pub fn note(channel: u8, note: u8) -> Option<Self> {
        ((1..=16).contains(&channel) && note <= 127).then_some(Self {
            channel,
            id: note,
            message: Message::Note,
            direction: 0,
        })
    }
    /// An Xbox controller button (channel 1, no direction).
    pub fn button(id: u8) -> Self {
        Self {
            channel: 1,
            id,
            message: Message::Button,
            direction: 0,
        }
    }
    pub fn validate(self) -> Result<()> {
        // Xbox controls are bounded by `gamepad::CONTROLS` (gestures go past 127, v7 ticket
        // 76); MIDI ids are 7-bit.
        if !(1..=16).contains(&self.channel)
            || (self.message != Message::Button && self.id > 127)
            || match self.message {
                Message::Note => self.direction != 0,
                Message::Button => {
                    self.channel != 1
                        || self.direction != 0
                        || usize::from(self.id) >= gamepad::CONTROLS
                }
                Message::Cc => ![-1, 1].contains(&self.direction),
                Message::Bend => self.id != 0 || ![-1, 1].contains(&self.direction),
            }
        {
            return Err("invalid control identity".into());
        }
        Ok(())
    }
    pub fn label(self) -> String {
        if self.message == Message::Button {
            return format!("Xbox {}", gamepad::button_name(self.id));
        }
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
pub enum Event {
    Press(Control),
    Release(Control, ReleaseType),
    DuplicatePress(Control),
    UnmatchedRelease(Control),
    Pressure(Control),
    Pulse(Control),
}

#[derive(Debug, PartialEq)]
pub enum ReleaseType {
    NoteOff,
    NoteOnVelocityZero,
    Neutral,
    PulseComplete,
    /// An Xbox button let go.
    ButtonUp,
}

#[derive(Default)]
pub struct Detector {
    pub down: HashSet<Control>,
    pressure_seen: HashSet<Control>,
}

impl Detector {
    pub fn observe(&mut self, bytes: [u8; 3]) -> Option<Event> {
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

/// The Akai is on USB (`lsusb -d 09e8:1049`), checked before any MIDI port is opened.
pub fn akai_connected() -> Result<()> {
    midi::real_midi()?; // v7.7 ticket 258: not even lsusb in tests
    if emulated().is_some() {
        return Ok(()); // v7.8 ticket 01: the emulator's port is found by name below
    }
    let usb = Command::new("lsusb").args(["-d", DEVICE]).output()?;
    if !usb.status.success() {
        return Err("AKAI MPK mini 3 (USB 09e8:1049) is unavailable; check lsusb".into());
    }
    if String::from_utf8(usb.stdout)?.trim().is_empty() {
        return Err("AKAI MPK mini 3 (USB 09e8:1049) is not connected".into());
    }
    Ok(())
}

/// The Akai's MIDI port name, without opening it (`doctor`).
pub fn input_port() -> Result<String> {
    akai_connected()?;
    let midi = midir::MidiInput::new("VibeConsole probe")
        .map_err(|e| format!("MIDI input unavailable: {e}"))?;
    Ok(midi::akai_port(&midi)?.1)
}
