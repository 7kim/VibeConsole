use std::io::{self, Read};

#[derive(Debug, PartialEq)]
pub enum Packet {
    Channel([u8; 3]),
    SysEx(std::result::Result<Vec<u8>, String>),
}

pub fn packets(reader: impl Read) -> impl Iterator<Item = io::Result<Packet>> {
    let mut bytes = io::BufReader::new(reader).bytes();
    let mut status = 0;
    let mut data = [0; 2];
    let mut received = 0;
    let mut sysex: Option<Vec<u8>> = None;
    let mut discarded = false;
    std::iter::from_fn(move || {
        loop {
            let byte = match bytes.next() {
                Some(Ok(byte)) => byte,
                Some(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                Some(Err(error)) => return Some(Err(error)),
                None => {
                    return sysex
                        .take()
                        .map(|_| Ok(Packet::SysEx(Err("truncated SysEx".into()))));
                }
            };
            if byte >= 0xF8 {
                continue;
            }
            if let Some(frame) = &mut sysex {
                if byte == 0xF7 {
                    frame.push(byte);
                    return Some(Ok(Packet::SysEx(Ok(sysex.take().unwrap()))));
                }
                if byte < 0x80 {
                    if frame.len() < 255 {
                        frame.push(byte);
                        continue;
                    }
                    sysex = None;
                    discarded = true;
                    return Some(Ok(Packet::SysEx(Err(
                        "oversized SysEx (maximum 256 bytes)".into(),
                    ))));
                }
                sysex = None;
                status = if byte < 0xF0 { byte } else { 0 };
                received = 0;
                if byte == 0xF0 {
                    sysex = Some(vec![byte]);
                }
                return Some(Ok(Packet::SysEx(Err("interrupted/malformed SysEx".into()))));
            }
            if byte == 0xF0 {
                status = 0;
                received = 0;
                discarded = false;
                sysex = Some(vec![byte]);
                continue;
            }
            if byte >= 0x80 {
                discarded = false;
                status = if byte < 0xF0 { byte } else { 0 };
                received = 0;
                continue;
            }
            if discarded {
                continue;
            }
            let length = match status & 0xF0 {
                0x80..=0xB0 | 0xE0 => 2,
                0xC0 | 0xD0 => 1,
                _ => continue,
            };
            data[received] = byte;
            received += 1;
            if received == length {
                received = 0;
                return Some(Ok(Packet::Channel([
                    status,
                    data[0],
                    if length == 1 { 0 } else { data[1] },
                ])));
            }
        }
    })
}

/// One complete message from midir (one callback) as packets: realtime bytes dropped,
/// SysEx bounded and checked by the same parser as a byte stream.
pub fn message_packets(message: &[u8]) -> Vec<Packet> {
    packets(message).filter_map(Result::ok).collect()
}

pub fn messages(reader: impl Read) -> impl Iterator<Item = io::Result<[u8; 3]>> {
    packets(reader).filter_map(|packet| match packet {
        Ok(Packet::Channel(note)) => Some(Ok(note)),
        Err(error) => Some(Err(error)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v7 ticket 06: a device is named without its sequencer id (which changes on replug);
    /// only the Akai counts as the Akai, and system ports are never offered.
    #[test]
    fn devices_are_named_without_their_port_id_and_only_the_akai_is_writable() {
        assert_eq!(
            device_name("Launchkey Mini MK3:Launchkey Mini MK3 MIDI 1 24:0"),
            "Launchkey Mini MK3:Launchkey Mini MK3 MIDI 1"
        );
        assert_eq!(device_name("plain name"), "plain name");
        assert!(is_akai("MPK mini 3:MPK mini 3 MIDI 1 16:0"));
        assert!(!is_akai(
            "Launchkey Mini MK3:Launchkey Mini MK3 MIDI 1 24:0"
        ));
        assert!(offered("Launchkey Mini MK3:Launchkey Mini MK3 MIDI 1 24:0"));
        for system in [
            "Midi Through:Midi Through Port-0 14:0",
            "MPK mini 3:MPK mini 3 MIDI 1 16:0",
            "VibeConsole:VibeConsole input 128:0",
        ] {
            assert!(!offered(system), "{system}");
        }
        assert!(akai_only(true).is_ok());
        assert!(
            akai_only(false)
                .unwrap_err()
                .to_string()
                .contains("only to the Akai")
        );
    }

    /// v7 ticket 05: midir hands over one complete message per callback.
    #[test]
    fn one_midir_message_becomes_its_packet() {
        assert_eq!(
            message_packets(&[0x99, 36, 100]),
            [Packet::Channel([0x99, 36, 100])]
        );
        // Two-byte messages (program change) keep the parser's padding.
        assert_eq!(message_packets(&[0xC9, 3]), [Packet::Channel([0xC9, 3, 0])]);
        assert_eq!(
            message_packets(&[0xF0, 0x47, 0, 0x49, 0xF7]),
            [Packet::SysEx(Ok(vec![0xF0, 0x47, 0, 0x49, 0xF7]))]
        );
        let mut large = vec![0xF0];
        large.extend([0; 256]);
        large.push(0xF7);
        assert!(matches!(
            &message_packets(&large)[..],
            [Packet::SysEx(Err(_))]
        ));
        // A SysEx without its end byte is reported, never passed on as complete.
        assert!(matches!(
            &message_packets(&[0xF0, 0x47])[..],
            [Packet::SysEx(Err(_))]
        ));
        // Clock and active sensing are dropped.
        assert!(message_packets(&[0xF8]).is_empty());
        assert!(message_packets(&[0xFE]).is_empty());
    }

    #[test]
    fn sysex_is_bounded_and_never_swallows_an_interleaved_release() {
        let input: &[u8] = &[0xF0, 0x47, 0, 0x49, 0xF8, 0xF7, 0x89, 36, 0];
        let output = packets(input).collect::<io::Result<Vec<_>>>().unwrap();
        assert_eq!(
            output[0],
            Packet::SysEx(Ok(vec![0xF0, 0x47, 0, 0x49, 0xF7]))
        );
        assert_eq!(output[1], Packet::Channel([0x89, 36, 0]));
        let mut large = vec![0xF0];
        large.extend([0; 256]);
        large.extend([0xF7, 0x89, 36, 0]);
        let output = packets(large.as_slice())
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        assert!(matches!(&output[0], Packet::SysEx(Err(_))));
        assert_eq!(output[1], Packet::Channel([0x89, 36, 0]));
        let interrupted: &[u8] = &[0xF0, 0x47, 0x89, 36, 0];
        let output = packets(interrupted)
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        assert!(matches!(&output[0], Packet::SysEx(Err(_))));
        assert_eq!(output[1], Packet::Channel([0x89, 36, 0]));
        assert!(matches!(
            packets(&[0xF0, 0x47][..]).next().unwrap().unwrap(),
            Packet::SysEx(Err(_))
        ));
    }

    #[test]
    fn running_status_realtime_system_messages_and_fragmented_reads() {
        let bytes: &[u8] = &[
            36, 20, // Orphan data must not create a message.
            0x99, 36, 0xF8, 20, 37, 0xFE, 0, // Running status and velocity-zero release.
            0xC9, 8, 9, 0xD9, 20, 0xB9, 1, 36, 0xE9, 36, 0, // Non-pad channel messages.
            0xF0, 36, 20, 0xF8, 0xF7, 36, 20, // SysEx and orphan data.
            0xF1, 36, 20, 0xF2, 36, 20, 0xF3, 36, 0xF6, 36, 20, 0x89, 36, 0, 0xA9, 36, 40, 0x99,
            36, 0x89, 36, 0, // New status discards partial packet.
            0x99, 36, // Truncated EOF must not fabricate a message.
        ];
        let expected = [
            [0x99, 36, 20],
            [0x99, 37, 0],
            [0xC9, 8, 0],
            [0xC9, 9, 0],
            [0xD9, 20, 0],
            [0xB9, 1, 36],
            [0xE9, 36, 0],
            [0x89, 36, 0],
            [0xA9, 36, 40],
            [0x89, 36, 0],
        ];
        assert_eq!(
            messages(bytes).collect::<io::Result<Vec<_>>>().unwrap(),
            expected
        );
        struct OneByte<'a>(&'a [u8]);
        impl Read for OneByte<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.0.read(&mut buffer[..1])
            }
        }
        assert_eq!(
            messages(OneByte(bytes))
                .collect::<io::Result<Vec<_>>>()
                .unwrap(),
            expected
        );
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("MIDI read failed"))
            }
        }
        assert!(messages(Broken).next().unwrap().is_err());
    }
}

/// The Akai's port on the ALSA sequencer, found by name through midir. Exactly one must
/// match, so a second Akai (or a renamed port) is never guessed.
pub fn akai_port<T: midir::MidiIO>(midi: &T) -> crate::Result<(T::Port, String)> {
    let mut matches = midi
        .ports()
        .into_iter()
        .filter_map(|port| {
            let name = midi.port_name(&port).ok()?;
            is_akai(&name).then_some((port, name))
        })
        .collect::<Vec<_>>();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err("MPK mini 3 is connected, but no matching MIDI port is available".into()),
        _ => Err("multiple MPK mini 3 MIDI ports found; refusing ambiguous selection".into()),
    }
}

pub fn is_akai(port: &str) -> bool {
    port.to_lowercase().contains("mpk mini 3")
}
/// A port's device name: its ALSA name without the trailing `client:port` id, which
/// changes on every replug.
pub fn device_name(port: &str) -> String {
    match port.rsplit_once(' ') {
        Some((name, id))
            if id.split_once(':').is_some_and(|(c, p)| {
                [c, p]
                    .iter()
                    .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            }) =>
        {
            name.to_owned()
        }
        _ => port.to_owned(),
    }
}
/// Ports offered as "other MIDI devices": not the Akai, ALSA's Midi Through or ours.
pub fn offered(port: &str) -> bool {
    let lower = port.to_lowercase();
    !is_akai(port) && !lower.contains("midi through") && !lower.starts_with("vibeconsole")
}
/// The one gate for device-specific messages (SysEx feedback): only the Akai gets them.
pub fn akai_only(akai: bool) -> crate::Result<()> {
    if akai {
        Ok(())
    } else {
        Err("device-specific messages go only to the Akai MPK mini 3; this device gets none".into())
    }
}
/// Connected other MIDI devices, by device name.
pub fn other_devices() -> crate::Result<Vec<String>> {
    let midi = midir::MidiInput::new("VibeConsole probe")
        .map_err(|e| format!("MIDI input unavailable: {e}"))?;
    let mut names = midi
        .ports()
        .iter()
        .filter_map(|port| midi.port_name(port).ok())
        .filter(|name| offered(name))
        .map(|name| device_name(&name))
        .collect::<Vec<_>>();
    names.dedup();
    Ok(names)
}

/// Bounded queues fed by the midir callback. A full queue drops both senders, so `next`
/// reports the overflow instead of silently losing a release.
type Senders = Option<(
    std::sync::mpsc::SyncSender<io::Result<[u8; 3]>>,
    std::sync::mpsc::SyncSender<std::result::Result<Vec<u8>, String>>,
)>;

pub struct Input {
    // Field order: the connection closes before the queues it feeds.
    _connection: midir::MidiInputConnection<Senders>,
    receiver: std::sync::mpsc::Receiver<io::Result<[u8; 3]>>,
    /// Lists ports for the disconnect check; never connected.
    probe: midir::MidiInput,
    /// The sequencer `client:port` id; a replugged device gets a new one.
    id: String,
    checked: std::cell::Cell<std::time::Instant>,
    pub port: String,
    /// Only the Akai receives device-specific messages (`output`).
    pub akai: bool,
    pub sysex: Option<std::sync::mpsc::Receiver<std::result::Result<Vec<u8>, String>>>,
}

impl Input {
    pub fn open() -> crate::Result<Self> {
        Self::open_device(None)
    }
    /// The Akai (`None`) or another MIDI device by its device name.
    pub fn open_device(device: Option<&str>) -> crate::Result<Self> {
        if device.is_none() {
            crate::akai_connected()?;
        }
        let mut midi = midir::MidiInput::new("VibeConsole")
            .map_err(|e| format!("MIDI input unavailable: {e}"))?;
        // SysEx carries the controller's replies; nothing is filtered by midir.
        midi.ignore(midir::Ignore::None);
        let (port, name) = match device {
            None => akai_port(&midi)?,
            Some(device) => {
                let mut matches = midi
                    .ports()
                    .into_iter()
                    .filter_map(|port| {
                        let name = midi.port_name(&port).ok()?;
                        (offered(&name) && device_name(&name) == device).then_some((port, name))
                    })
                    .collect::<Vec<_>>();
                match matches.len() {
                    1 => matches.remove(0),
                    0 => return Err(format!("MIDI device {device} is not connected").into()),
                    _ => {
                        return Err(format!(
                            "several ports named {device}; refusing ambiguous selection"
                        )
                        .into());
                    }
                }
            }
        };
        let id = port.id();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1024);
        let (sysex_sender, sysex) = std::sync::mpsc::sync_channel(16);
        let connection = midi
            .connect(
                &port,
                "VibeConsole input",
                |_, message, senders: &mut Senders| {
                    let Some((notes, frames)) = senders else {
                        return;
                    };
                    for packet in message_packets(message) {
                        let sent = match packet {
                            Packet::Channel(note) => notes.try_send(Ok(note)).is_ok(),
                            Packet::SysEx(frame) => frames.try_send(frame).is_ok(),
                        };
                        if !sent {
                            *senders = None;
                            return;
                        }
                    }
                },
                Some((sender, sysex_sender)),
            )
            .map_err(|e| format!("cannot open MIDI input {name}: {e}"))?;
        Ok(Self {
            _connection: connection,
            receiver,
            probe: midir::MidiInput::new("VibeConsole probe")
                .map_err(|e| format!("MIDI input unavailable: {e}"))?,
            id,
            checked: std::cell::Cell::new(std::time::Instant::now()),
            akai: device.is_none(),
            port: name,
            sysex: Some(sysex),
        })
    }

    /// Output to the same sequencer port as the input, so a replaced device can't receive
    /// a stale restoration.
    pub fn output(&self) -> crate::Result<Output> {
        akai_only(self.akai)?;
        let midi = midir::MidiOutput::new("VibeConsole")
            .map_err(|e| format!("MIDI output unavailable: {e}"))?;
        let port = midi
            .find_port_by_id(&self.id)
            .ok_or("MIDI device changed while opening output")?;
        Ok(Output(midi.connect(&port, "VibeConsole output").map_err(
            |e| format!("cannot open MIDI output {}: {e}", self.port),
        )?))
    }

    pub fn next(&self) -> crate::Result<Option<[u8; 3]>> {
        // ponytail: re-enumerating ports every 250 ms; an ALSA announce subscription would
        // report unplugging at once if that delay ever matters.
        if self.checked.get().elapsed() >= std::time::Duration::from_millis(250) {
            self.checked.set(std::time::Instant::now());
            if self.probe.find_port_by_id(&self.id).is_none() {
                return Err("MIDI device disconnected".into());
            }
        }
        match self.receiver.try_recv() {
            Ok(message) => Ok(Some(message?)),
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("MIDI input ended or queue overflowed; check device/port".into())
            }
        }
    }
}

/// An open MIDI output (midir sends each message whole, SysEx included).
pub struct Output(midir::MidiOutputConnection);
impl Output {
    pub fn send(&mut self, message: &[u8]) -> crate::Result<()> {
        self.0
            .send(message)
            .map_err(|e| format!("MIDI transmission failed: {e}").into())
    }
}
