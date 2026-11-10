use std::io::{self, Read};
use std::process::{Command, Stdio};

pub fn command(port: &str) -> Command {
    let mut command = Command::new("amidi");
    // Read packets directly: --dump starts the next line only when another message arrives.
    command
        .args(["--receive=/dev/stdout", "--port", port])
        .stdout(Stdio::piped());
    command
}

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

// Own the child and reader together so cancellation cannot leave an amidi process behind.
pub struct Input {
    child: std::process::Child,
    reader: Option<std::thread::JoinHandle<()>>,
    receiver: std::sync::mpsc::Receiver<io::Result<[u8; 3]>>,
    node: String,
    identity: (u64, u64, u64),
    pub port: String,
    pub sysex: Option<std::sync::mpsc::Receiver<std::result::Result<Vec<u8>, String>>>,
}

impl Input {
    pub fn open() -> crate::Result<Self> {
        use std::os::unix::{
            fs::{MetadataExt, OpenOptionsExt},
            process::CommandExt,
        };
        let port = crate::input_port()?;
        let fields: Vec<_> = port
            .strip_prefix("hw:")
            .ok_or("unexpected ALSA port")?
            .split(',')
            .collect();
        let [card, device, _] = fields.as_slice() else {
            return Err("unexpected ALSA port".into());
        };
        let node = format!("/dev/snd/midiC{card}D{device}");
        let meta = std::fs::metadata(&node)?;
        // Check permissions and port occupancy before spawning the long-lived reader.
        // Close this descriptor before amidi acquires the input side.
        let probe = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&node)
            .map_err(|error| {
                format!("cannot open MIDI input {node} (permissions/port occupancy): {error}")
            })?;
        drop(probe);

        let mut command = command(&port);
        command.process_group(0); // handled parent signals must leave input alive for restoration
        let parent = std::process::id();
        // SAFETY: only async-signal-safe syscalls between fork and exec.
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
        // Bound queued packets; try_send avoids a blocked reader during shutdown.
        let (sender, receiver) = std::sync::mpsc::sync_channel(1024);
        let (sysex_sender, sysex) = std::sync::mpsc::sync_channel(16);
        let reader = std::thread::spawn(move || {
            for packet in packets(stdout) {
                match packet {
                    Ok(Packet::Channel(note)) => {
                        if sender.try_send(Ok(note)).is_err() {
                            break;
                        }
                    }
                    Ok(Packet::SysEx(frame)) => {
                        if sysex_sender.try_send(frame).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.try_send(Err(error));
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            reader: Some(reader),
            receiver,
            node,
            identity: (meta.dev(), meta.ino(), meta.rdev()),
            port,
            sysex: Some(sysex),
        })
    }

    pub fn output(&self) -> crate::Result<std::fs::File> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.node)?;
        let meta = file.metadata()?;
        if (meta.dev(), meta.ino(), meta.rdev()) != self.identity {
            return Err("MIDI device changed while opening output".into());
        }
        Ok(file)
    }

    pub fn next(&self) -> crate::Result<Option<[u8; 3]>> {
        use std::os::unix::fs::MetadataExt;
        if !std::fs::metadata(&self.node)
            .is_ok_and(|meta| (meta.dev(), meta.ino(), meta.rdev()) == self.identity)
        {
            return Err("MIDI device disconnected".into());
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

impl Drop for Input {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
