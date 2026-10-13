use crate::{Result, midi};
use std::fmt;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const OFFSETS: [usize; 4] = [0x13, 0x14, 0x1B, 0x1C];
const QUERY: [u8; 9] = [0xF0, 0x47, 0x7F, 0x49, 0x66, 0, 1, 0, 0xF7];
pub type Payload = [u8; 245];
type Reply = std::result::Result<Vec<u8>, String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct State {
    pub octave: i8,
    pub tempo: u16,
    pub arp: bool,
}
impl State {
    pub fn validate(self) -> Result<Self> {
        if !(-4..=4).contains(&self.octave) || !(1..=300).contains(&self.tempo) {
            return Err("octave must be -4..+4 and tempo 1..300 BPM".into());
        }
        Ok(self)
    }
    fn read(payload: &Payload) -> Result<Self> {
        if payload[0x13] > 8 || payload[0x14] > 1 {
            return Err("invalid octave/arpeggiator fields".into());
        }
        Self {
            octave: payload[0x13] as i8 - 4,
            tempo: (payload[0x1B] as u16) << 7 | payload[0x1C] as u16,
            arp: payload[0x14] == 1,
        }
        .validate()
    }
}
impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "octave {:+}, tempo {} BPM ({}), arpeggiator {}",
            self.octave,
            self.tempo,
            if self.arp {
                "blink requested"
            } else {
                "stored, not blinking"
            },
            if self.arp { "On" } else { "Off" }
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Settings {
    pub octave: Option<i8>,
    pub tempo: Option<u16>,
    pub arp: Option<bool>,
}
impl Settings {
    pub fn validate(self) -> Result<Self> {
        if self.octave.is_some_and(|n| !(-4..=4).contains(&n))
            || self.tempo.is_some_and(|n| !(1..=300).contains(&n))
        {
            return Err("invalid feedback: octave -4..+4, tempo 1..300".into());
        }
        Ok(self)
    }
    pub fn enabled(self) -> bool {
        self != Self::default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Octave {
    Offset(i8),
    Absolute(i8),
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Feedback {
    pub octave: Option<Octave>,
    pub tempo: Option<u16>,
    pub arp: Option<bool>,
}
impl Feedback {
    pub fn validate(self) -> Result<()> {
        let octave = self.octave.map(|value| match value {
            Octave::Offset(n) | Octave::Absolute(n) => n,
        });
        Settings {
            octave,
            tempo: self.tempo,
            arp: self.arp,
        }
        .validate()?;
        if self.tempo.is_some() && self.arp == Some(false) {
            return Err("tempo blinking requires arpeggiator On".into());
        }
        Ok(())
    }
    pub fn enabled(self) -> bool {
        self != Self::default()
    }
}
impl fmt::Display for Feedback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if let Some(value) = self.octave {
            parts.push(match value {
                Octave::Offset(n) => format!("Octave offset {n:+}"),
                Octave::Absolute(n) => format!("Absolute octave {n:+}"),
            });
        }
        if let Some(bpm) = self.tempo {
            parts.push(format!("Tempo {bpm} BPM"));
        }
        if let Some(arp) = self.arp {
            parts.push(format!("Arpeggiator {}", if arp { "On" } else { "Off" }));
        }
        f.write_str(&if parts.is_empty() {
            "None".into()
        } else {
            parts.join(", ")
        })
    }
}

pub struct Resolver {
    mappings: Vec<crate::mappings::Mapping>,
    order: Vec<crate::Control>,
    baseline: State,
    idle: Settings,
    scheduled: Option<Settings>,
}
impl Resolver {
    pub fn new(
        mappings: &[crate::mappings::Mapping],
        baseline: State,
        idle: Settings,
    ) -> Result<Self> {
        baseline.validate()?;
        idle.validate()?;
        for mapping in mappings {
            mapping.validate()?;
        }
        Ok(Self {
            mappings: mappings.to_vec(),
            order: Vec::new(),
            baseline,
            idle,
            scheduled: None,
        })
    }
    pub fn transition(&mut self, pad: crate::Control, active: bool) -> Result<Option<Settings>> {
        self.order.retain(|p| *p != pad);
        if active {
            self.order.push(pad);
        }
        self.update()
    }
    pub fn clear(&mut self) -> Result<Option<Settings>> {
        self.order.clear();
        self.update()
    }
    fn effective(&self) -> Result<Settings> {
        let mut result = Settings::default();
        let mut active_tempo = false;
        for pad in self.order.iter().rev() {
            let mapping = self
                .mappings
                .iter()
                .find(|m| m.control == *pad)
                .ok_or("unknown active feedback owner")?;
            let feedback = mapping.feedback;
            if result.octave.is_none() {
                result.octave = match feedback.octave {
                    Some(Octave::Offset(n)) => Some(self.baseline.octave + n),
                    Some(Octave::Absolute(n)) => Some(n),
                    None => None,
                };
            }
            if result.tempo.is_none() {
                result.tempo = feedback.tempo;
            }
            if result.arp.is_none() {
                result.arp = feedback.arp;
            }
            active_tempo |= feedback.tempo.is_some();
        }
        result.octave = result.octave.or(self.idle.octave);
        result.tempo = result.tempo.or(self.idle.tempo);
        result.arp = result.arp.or(self.idle.arp);
        if active_tempo {
            result.arp = Some(true);
        }
        result.validate()
    }
    fn update(&mut self) -> Result<Option<Settings>> {
        let settings = self.effective()?;
        if self.scheduled == Some(settings) {
            return Ok(None);
        }
        self.scheduled = Some(settings);
        Ok(Some(settings))
    }
}

#[cfg(test)]
fn response(frame: &[u8]) -> Result<Payload> {
    program_response(frame, 0)
}

fn program_response(frame: &[u8], slot: u8) -> Result<Payload> {
    if frame.len() != 254
        || frame[..5] != [0xF0, 0x47, 0, 0x49, 0x67]
        || frame[7] != slot
        || frame[253] != 0xF7
    {
        return Err("wrong device/program or malformed program response".into());
    }
    if frame[1..253].iter().any(|byte| *byte > 127)
        || ((frame[5] as usize) << 7 | frame[6] as usize) != 246
    {
        return Err("invalid SysEx data/declared length".into());
    }
    let payload: Payload = frame[8..253].try_into()?;
    State::read(&payload)?;
    Ok(payload)
}
#[cfg(test)]
fn write_message(payload: &Payload) -> Result<Vec<u8>> {
    program_write_message(0, payload)
}

fn program_write_message(slot: u8, payload: &Payload) -> Result<Vec<u8>> {
    if slot > 8 {
        return Err("program slot must be 0–8".into());
    }
    State::read(payload)?;
    if payload.iter().any(|byte| *byte > 127) {
        return Err("payload must contain 7-bit data".into());
    }
    let mut message = vec![0xF0, 0x47, 0x7F, 0x49, 0x64, 1, 118, slot];
    message.extend(payload);
    message.push(0xF7);
    Ok(message)
}

// Own only fields we actually change, and refresh every full-payload write.
struct Owned {
    original: Payload,
    last: Payload,
    mask: [bool; 4],
    previous: Payload,
    uncertain: bool,
}
impl Owned {
    fn new(original: Payload) -> Self {
        Self {
            original,
            last: original,
            mask: [false; 4],
            previous: original,
            uncertain: false,
        }
    }
    fn prepare(&self, fresh: Payload, settings: Settings) -> Result<Payload> {
        settings.validate()?;
        if fresh
            .iter()
            .enumerate()
            .any(|(i, byte)| !OFFSETS.contains(&i) && *byte != self.last[i])
        {
            return Err("controller preset/settings changed externally; refusing stale feedback/restoration".into());
        }
        for (i, offset) in OFFSETS.iter().enumerate() {
            if self.mask[i]
                && fresh[*offset] != self.last[*offset]
                && !(self.uncertain && fresh[*offset] == self.previous[*offset])
            {
                return Err("controller state conflict: an owned field changed externally; paused without overwriting it".into());
            }
        }
        let mut next = fresh;
        for (i, offset) in OFFSETS.iter().enumerate() {
            if self.mask[i] {
                next[*offset] = self.original[*offset];
            }
        }
        if let Some(octave) = settings.octave {
            next[0x13] = (octave + 4) as u8;
        }
        if let Some(arp) = settings.arp {
            next[0x14] = u8::from(arp);
        }
        if let Some(tempo) = settings.tempo {
            next[0x1B] = (tempo >> 7) as u8;
            next[0x1C] = (tempo & 127) as u8;
        }
        State::read(&next)?;
        Ok(next)
    }
    fn attempting(&mut self, fresh: Payload, next: Payload) {
        for (i, offset) in OFFSETS.iter().enumerate() {
            if !self.mask[i] && next[*offset] != fresh[*offset] {
                self.original[*offset] = fresh[*offset];
                self.mask[i] = true;
            }
        }
        // Track attempted writes too: a failed transmission may have changed the device.
        self.previous = fresh;
        self.last = next;
        self.uncertain = true;
    }
    fn confirmed(&mut self, next: Payload) {
        for (i, offset) in OFFSETS.iter().enumerate() {
            self.mask[i] &= next[*offset] != self.original[*offset];
        }
        self.last = next;
        self.previous = next;
        self.uncertain = false;
    }
}

trait Transport {
    fn stored_program(&mut self, _slot: u8) -> Result<Payload> {
        Err("stored-program queries unavailable".into())
    }
    fn read(&mut self) -> Result<Payload>;
    fn write(&mut self, payload: &Payload) -> Result<()>;
}
struct Hardware {
    output: std::fs::File,
    replies: mpsc::Receiver<Reply>,
}
impl Hardware {
    fn send(&mut self, message: &[u8]) -> Result<()> {
        let mut sent = 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        while sent < message.len() {
            match self.output.write(&message[sent..]) {
                Ok(0) => return Err("MIDI output ended".into()),
                Ok(count) => sent += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    if Instant::now() >= deadline {
                        return Err("MIDI transmission timed out".into());
                    }
                    let mut fd = libc::pollfd {
                        fd: self.output.as_raw_fd(),
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    // SAFETY: one valid borrowed output descriptor. It retains the original
                    // kernel device, so a replaced port cannot receive a stale restoration.
                    unsafe {
                        libc::poll(&mut fd, 1, 10);
                    }
                }
                Err(error) => return Err(format!("MIDI transmission failed: {error}").into()),
            }
        }
        Ok(())
    }
}
impl Transport for Hardware {
    fn stored_program(&mut self, slot: u8) -> Result<Payload> {
        self.read_slot(slot)
    }
    fn read(&mut self) -> Result<Payload> {
        self.read_slot(0)
    }
    fn write(&mut self, payload: &Payload) -> Result<()> {
        self.write_slot(0, payload)
    }
}

trait Programs {
    fn read_slot(&mut self, slot: u8) -> Result<Payload>;
    fn select_slot(&mut self, slot: u8) -> Result<()>;
    fn write_slot(&mut self, slot: u8, payload: &Payload) -> Result<()>;
}
impl Programs for Hardware {
    fn write_slot(&mut self, slot: u8, payload: &Payload) -> Result<()> {
        self.send(&program_write_message(slot, payload)?)?;
        thread::sleep(Duration::from_millis(50));
        Ok(())
    }

    fn read_slot(&mut self, slot: u8) -> Result<Payload> {
        if slot > 8 {
            return Err("program slot must be 0–8".into());
        }
        while self.replies.try_recv().is_ok() {}
        let mut query = QUERY;
        query[7] = slot;
        self.send(&query)?;
        let frame = self
            .replies
            .recv_timeout(Duration::from_secs(2))
            .map_err(|e| format!("Program {slot} query failed: {e}"))?
            .map_err(io::Error::other)?;
        program_response(&frame, slot)
    }
    fn select_slot(&mut self, slot: u8) -> Result<()> {
        self.send(&selection_message(slot)?)?;
        // ponytail: same settling window as preset writes; replace with a verified ACK if available.
        thread::sleep(Duration::from_millis(50));
        Ok(())
    }
}
fn selection_message(slot: u8) -> Result<[u8; 9]> {
    if !(1..=8).contains(&slot) {
        return Err("stored program must be 1–8".into());
    }
    Ok([0xf0, 0x47, 0x7f, 0x49, 0x62, 0, 1, slot, 0xf7])
}
fn select_program(transport: &mut impl Programs, context: u8) -> Result<Payload> {
    let slot = context
        .checked_add(1)
        .filter(|n| *n <= 8)
        .ok_or("mapping context must be 0–7")?;
    let expected = transport.read_slot(slot)?;
    transport.select_slot(slot)?;
    if transport.read_slot(0)? != expected {
        return Err(
            "selected program RAM mismatch; selection unverified; output remains paused".into(),
        );
    }
    Ok(expected)
}
fn identify_program(ram: &Payload, programs: &[Payload]) -> Option<u8> {
    let mut matches = programs
        .iter()
        .enumerate()
        .filter(|(_, preset)| *preset == ram);
    let (context, _) = matches.next()?;
    if matches.next().is_some() {
        None
    } else {
        Some(context as u8)
    }
}

pub enum ProgramObservation {
    Detected {
        context: Option<u8>,
        payload: Payload,
        names: Vec<String>,
    },
    Changed,
}

fn program_name(payload: &Payload) -> String {
    payload[..16]
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| {
            if (32..=126).contains(b) {
                char::from(*b)
            } else {
                '?'
            }
        })
        .collect()
}
fn name_bytes(name: &str) -> Result<[u8; 16]> {
    if name.trim().is_empty() || name.len() > 16 || !name.bytes().all(|b| (32..=126).contains(&b)) {
        return Err("Use 1–16 printable ASCII characters; name cannot be blank".into());
    }
    let mut bytes = [0; 16];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    Ok(bytes)
}
fn rename_program(transport: &mut impl Programs, slot: u8, name: &str) -> Result<()> {
    selection_message(slot)?;
    let name = name_bytes(name)?;
    let mut payload = transport.read_slot(slot)?;
    payload[..16].copy_from_slice(&name);
    transport.write_slot(slot, &payload).map_err(|e| {
        format!("Rename write uncertain: {e}; read the stored preset before retrying")
    })?;
    let readback = transport
        .read_slot(slot)
        .map_err(|e| format!("Rename readback failed; stored name uncertain: {e}"))?;
    if readback != payload {
        return Err("Rename readback mismatch; stored name uncertain".into());
    }
    Ok(())
}
fn program_names(transport: &mut impl Programs) -> Vec<String> {
    (1..=8)
        .map(|slot| match transport.read_slot(slot) {
            Ok(payload) => format!("Program {slot} — {}", program_name(&payload)),
            Err(_) => format!("Program {slot} — name unavailable"),
        })
        .collect()
}
pub fn names(input: &mut midi::Input) -> Result<Vec<String>> {
    with_programs(input, |hardware| Ok(program_names(hardware)))
}
pub fn rename(input: &mut midi::Input, slot: u8, name: &str) -> Result<()> {
    with_programs(input, |hardware| rename_program(hardware, slot, name))
}

pub fn set_knob_mode(
    input: &mut midi::Input,
    context: u8,
    knob: u8,
    cc: u8,
    relative: bool,
) -> Result<Payload> {
    with_programs(input, |hardware| {
        change_knob_mode(hardware, context, knob, cc, relative)
    })
}

fn change_knob_mode(
    hardware: &mut impl Programs,
    context: u8,
    knob: u8,
    cc: u8,
    relative: bool,
) -> Result<Payload> {
    let slot = context
        .checked_add(1)
        .filter(|n| *n <= 8)
        .ok_or("program must be 1–8")?;
    if !(1..=8).contains(&knob) {
        return Err("knob must be 1–8".into());
    }
    (|| {
        let original = hardware.read_slot(slot)?;
        if hardware.read_slot(0)? != original {
            return Err(
                "current RAM differs from stored program; select it before changing knob mode"
                    .into(),
            );
        }
        let offset = 0x54 + 20 * usize::from(knob - 1);
        if original[offset + 1] != cc || original[offset] > 1 {
            return Err("knob CC or mode differs from the observed program; retry learning".into());
        }
        let mut next = original;
        next[offset] = u8::from(relative);
        if next == original {
            return Ok(next);
        }
        let changed = (|| -> Result<()> {
            hardware.write_slot(slot, &next)?;
            if hardware.read_slot(slot)? != next {
                return Err("stored knob-mode readback mismatch".into());
            }
            hardware.write_slot(0, &next)?;
            if hardware.read_slot(0)? != next {
                return Err("RAM knob-mode readback mismatch".into());
            }
            Ok(())
        })();
        if let Err(error) = changed {
            let restored = (|| -> Result<()> {
                hardware.write_slot(slot, &original)?;
                hardware.write_slot(0, &original)?;
                if hardware.read_slot(slot)? != original || hardware.read_slot(0)? != original {
                    return Err("restoration readback mismatch".into());
                }
                Ok(())
            })();
            return Err(
                format!("Knob mode change failed: {error}; restore result: {restored:?}").into(),
            );
        }
        Ok(next)
    })()
}

// The feedback worker must be finished before borrowing its single response stream.
fn with_programs<T>(
    input: &mut midi::Input,
    operation: impl FnOnce(&mut Hardware) -> Result<T>,
) -> Result<T> {
    let output = input.output()?;
    let replies = input
        .sysex
        .take()
        .ok_or("controller response stream already owned")?;
    let mut hardware = Hardware { output, replies };
    let result = operation(&mut hardware);
    input.sysex = Some(hardware.replies);
    result
}
pub fn select(input: &mut midi::Input, context: u8) -> Result<Payload> {
    with_programs(input, |hardware| select_program(hardware, context))
}

fn apply(
    transport: &mut impl Transport,
    owned: &mut Owned,
    settings: Settings,
    current: impl Fn() -> bool,
    report: &mut impl FnMut(String),
) -> Result<()> {
    let fresh = transport.read()?;
    let next = owned.prepare(fresh, settings)?;
    if !current() {
        return Ok(());
    }
    if next == fresh {
        owned.confirmed(next);
        return Ok(());
    }
    owned.attempting(fresh, next);
    transport.write(&next)?;
    report(format!(
        "Sent Program 0 update: {} (physical effect unconfirmed)",
        State::read(&next)?
    ));
    let readback = transport.read()?;
    if readback != next {
        return Err("controller read-back mismatch; update/restoration unconfirmed".into());
    }
    owned.confirmed(readback);
    report(format!(
        "Read-back verified: {} (persistence across unplug/reboot unmeasured)",
        State::read(&next)?
    ));
    Ok(())
}

#[derive(Default)]
struct Work {
    revision: u64,
    pending: Option<Settings>,
    stop: bool,
    abandon: bool,
}
enum Notice {
    Program(ProgramObservation),
    Ready(State),
    Status(String),
    Failed(String),
}
pub struct Controller {
    work: Arc<(Mutex<Work>, Condvar)>,
    worker: Option<thread::JoinHandle<std::result::Result<String, String>>>,
    notices: mpsc::Receiver<Notice>,
    pub baseline: Option<State>,
    pub program: Option<ProgramObservation>,
    pub status: String,
}
impl Controller {
    pub fn start(input: &mut midi::Input) -> Result<Self> {
        Self::start_verified(input, None)
    }
    pub fn start_verified(input: &mut midi::Input, expected: Option<Payload>) -> Result<Self> {
        let output = input.output()?;
        let replies = input
            .sysex
            .take()
            .ok_or("controller response stream already owned")?;
        let hardware = Hardware { output, replies };
        Ok(Self::spawn_verified(hardware, expected))
    }
    pub fn start_follow(input: &mut midi::Input, selected: Option<(u8, Payload)>) -> Result<Self> {
        let output = input.output()?;
        let replies = input
            .sysex
            .take()
            .ok_or("controller response stream already owned")?;
        Ok(Self::spawn_worker(
            Hardware { output, replies },
            selected.map(|(_, p)| p),
            true,
            selected.map(|(c, _)| c),
        ))
    }
    #[cfg(test)]
    fn spawn(transport: impl Transport + Send + 'static) -> Self {
        Self::spawn_verified(transport, None)
    }
    #[cfg(test)]
    pub(crate) fn fake_for_reset(writes: Arc<Mutex<usize>>) -> Self {
        struct Fake(Arc<Mutex<usize>>);
        impl Transport for Fake {
            fn read(&mut self) -> Result<Payload> {
                let mut payload = [0; 245];
                payload[0x13] = 4;
                payload[0x1C] = 120;
                Ok(payload)
            }
            fn write(&mut self, _: &Payload) -> Result<()> {
                *self.0.lock().unwrap() += 1;
                Ok(())
            }
        }
        Self::spawn(Fake(writes))
    }
    fn spawn_verified(
        transport: impl Transport + Send + 'static,
        expected: Option<Payload>,
    ) -> Self {
        Self::spawn_worker(transport, expected, false, None)
    }
    fn spawn_worker(
        mut transport: impl Transport + Send + 'static,
        expected: Option<Payload>,
        follow: bool,
        selected: Option<u8>,
    ) -> Self {
        let work = Arc::new((Mutex::new(Work::default()), Condvar::new()));
        let shared = Arc::clone(&work);
        let (sender, notices) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut programs = Vec::new();
            if follow {
                for slot in 1..=8 {
                    if shared.0.lock().unwrap().stop {
                        return Ok("Detection cancelled; no writes".into());
                    }
                    match transport.stored_program(slot) {
                        Ok(payload) => programs.push(payload),
                        Err(error) => {
                            let _ = sender.send(Notice::Failed(format!(
                                "Program detection failed: {error}; no update sent"
                            )));
                            return Ok("No owned hardware fields to restore".into());
                        }
                    }
                }
            }
            let original = match transport.read() {
                Ok(payload) => payload,
                Err(error) => {
                    let _ = sender.send(Notice::Failed(format!(
                        "Snapshot rejected: {error}; no update sent"
                    )));
                    return Ok("No owned hardware fields to restore".into());
                }
            };
            if expected.is_some_and(|expected| expected != original) {
                let _ = sender.send(Notice::Failed("RAM changed since verified selection; select /prog-select again; no update sent".into()));
                return Ok("No owned hardware fields to restore".into());
            }
            if follow {
                let context = selected.or_else(|| identify_program(&original, &programs));
                let names = programs
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!("Program {} — {}", i + 1, program_name(p)))
                    .collect();
                let _ = sender.send(Notice::Program(ProgramObservation::Detected {
                    context,
                    payload: original,
                    names,
                }));
            }
            let _ = sender.send(Notice::Ready(
                State::read(&original).expect("validated snapshot"),
            ));
            let mut owned = Owned::new(original);
            let mut report = |text| {
                let _ = sender.send(Notice::Status(text));
            };
            loop {
                let (lock, wake) = &*shared;
                let mut request = lock.lock().unwrap();
                while request.pending.is_none() && !request.stop {
                    if follow {
                        let (next, timeout) = wake
                            .wait_timeout(request, Duration::from_millis(250))
                            .unwrap();
                        request = next;
                        if timeout.timed_out() {
                            break;
                        }
                    } else {
                        request = wake.wait(request).unwrap();
                    }
                }
                if request.stop {
                    break;
                }
                let settings = request.pending.take();
                let revision = request.revision;
                drop(request);
                if follow {
                    match transport.read() {
                        Ok(fresh) if fresh == owned.last => {}
                        Ok(_) => {
                            let mut work = lock.lock().unwrap();
                            work.abandon = true;
                            work.stop = true;
                            work.pending = None;
                            let _ = sender.send(Notice::Program(ProgramObservation::Changed));
                            break;
                        }
                        Err(error) => {
                            lock.lock().unwrap().abandon = true;
                            let _ = sender.send(Notice::Failed(format!(
                                "Program monitoring failed: {error}; output must remain inactive"
                            )));
                            break;
                        }
                    }
                }
                let Some(settings) = settings else {
                    continue;
                };
                let current = || {
                    let work = lock.lock().unwrap();
                    !work.stop && work.revision == revision
                };
                if let Err(error) =
                    apply(&mut transport, &mut owned, settings, current, &mut report)
                {
                    if follow
                        && !owned.uncertain
                        && (error.to_string().contains("changed externally")
                            || error.to_string().contains("state conflict"))
                    {
                        // A physical switch can race the pre-write query. Discard the old baseline and re-detect.
                        lock.lock().unwrap().abandon = true;
                        let _ = sender.send(Notice::Program(ProgramObservation::Changed));
                    } else {
                        if follow {
                            lock.lock().unwrap().abandon = true;
                        }
                        let _ = sender.send(Notice::Failed(format!("Feedback failed: {error}")));
                    }
                    break;
                }
            }
            // Caller releases synthetic keys before stopping this worker.
            if shared.0.lock().unwrap().abandon {
                return Ok("Preset changed externally; old feedback snapshot discarded without restoration".into());
            }
            if !owned.mask.iter().any(|owned| *owned) {
                return Ok("Restoration complete: no modified fields remain".into());
            }
            match apply(
                &mut transport,
                &mut owned,
                Settings::default(),
                || true,
                &mut report,
            ) {
                Ok(()) => Ok("Owned hardware fields restored and read-back verified".into()),
                Err(error) => Err(format!(
                    "Hardware restoration unavailable/unconfirmed: {error}"
                )),
            }
        });
        Self {
            work,
            worker: Some(worker),
            notices,
            baseline: None,
            program: None,
            status: "Reading validated Program 0 snapshot; no writes yet".into(),
        }
    }
    pub fn set(&mut self, settings: Settings) -> Result<()> {
        settings.validate()?;
        let (lock, wake) = &*self.work;
        let mut work = lock.lock().unwrap();
        work.revision += 1;
        work.pending = Some(settings);
        wake.notify_one();
        Ok(())
    }
    pub fn poll(&mut self) -> Result<()> {
        while let Ok(notice) = self.notices.try_recv() {
            match notice {
                Notice::Program(observation) => self.program = Some(observation),
                Notice::Ready(state) => {
                    self.baseline = Some(state);
                    self.status = format!("Program 0 snapshot validated: {state}");
                }
                Notice::Status(text) => self.status = text,
                Notice::Failed(error) => {
                    self.status = error.clone();
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }
}
impl Controller {
    pub fn abandon(&mut self) -> Result<String> {
        self.work.0.lock().unwrap().abandon = true;
        self.finish()
    }
    pub fn finish(&mut self) -> Result<String> {
        let (lock, wake) = &*self.work;
        {
            let mut work = lock.lock().unwrap();
            work.stop = true;
            work.pending = None;
            work.revision += 1;
            wake.notify_one();
        }
        match self.worker.take() {
            Some(worker) => worker
                .join()
                .map_err(|_| "Feedback worker failed; hardware restoration unconfirmed")?
                .map_err(Into::into),
            None => Ok("Controller worker already closed".into()),
        }
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        if self.worker.is_some() {
            match self.finish() {
                Ok(message) => eprintln!("{message}"),
                Err(error) => eprintln!("{error}"),
            }
        }
    }
}

pub fn read_only() -> Result<()> {
    let mut input = midi::Input::open()?;
    let mut controller = Controller::start(&mut input)?;
    loop {
        controller.poll()?;
        if let Some(state) = controller.baseline {
            println!(
                "Validated Program 0 snapshot: {state}\nQuery identifies Program 0; it does not prove which stored program is selected. No settings written."
            );
            return Ok(());
        }
        while input.next()?.is_some() {}
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knob_mode_changes_only_one_byte_in_stored_program_and_ram() {
        struct Fake {
            stored: Payload,
            ram: Payload,
            fail_ram: bool,
        }
        impl Programs for Fake {
            fn read_slot(&mut self, slot: u8) -> Result<Payload> {
                Ok(if slot == 0 { self.ram } else { self.stored })
            }
            fn write_slot(&mut self, slot: u8, payload: &Payload) -> Result<()> {
                if slot == 0 {
                    if self.fail_ram {
                        self.fail_ram = false;
                        return Err("simulated RAM write failure".into());
                    }
                    self.ram = *payload;
                } else {
                    self.stored = *payload;
                }
                Ok(())
            }
            fn select_slot(&mut self, _slot: u8) -> Result<()> {
                Ok(())
            }
        }
        let mut original = [0u8; 245];
        original[0x55] = 16;
        let mut fake = Fake {
            stored: original,
            ram: original,
            fail_ram: false,
        };
        let changed = change_knob_mode(&mut fake, 0, 1, 16, true).unwrap();
        assert_eq!(changed[0x54], 1);
        assert_eq!(
            changed
                .iter()
                .zip(original)
                .filter(|(a, b)| **a != *b)
                .count(),
            1
        );
        assert_eq!(fake.stored, changed);
        assert_eq!(fake.ram, changed);
        assert_eq!(identify_program(&fake.ram, &[fake.stored]), Some(0));
        change_knob_mode(&mut fake, 0, 1, 16, false).unwrap();
        assert_eq!(fake.stored, original);
        assert_eq!(fake.ram, original);
        fake.fail_ram = true;
        assert!(change_knob_mode(&mut fake, 0, 1, 16, true).is_err());
        assert_eq!(fake.stored, original);
        assert_eq!(fake.ram, original);
        assert!(change_knob_mode(&mut fake, 0, 1, 17, true).is_err());
        assert_eq!(fake.stored, original);
    }
    fn payload() -> Payload {
        let mut p = [0; 245];
        p[0x13] = 4;
        p[0x1C] = 120;
        p
    }
    struct Fake {
        payload: Payload,
        writes: Vec<Payload>,
        fail: bool,
    }
    impl Transport for Fake {
        fn read(&mut self) -> Result<Payload> {
            Ok(self.payload)
        }
        fn write(&mut self, payload: &Payload) -> Result<()> {
            if self.fail {
                return Err("write failed".into());
            }
            self.payload = *payload;
            self.writes.push(*payload);
            Ok(())
        }
    }
    #[test]
    fn captured_program_selection_verifies_ram_and_rejects_uncertain_transitions() {
        struct Fake {
            slots: Vec<Payload>,
            ram: Payload,
            log: Vec<String>,
            mismatch: bool,
            fail: bool,
        }
        impl Programs for Fake {
            fn write_slot(&mut self, slot: u8, payload: &Payload) -> Result<()> {
                self.log.push(format!("write {slot}"));
                if !self.mismatch {
                    self.slots[slot as usize - 1] = *payload;
                }
                Ok(())
            }
            fn read_slot(&mut self, slot: u8) -> Result<Payload> {
                self.log.push(format!("read {slot}"));
                if self.fail && slot == 0 {
                    return Err("timeout".into());
                }
                Ok(if slot == 0 {
                    self.ram
                } else {
                    self.slots[slot as usize - 1]
                })
            }
            fn select_slot(&mut self, slot: u8) -> Result<()> {
                self.log.push(format!("select {slot}"));
                if !self.mismatch {
                    self.ram = self.slots[slot as usize - 1];
                }
                Ok(())
            }
        }
        let capture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/mpk-mini3-programs.hex"
        ));
        let slots: Vec<Payload> = capture
            .lines()
            .filter(|line| !line.starts_with('#'))
            .map(|line| {
                line.split_whitespace()
                    .map(|b| u8::from_str_radix(b, 16).unwrap())
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap()
            })
            .collect();
        assert_eq!(slots.len(), 9);
        let mut fake = Fake {
            ram: slots[0],
            slots: slots[1..9].to_vec(),
            log: vec![],
            mismatch: false,
            fail: false,
        };
        for context in 0..8 {
            fake.log.clear();
            select_program(&mut fake, context).unwrap();
            assert_eq!(
                fake.log,
                [
                    format!("read {}", context + 1),
                    format!("select {}", context + 1),
                    "read 0".into()
                ]
            );
            assert_eq!(selection_message(context + 1).unwrap()[7], context + 1);
        }
        assert_eq!(
            selection_message(3).unwrap(),
            [0xf0, 0x47, 0x7f, 0x49, 0x62, 0, 1, 3, 0xf7]
        );
        assert!(selection_message(0).is_err());
        assert!(select_program(&mut fake, 8).is_err());
        fake.mismatch = true;
        assert!(select_program(&mut fake, 0).is_err());
        fake.mismatch = false;
        fake.fail = true;
        assert!(select_program(&mut fake, 0).is_err());
        let mut frame = program_write_message(3, &fake.slots[2]).unwrap();
        frame[2] = 0;
        frame[4] = 0x67;
        assert_eq!(program_response(&frame, 3).unwrap(), fake.slots[2]);
        assert!(program_response(&frame, 0).is_err());
        frame.pop();
        assert!(program_response(&frame, 3).is_err());
        fake.fail = false;
        let before = fake.slots.clone();
        let ram = fake.ram;
        for invalid in ["", "   ", "12345678901234567", "bad\nname", "é"] {
            fake.log.clear();
            assert!(rename_program(&mut fake, 2, invalid).is_err());
            assert!(fake.log.is_empty());
        }
        rename_program(&mut fake, 2, "1234567890123456").unwrap();
        assert_eq!(program_name(&fake.slots[1]), "1234567890123456");
        rename_program(&mut fake, 2, "Test").unwrap();
        assert_eq!(&fake.slots[1][..16], b"Test\0\0\0\0\0\0\0\0\0\0\0\0");
        assert_eq!(&fake.slots[1][16..], &before[1][16..]);
        assert_eq!(fake.slots[0], before[0]);
        assert_eq!(fake.ram, ram); // stored rename does not overwrite live RAM
        fake.mismatch = true;
        assert!(rename_program(&mut fake, 2, "Other").is_err());
        struct Partial(Fake);
        impl Programs for Partial {
            fn read_slot(&mut self, slot: u8) -> Result<Payload> {
                if slot == 4 {
                    Err("unavailable".into())
                } else {
                    self.0.read_slot(slot)
                }
            }
            fn select_slot(&mut self, slot: u8) -> Result<()> {
                self.0.select_slot(slot)
            }
            fn write_slot(&mut self, slot: u8, payload: &Payload) -> Result<()> {
                self.0.write_slot(slot, payload)
            }
        }
        let listed = program_names(&mut Partial(fake));
        assert_eq!(listed.len(), 8);
        assert!(listed[3].contains("name unavailable"));
        assert!(listed[0].starts_with("Program 1 —"));
    }

    #[test]
    fn follow_identifies_unique_payloads_and_never_restores_across_external_switch() {
        struct Device {
            ram: Arc<Mutex<Payload>>,
            slots: Vec<Payload>,
            writes: Arc<Mutex<Vec<Payload>>>,
        }
        impl Transport for Device {
            fn stored_program(&mut self, slot: u8) -> Result<Payload> {
                Ok(self.slots[slot as usize - 1])
            }
            fn read(&mut self) -> Result<Payload> {
                Ok(*self.ram.lock().unwrap())
            }
            fn write(&mut self, payload: &Payload) -> Result<()> {
                *self.ram.lock().unwrap() = *payload;
                self.writes.lock().unwrap().push(*payload);
                Ok(())
            }
        }
        let slots = (0..8)
            .map(|i| {
                let mut p = payload();
                p[80] = i;
                p
            })
            .collect::<Vec<_>>();
        assert_eq!(identify_program(&slots[2], &slots), Some(2));
        assert_eq!(program_name(&slots[2]), program_name(&slots[3])); // names do not identify a slot
        let mut duplicate = slots.clone();
        duplicate[3] = duplicate[2];
        assert_eq!(identify_program(&slots[2], &duplicate), None);
        let mut unknown = slots[2];
        unknown[81] = 17;
        assert_eq!(identify_program(&unknown, &slots), None);
        let ram = Arc::new(Mutex::new(slots[2]));
        let writes = Arc::new(Mutex::new(Vec::new()));
        let mut controller = Controller::spawn_worker(
            Device {
                ram: ram.clone(),
                slots: slots.clone(),
                writes: writes.clone(),
            },
            None,
            true,
            None,
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            controller.poll().unwrap();
            if let Some(ProgramObservation::Detected { context, .. }) = controller.program.take() {
                assert_eq!(context, Some(2));
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(writes.lock().unwrap().is_empty()); // detection never selects/writes hardware
        controller
            .set(Settings {
                octave: Some(1),
                ..Settings::default()
            })
            .unwrap();
        while writes.lock().unwrap().is_empty() {
            controller.poll().unwrap();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(300));
        controller.poll().unwrap();
        assert!(
            controller.program.is_none(),
            "own feedback must not look like a physical switch"
        );
        *ram.lock().unwrap() = slots[0]; // user selected Program 1 on hardware, with feedback owned in 3
        loop {
            controller.poll().unwrap();
            if matches!(controller.program.take(), Some(ProgramObservation::Changed)) {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        // Even a late old-context update cannot write after the monitor observed the switch.
        controller
            .set(Settings {
                octave: Some(2),
                ..Settings::default()
            })
            .unwrap();
        controller.finish().unwrap();
        assert_eq!(*ram.lock().unwrap(), slots[0]);
        assert_eq!(
            writes.lock().unwrap().len(),
            1,
            "no old-context restoration or queued write"
        );
    }

    #[test]
    fn follow_refuses_unavailable_stored_programs_without_any_write() {
        struct Missing;
        impl Transport for Missing {
            fn read(&mut self) -> Result<Payload> {
                panic!("missing stored programs must stop startup")
            }
            fn write(&mut self, _: &Payload) -> Result<()> {
                panic!("must not write")
            }
        }
        let mut controller = Controller::spawn_worker(Missing, None, true, None);
        let deadline = Instant::now() + Duration::from_secs(1);
        while controller.poll().is_ok() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(controller.baseline.is_none());
        assert!(controller.program.is_none());
        controller.finish().unwrap();
    }

    #[test]
    fn controller_rejects_ram_that_changed_after_verified_program_selection() {
        let expected = payload();
        let mut changed = expected;
        changed[80] = 1;
        let mut controller = Controller::spawn_verified(
            Fake {
                payload: changed,
                writes: Vec::new(),
                fail: false,
            },
            Some(expected),
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if controller.poll().is_err() {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(controller.status.contains("select /prog-select again"));
        assert!(controller.baseline.is_none());
        controller.finish().unwrap();
    }

    #[test]
    fn failed_snapshot_and_readback_are_reported_without_claiming_restoration() {
        struct Unavailable;
        impl Transport for Unavailable {
            fn read(&mut self) -> Result<Payload> {
                Err("query timed out".into())
            }
            fn write(&mut self, _: &Payload) -> Result<()> {
                panic!("invalid snapshot must never write")
            }
        }
        let mut controller = Controller::spawn(Unavailable);
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if controller.poll().is_err() {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(controller.baseline.is_none());
        assert!(controller.status.contains("no update sent"));
        controller.finish().unwrap();
        struct Mismatch(Fake);
        impl Transport for Mismatch {
            fn read(&mut self) -> Result<Payload> {
                self.0.read()
            }
            fn write(&mut self, p: &Payload) -> Result<()> {
                self.0.write(p)?;
                self.0.payload[100] ^= 1;
                Ok(())
            }
        }
        let mut transport = Mismatch(Fake {
            payload: payload(),
            writes: Vec::new(),
            fail: false,
        });
        let mut owned = Owned::new(payload());
        assert!(
            apply(
                &mut transport,
                &mut owned,
                Settings {
                    octave: Some(1),
                    ..Settings::default()
                },
                || true,
                &mut |_| {}
            )
            .is_err()
        );
        assert!(
            apply(
                &mut transport,
                &mut owned,
                Settings::default(),
                || true,
                &mut |_| {}
            )
            .is_err()
        );
        assert!(owned.uncertain); // a failed restoration readback cannot clear the ownership record
    }

    #[test]
    fn latest_owner_tempo_fallback_relative_octave_and_coupled_arpeggiator() {
        use crate::{Control, mappings::Mapping};
        let a = Control::from_note(10, 36).unwrap();
        let b = Control::from_note(10, 44).unwrap();
        let c = Control::from_note(10, 37).unwrap();
        let mut first = Mapping::new(a, "Shift", "hold").unwrap();
        first.feedback = Feedback {
            octave: Some(Octave::Offset(1)),
            tempo: Some(50),
            arp: Some(true),
        };
        let mut second = Mapping::new(b, "Ctrl", "toggle").unwrap();
        second.feedback = Feedback {
            octave: Some(Octave::Absolute(-1)),
            tempo: Some(240),
            arp: Some(true),
        };
        let mut off = Mapping::new(c, "Alt", "hold").unwrap();
        off.feedback.arp = Some(false);
        let idle = Settings {
            tempo: Some(120),
            arp: Some(true),
            ..Settings::default()
        };
        let mut resolver = Resolver::new(
            &[first.clone(), second, off],
            State {
                octave: 0,
                tempo: 120,
                arp: false,
            },
            idle,
        )
        .unwrap();
        let mut recorded = vec![resolver.clear().unwrap().unwrap().tempo.unwrap()];
        for (pad, active) in [(a, true), (b, true), (b, false), (a, false)] {
            recorded.push(
                resolver
                    .transition(pad, active)
                    .unwrap()
                    .unwrap()
                    .tempo
                    .unwrap(),
            );
        }
        assert_eq!(recorded, [120, 50, 240, 50, 120]);
        assert_eq!(
            resolver.transition(a, true).unwrap().unwrap().octave,
            Some(1)
        );
        assert!(resolver.transition(a, true).unwrap().is_none()); // duplicate request yields no changed write
        assert_eq!(resolver.transition(c, true).unwrap(), None); // active tempo still forces On
        assert_eq!(
            resolver.transition(a, false).unwrap().unwrap().arp,
            Some(false)
        );
        assert_eq!(resolver.clear().unwrap().unwrap(), idle);
        let mut boundary = Resolver::new(
            &[first],
            State {
                octave: 4,
                tempo: 120,
                arp: false,
            },
            Settings::default(),
        )
        .unwrap();
        assert!(boundary.transition(a, true).is_err());
        assert_eq!(boundary.clear().unwrap().unwrap(), Settings::default());
    }

    #[test]
    fn pending_hardware_query_does_not_delay_key_release() {
        use crate::{Control, Event, ReleaseType, keyboard::Keyboard, mappings::Mapping};
        struct Waiting {
            fake: Fake,
            reads: usize,
            entered: mpsc::Sender<()>,
            permit: mpsc::Receiver<()>,
        }
        impl Transport for Waiting {
            fn read(&mut self) -> Result<Payload> {
                self.reads += 1;
                if self.reads == 2 {
                    self.entered.send(())?;
                    self.permit.recv_timeout(Duration::from_secs(2))?;
                }
                self.fake.read()
            }
            fn write(&mut self, payload: &Payload) -> Result<()> {
                self.fake.write(payload)
            }
        }
        let (entered, incoming) = mpsc::channel();
        let (permit, release) = mpsc::channel();
        let mut controller = Controller::spawn(Waiting {
            fake: Fake {
                payload: payload(),
                writes: Vec::new(),
                fail: false,
            },
            reads: 0,
            entered,
            permit: release,
        });
        controller
            .set(Settings {
                tempo: Some(50),
                arp: Some(true),
                ..Settings::default()
            })
            .unwrap();
        incoming.recv_timeout(Duration::from_secs(1)).unwrap();
        let a = Control::from_note(10, 36).unwrap();
        let mut keyboard = Keyboard::new(&[Mapping::new(a, "Shift", "hold").unwrap()]).unwrap();
        let mut events = Vec::new();
        let mut emit = |key, down| {
            events.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard
            .observe(Event::Release(a, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        assert_eq!(
            events,
            [
                (evdev::KeyCode::KEY_LEFTSHIFT, true),
                (evdev::KeyCode::KEY_LEFTSHIFT, false)
            ]
        );
        permit.send(()).unwrap();
        controller.finish().unwrap();
    }

    #[test]
    fn protocol_preserves_unrelated_fields_rejects_bad_frames_and_restores_owned_fields() {
        let p = payload();
        let mut frame = write_message(&p).unwrap();
        frame[2] = 0;
        frame[4] = 0x67;
        assert_eq!(response(&frame).unwrap(), p);
        for index in [0, 1, 2, 3, 4, 5, 6, 7, 253] {
            let mut bad = frame.clone();
            bad[index] ^= 1;
            assert!(response(&bad).is_err());
        }
        assert!(response(&frame[..253]).is_err());
        let mut oversized = frame.clone();
        oversized.push(0);
        assert!(response(&oversized).is_err());
        let mut bad = frame.clone();
        bad[50] = 128;
        assert!(response(&bad).is_err());
        let mut hardware = Fake {
            payload: p,
            writes: Vec::new(),
            fail: false,
        };
        let mut owned = Owned::new(p);
        let mut reports = Vec::new();
        hardware.payload[100] = 7;
        assert!(
            owned
                .prepare(hardware.payload, Settings::default())
                .is_err()
        );
        hardware.payload[100] = p[100];
        apply(
            &mut hardware,
            &mut owned,
            Settings {
                octave: Some(1),
                tempo: Some(50),
                arp: Some(true),
            },
            || true,
            &mut |s| reports.push(s),
        )
        .unwrap();
        assert_eq!(hardware.payload[100], p[100]);
        assert_eq!(
            State::read(&hardware.payload).unwrap(),
            State {
                octave: 1,
                tempo: 50,
                arp: true
            }
        );
        hardware.payload[101] = 9;
        assert!(
            owned
                .prepare(hardware.payload, Settings::default())
                .is_err()
        );
        hardware.payload[101] = p[101];
        apply(
            &mut hardware,
            &mut owned,
            Settings::default(),
            || true,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            State::read(&hardware.payload).unwrap(),
            State::read(&p).unwrap()
        );
        assert_eq!(hardware.payload[100], p[100]);
        assert_eq!(hardware.payload[101], p[101]);
        assert!(reports.iter().any(|s| s.starts_with("Sent")));
        assert!(reports.iter().any(|s| s.starts_with("Read-back")));
        apply(
            &mut hardware,
            &mut owned,
            Settings {
                octave: Some(-1),
                ..Settings::default()
            },
            || true,
            &mut |_| {},
        )
        .unwrap();
        hardware.payload[0x13] = 6;
        assert!(
            apply(
                &mut hardware,
                &mut owned,
                Settings::default(),
                || true,
                &mut |_| {}
            )
            .is_err()
        );
        assert_eq!(hardware.payload[0x13], 6);

        assert!(
            Settings {
                octave: Some(5),
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
        let mut hardware2 = Fake {
            payload: p,
            writes: Vec::new(),
            fail: false,
        };
        let mut owned2 = Owned::new(p);
        hardware2.payload[0x1C] = 100; // tempo is not owned by an octave-only update
        apply(
            &mut hardware2,
            &mut owned2,
            Settings {
                octave: Some(1),
                ..Settings::default()
            },
            || true,
            &mut |_| {},
        )
        .unwrap();
        apply(
            &mut hardware2,
            &mut owned2,
            Settings::default(),
            || true,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(hardware2.payload[0x1C], 100);
        hardware2.fail = true;
        assert!(
            apply(
                &mut hardware2,
                &mut owned2,
                Settings {
                    octave: Some(1),
                    ..Settings::default()
                },
                || true,
                &mut |_| {}
            )
            .is_err()
        );
        apply(
            &mut hardware2,
            &mut owned2,
            Settings::default(),
            || true,
            &mut |_| {},
        )
        .unwrap();
        assert!(!owned2.mask.iter().any(|v| *v));
        let before = hardware.writes.len();
        let snapshot = hardware.payload;
        apply(
            &mut hardware,
            &mut Owned::new(snapshot),
            Settings {
                octave: Some(-1),
                ..Settings::default()
            },
            || false,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(hardware.writes.len(), before);
    }
}
