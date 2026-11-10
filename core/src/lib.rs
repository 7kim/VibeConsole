//! VibeConsole core: devices, the keyboard state machine, mappings and config, actions,
//! status sources and Akai feedback. Both editions (cli, app) call it.
pub mod actions;
pub mod feedback;
pub mod keyboard;
pub mod mappings;
pub mod midi;
pub mod motion;
pub mod recording;
pub mod screen;
pub mod status;

use std::{collections::HashSet, process::Command};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub const PAD_CHANNEL: u8 = 10;
pub const DEVICE: &str = "09e8:1049";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Message {
    Note,
    Cc,
    Bend,
}
impl Message {
    pub fn text(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Cc => "cc",
            Self::Bend => "bend",
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
    pub fn validate(self) -> Result<()> {
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
    pub fn label(self) -> String {
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

pub fn input_port() -> Result<String> {
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
