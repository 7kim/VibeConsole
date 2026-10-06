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

pub fn messages(reader: impl Read) -> impl Iterator<Item = io::Result<[u8; 3]>> {
    let mut bytes = io::BufReader::new(reader).bytes();
    let mut status = 0;
    let mut data = [0; 2];
    let mut received = 0;
    std::iter::from_fn(move || {
        loop {
            let byte = match bytes.next()? {
                Ok(byte) => byte,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Some(Err(error)),
            };
            if byte >= 0xF8 {
                // Realtime bytes can occur inside any packet without altering running status.
                continue;
            }
            if byte >= 0x80 {
                // System common/SysEx cancels running status; its data is ignored until a channel status.
                status = if byte < 0xF0 { byte } else { 0 };
                received = 0;
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
                if matches!(status & 0xF0, 0x80 | 0x90 | 0xA0) {
                    return Some(Ok([status, data[0], data[1]]));
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
