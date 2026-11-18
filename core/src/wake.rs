//! v7.7 ticket 225: one pipe that ends the terminal's wait for a key as soon as a MIDI or
//! Xbox event arrives, instead of after up to 50 ms. Only a terminal run asks for it
//! (`fd`); until then `wake` does nothing, so the login service never fills it.

use std::os::fd::RawFd;
use std::sync::OnceLock;

/// The pipe's read and write ends, made non-blocking; None if the pipe couldn't be made.
static PIPE: OnceLock<Option<[RawFd; 2]>> = OnceLock::new();

/// The read end to poll beside stdin. The first call makes the pipe and starts the Xbox
/// watcher: a thread with its own read-only gilrs that blocks until any controller event
/// (none while idle, so no busy loop) and wakes the wait.
pub fn fd() -> Option<RawFd> {
    PIPE.get_or_init(|| {
        let mut ends = [0; 2];
        // SAFETY: two valid ints for pipe2 to fill; no ownership transfer.
        if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
            return None;
        }
        #[cfg(not(test))]
        std::thread::spawn(|| {
            // Its own instance: the session's Pad keeps reading every event itself.
            let Ok(mut gilrs) = gilrs::Gilrs::new() else {
                return;
            };
            loop {
                if gilrs.next_event_blocking(None).is_some() {
                    wake();
                }
            }
        });
        Some(ends)
    })
    .map(|[read, _]| read)
}

/// One byte into the pipe, if a terminal asked for it. A full pipe already wakes it.
pub fn wake() {
    if let Some(Some([_, write])) = PIPE.get() {
        // SAFETY: one byte from valid storage into our own non-blocking pipe.
        let _ = unsafe { libc::write(*write, [1u8].as_ptr().cast(), 1) };
    }
}

/// Empty the pipe after a wake, so the next wait blocks again.
pub fn drain() {
    if let Some(Some([read, _])) = PIPE.get() {
        let mut buffer = [0u8; 256];
        // SAFETY: reads into valid storage from our own non-blocking pipe; stops at empty.
        while unsafe { libc::read(*read, buffer.as_mut_ptr().cast(), buffer.len()) } > 0 {}
    }
}

#[cfg(test)]
mod tests {
    /// A wake makes the read end readable at once; draining makes it wait again.
    #[test]
    fn a_wake_ends_the_wait_and_a_drain_resets_it() {
        let fd = super::fd().expect("pipe");
        let ready = |timeout| {
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd.
            unsafe { libc::poll(&mut poll, 1, timeout) == 1 }
        };
        super::drain();
        assert!(!ready(0), "nothing pending");
        super::wake();
        assert!(ready(0), "woken at once");
        super::drain();
        assert!(!ready(0), "drained");
    }
}
