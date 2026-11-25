use crate::mappings::{Behavior, Mapping};
use crate::{Control, Event, Result};
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, uinput::VirtualDevice};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// How long a risky control must be held before it fires.
pub const ARM: Duration = Duration::from_secs(1);

pub fn key_code(name: &str) -> Result<KeyCode> {
    let linux_name = match name {
        "Ctrl" => "LEFTCTRL",
        "Shift" => "LEFTSHIFT",
        "Alt" => "LEFTALT",
        "Meta" => "LEFTMETA",
        "Escape" => "ESC",
        "LeftBracket" => "LEFTBRACE",
        "RightBracket" => "RIGHTBRACE",
        "Apostrophe" => "APOSTROPHE",
        other => &other.to_ascii_uppercase(),
    };
    format!("KEY_{linux_name}")
        .parse()
        .map_err(|_| format!("key {name:?} is unsupported by the Linux input backend").into())
}

pub fn paste_keys(shift: bool) -> Vec<KeyCode> {
    let mut keys = vec![KeyCode::KEY_LEFTCTRL];
    if shift {
        keys.push(KeyCode::KEY_LEFTSHIFT);
    }
    keys.push(KeyCode::KEY_V);
    keys
}

fn modifier(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::KEY_LEFTCTRL
            | KeyCode::KEY_RIGHTCTRL
            | KeyCode::KEY_LEFTSHIFT
            | KeyCode::KEY_RIGHTSHIFT
            | KeyCode::KEY_LEFTALT
            | KeyCode::KEY_RIGHTALT
            | KeyCode::KEY_LEFTMETA
            | KeyCode::KEY_RIGHTMETA
    )
}

struct Assignment {
    control: Control,
    keys: Vec<KeyCode>,
    behavior: Behavior,
    active: bool,
    risky: bool,
}

pub struct Keyboard {
    assignments: Vec<Assignment>,
    down: HashSet<Control>,
    blocked: HashSet<Control>,
    // Includes attempted key-downs: emit can fail after the kernel received the key.
    held: HashSet<KeyCode>,
    running: bool,
    /// Risky controls held but not yet fired, with their press time.
    armed: HashMap<Control, Instant>,
    /// v7 ticket 99: a risky control let go before one second, until the session reads it.
    early: Option<Control>,
}

impl Keyboard {
    pub fn new(mappings: &[Mapping]) -> Result<Self> {
        let mut seen = HashSet::new();
        let mut assignments = Vec::new();
        for mapping in mappings {
            mapping.validate()?;
            if !seen.insert(mapping.control) {
                return Err("duplicate assignment".into());
            }
            let keys = mapping
                .keys
                .iter()
                .map(|key| key_code(key))
                .collect::<Result<_>>()?;
            assignments.push(Assignment {
                control: mapping.control,
                keys,
                behavior: mapping.behavior.clone(),
                active: false,
                risky: mapping.risky,
            });
        }
        Ok(Self {
            assignments,
            down: HashSet::new(),
            blocked: HashSet::new(),
            held: HashSet::new(),
            running: true,
            armed: HashMap::new(),
            early: None,
        })
    }

    pub fn observe(
        &mut self,
        event: Event,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<Option<(Control, bool)>> {
        self.observe_at(event, Instant::now(), emit)
    }

    pub fn observe_at(
        &mut self,
        event: Event,
        now: Instant,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<Option<(Control, bool)>> {
        if let Event::Pulse(control) = event {
            // A pulse acquires/releases only keys not owned by another active mapping.
            // Key-up follows immediately; no timer can delay another control's release.
            let transition = self.observe(Event::Press(control), emit)?;
            self.observe(
                Event::Release(control, crate::ReleaseType::PulseComplete),
                emit,
            )?;
            return Ok(transition.filter(|_| {
                self.assignments
                    .iter()
                    .any(|a| a.control == control && a.behavior == Behavior::Trigger)
            }));
        }
        let (pad, pressed) = match event {
            Event::Press(pad) if self.down.insert(pad) => {
                if self.blocked.contains(&pad) {
                    return Ok(None);
                }
                (pad, true)
            }
            Event::Release(pad, _) | Event::UnmatchedRelease(pad) => {
                self.blocked.remove(&pad);
                if !self.down.remove(&pad) {
                    return Ok(None);
                }
                (pad, false)
            }
            _ => return Ok(None),
        };
        if !self.running {
            return Ok(None);
        }
        let Some(index) = self.assignments.iter().position(|item| item.control == pad) else {
            return Ok(None);
        };
        if self.assignments[index].risky {
            if pressed {
                self.armed.insert(pad, now);
                return Ok(None);
            }
            if self.armed.remove(&pad).is_some() {
                self.early = Some(pad); // released before ARM: nothing was sent
                return Ok(None);
            }
        }
        self.activate(index, pressed, emit)
    }

    /// v7 ticket 99: the risky control last let go before its one second, once.
    pub fn take_early(&mut self) -> Option<Control> {
        self.early.take()
    }
    /// Fire risky controls held for at least `ARM` by `now`.
    pub fn fire_due(
        &mut self,
        now: Instant,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<Vec<(Control, bool)>> {
        let due: Vec<_> = self
            .armed
            .iter()
            .filter(|(_, at)| now.saturating_duration_since(**at) >= ARM)
            .map(|(pad, _)| *pad)
            .collect();
        let mut fired = Vec::new();
        for pad in due {
            self.armed.remove(&pad);
            let index = self.assignments.iter().position(|a| a.control == pad);
            if let (true, Some(index)) = (self.running, index) {
                fired.extend(self.activate(index, true, emit)?);
            }
        }
        Ok(fired)
    }

    fn activate(
        &mut self,
        index: usize,
        pressed: bool,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<Option<(Control, bool)>> {
        let assignment = &self.assignments[index];
        let pad = assignment.control;
        if assignment.behavior == Behavior::Tap {
            // v7 ticket 96: one tap per press: keys down, then up in reverse; nothing stays
            // held, and a key another held assignment shares is left alone.
            if !pressed {
                return Ok(None);
            }
            let keys = assignment
                .keys
                .iter()
                .copied()
                .filter(|key| {
                    !self.assignments.iter().enumerate().any(|(other, item)| {
                        other != index && item.active && item.keys.contains(key)
                    })
                })
                .collect::<Vec<_>>();
            // Tracked like `tap`: a failed emit leaves pressed keys for `cleanup` to release.
            for &key in &keys {
                self.held.insert(key);
                emit(key, true)?;
            }
            for &key in keys.iter().rev() {
                emit(key, false)?;
                self.held.remove(&key);
            }
            return Ok(Some((pad, true)));
        }
        let active = match assignment.behavior {
            Behavior::Trigger => return Ok(pressed.then_some((pad, true))),
            Behavior::Tap => unreachable!("handled above"),
            Behavior::Hold | Behavior::Pulse => pressed,
            Behavior::Toggle if pressed => !assignment.active,
            Behavior::Toggle => return Ok(None),
        };
        if active == assignment.active {
            return Ok(None);
        }
        let mut keys = assignment.keys.clone();
        if !active {
            keys.reverse();
        }
        for key in keys {
            // ponytail: scan active assignments; use counts if large setups make this measurable.
            let shared = self
                .assignments
                .iter()
                .enumerate()
                .any(|(other, item)| other != index && item.active && item.keys.contains(&key));
            if shared {
                continue;
            }
            if active {
                self.held.insert(key);
                emit(key, true)?;
            } else {
                emit(key, false)?;
                self.held.remove(&key);
            }
        }
        self.assignments[index].active = active;
        Ok(Some((pad, active)))
    }

    pub fn pause(
        &mut self,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        self.running = false;
        self.clear(emit)
    }

    pub fn clear(
        &mut self,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        let down = self
            .down
            .iter()
            .filter(|c| c.message == crate::Message::Note)
            .copied()
            .collect();
        let blocked = self.blocked.clone();
        let result = self.cleanup(emit);
        self.down = down;
        self.blocked = blocked; // repeats cannot reacquire keys until a real release
        result
    }

    pub fn replace(&mut self, mappings: &[Mapping]) -> Result<()> {
        if self.running || !self.held.is_empty() {
            return Err("pause and release keys before editing mappings".into());
        }
        let mut next = Self::new(mappings)?;
        next.down = self.down.clone();
        next.blocked.extend(&self.blocked);
        next.running = false;
        *self = next;
        Ok(())
    }

    /// Press then release `keys` once. Keys already held by an active mapping stay held.
    pub fn tap(
        &mut self,
        keys: &[KeyCode],
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        if !self.running {
            return Err(io::Error::other("output paused; keys not sent"));
        }
        let pressed: Vec<_> = keys
            .iter()
            .copied()
            .filter(|k| !self.held.contains(k))
            .collect();
        for &key in &pressed {
            self.held.insert(key);
            emit(key, true)?;
        }
        for &key in pressed.iter().rev() {
            emit(key, false)?;
            self.held.remove(&key);
        }
        Ok(())
    }

    pub fn resume(&mut self) {
        self.running = true;
    }

    pub fn block_down(&mut self, down: &HashSet<Control>) {
        self.down = down.clone();
        self.blocked.extend(down);
    }

    pub fn prepare_connection(
        &mut self,
        mappings: &[Mapping],
        down: &HashSet<Control>,
        reconnected: &mut bool,
    ) -> Result<bool> {
        self.replace(mappings)?;
        self.block_down(down);
        // A pad held through the transition sends no new note-on, so it cannot fire until
        // pressed again; queued input is drained while output is still paused.
        Ok(std::mem::take(reconnected))
    }

    pub fn active(&self, control: Control) -> bool {
        self.assignments
            .iter()
            .any(|assignment| assignment.control == control && assignment.active)
    }

    pub fn status(&self, name: impl Fn(Control) -> String) -> String {
        let active = self
            .assignments
            .iter()
            .filter(|a| a.active)
            .map(|a| name(a.control))
            .collect::<Vec<_>>();
        let mut down = self.down.iter().map(|p| name(*p)).collect::<Vec<_>>();
        down.sort();
        let mut arming = self
            .armed
            .iter()
            .map(|(pad, at)| {
                let done = at.elapsed().as_millis().min(ARM.as_millis()) * 100 / ARM.as_millis();
                format!("{} {done}%", name(*pad))
            })
            .collect::<Vec<_>>();
        arming.sort();
        format!(
            "{} | {} saved (/list shows keys/modes) | {} await release\nActive holds/toggles: {}\nPhysically down: {}{}",
            if self.running { "RUNNING" } else { "PAUSED" },
            self.assignments.len(),
            self.blocked.len(),
            if active.is_empty() {
                "none".into()
            } else {
                active.join(", ")
            },
            if down.is_empty() {
                "none".into()
            } else {
                down.join(", ")
            },
            if arming.is_empty() {
                String::new()
            } else {
                format!("\nArming risky (hold 1 s): {}", arming.join(", "))
            }
        )
    }

    fn cleanup(
        &mut self,
        emit: &mut impl FnMut(KeyCode, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut keys: Vec<_> = self.held.iter().copied().collect();
        keys.sort_by_key(|key| (modifier(*key), key.0));
        let mut failure = None;
        for key in keys {
            match emit(key, false) {
                Ok(()) => {
                    self.held.remove(&key);
                }
                Err(error) => {
                    failure = Some(error);
                }
            }
        }
        self.down.clear();
        self.blocked.clear();
        self.armed.clear();
        for assignment in &mut self.assignments {
            assignment.active = false;
        }
        failure.map_or(Ok(()), Err)
    }
}

fn create_output() -> Result<VirtualDevice> {
    // Advertise a standard keyboard range so modifier-only mappings are classified as keyboards.
    let mut supported = AttributeSet::<KeyCode>::new();
    for code in 1..=KeyCode::KEY_RIGHTMETA.0 {
        supported.insert(KeyCode(code));
    }
    // v7.8 ticket 10: the media keys Music & Sound taps.
    for media in crate::actions::Media::ALL {
        supported.insert(media.key());
    }
    let output = VirtualDevice::builder()
        .and_then(|builder| {
            builder
                .name("VibeConsole virtual keyboard")
                .with_keys(&supported)?
                .build()
        })
        .map_err(|error| {
            format!(
                "cannot create keyboard through /dev/uinput: {error}; see README.md for permissions"
            )
        })?;
    Ok(output)
}

pub struct Output {
    pub keyboard: Keyboard,
    /// None: a recording output (v7 ticket 51), for session tests; nothing reaches the desktop.
    device: Option<VirtualDevice>,
    pub node: std::path::PathBuf,
    /// Keys a recording output was asked to press (true) and release (false), in order.
    pub recorded: Vec<(KeyCode, bool)>,
}

fn send(
    device: &mut Option<VirtualDevice>,
    recorded: &mut Vec<(KeyCode, bool)>,
    key: KeyCode,
    down: bool,
) -> io::Result<()> {
    match device {
        Some(device) => device.emit(&[InputEvent::new(EventType::KEY.0, key.0, i32::from(down))]),
        None => {
            recorded.push((key, down));
            // v7.8 ticket 01: an emulated run's keys, for its checks to read.
            crate::emulated_log(
                "output.log",
                &format!("key {key:?} {}", if down { "down" } else { "up" }),
            );
            Ok(())
        }
    }
}

/// v7 ticket 113: the computer keyboard as an agent status light. One LED per keyboard
/// VibeConsole may write to: Scroll Lock where the keyboard has it, else Caps Lock. Only the
/// LED is lit; the lock state itself is never toggled, so typing is unaffected. Each LED goes
/// back to what it showed before on `restore` and on drop (pause, error, quit).
/// v7.8 ticket 01: an emulated run writes to no real light.
pub fn off_when_emulated(device: &str) -> Result<()> {
    match crate::emulated() {
        Some(_) => Err(format!("{device}: unavailable in an emulated run").into()),
        None => Ok(()),
    }
}

pub struct LockLights {
    leds: Vec<Led>,
    lit: Option<bool>,
    /// Chosen lights no keyboard offers, as messages (v7 ticket 128).
    pub missing: Vec<String>,
}
/// OpenRazer's keyboard driver in sysfs (v7 ticket 128).
pub const RAZER: &str = "/sys/bus/hid/drivers/razerkbd";
fn led_name(led: evdev::LedCode) -> &'static str {
    match led {
        evdev::LedCode::LED_CAPSL => "Caps Lock",
        evdev::LedCode::LED_NUML => "Num Lock",
        _ => "Scroll Lock",
    }
}
/// v7 ticket 128: each Razer keyboard's M (macro) light, from OpenRazer's `macro_led_state`
/// (read, then written `1`/`0`, github.com/openrazer/openrazer wiki "Using the keyboard
/// driver"). Only files that can be read and written are used. The G light is never used:
/// `game_led_state` turns on Razer game mode, which turns off the Super key.
pub fn macro_leds(driver: &Path) -> Vec<Led> {
    let mut dirs = std::fs::read_dir(driver)
        .map(|entries| entries.flatten().map(|e| e.path()).collect::<Vec<_>>())
        .unwrap_or_default();
    dirs.sort();
    dirs.into_iter()
        .filter_map(|dir| {
            let file = dir.join("macro_led_state");
            let was = std::fs::read_to_string(&file).ok()?.trim() == "1";
            std::fs::OpenOptions::new().write(true).open(&file).ok()?;
            let name = std::fs::read_to_string(dir.join("device_type"))
                .map(|t| t.trim().to_owned())
                .unwrap_or_else(|_| "Razer keyboard".into());
            Some(Led::new(
                &format!("{name}: M light"),
                was,
                Box::new(move |on| std::fs::write(&file, if on { "1" } else { "0" })),
            ))
        })
        .collect()
}
/// One keyboard's LED: its name, what it showed before, and how to set it.
pub struct Led {
    pub name: String,
    was: bool,
    write: Box<dyn FnMut(bool) -> io::Result<()> + Send>,
}
impl Led {
    pub fn new(
        name: &str,
        was: bool,
        write: Box<dyn FnMut(bool) -> io::Result<()> + Send>,
    ) -> Self {
        Self {
            name: name.to_owned(),
            was,
            write,
        }
    }
}
impl LockLights {
    /// Every keyboard with a Scroll Lock or Caps Lock LED that can be opened for writing.
    /// Nothing is sent until `show`.
    pub fn open() -> Result<Self> {
        Self::open_with([false; 4], Path::new(RAZER))
    }
    /// v7 ticket 128: the lights chosen in `crate::status::KEY_LEDS` order (Scroll, Caps, Num
    /// lock LEDs on every writable keyboard that has them; a Razer keyboard's M light through
    /// OpenRazer's driver files under `razer`). None chosen: Scroll Lock, else Caps Lock.
    /// A chosen light no keyboard offers is named in `missing`; nothing is sent until `show`.
    pub fn open_with(chosen: [bool; 4], razer: &Path) -> Result<Self> {
        use evdev::{EventType, InputEvent, LedCode};
        off_when_emulated("Keyboard lights")?;
        let auto = chosen == [false; 4];
        let codes = [LedCode::LED_SCROLLL, LedCode::LED_CAPSL, LedCode::LED_NUML];
        let mut leds = Vec::new();
        let mut found = [false; 4];
        for (path, device) in evdev::enumerate() {
            let Some(supported) = device.supported_leds() else {
                continue;
            };
            let wanted = if auto {
                codes[..2]
                    .iter()
                    .find(|c| supported.contains(**c))
                    .into_iter()
                    .copied()
                    .collect::<Vec<_>>()
            } else {
                codes
                    .iter()
                    .zip(chosen)
                    .filter(|(c, on)| *on && supported.contains(**c))
                    .map(|(c, _)| *c)
                    .collect()
            };
            let typing = device
                .supported_keys()
                .is_some_and(|k| k.contains(KeyCode::KEY_A));
            let writable = std::fs::OpenOptions::new().write(true).open(&path).is_ok();
            if wanted.is_empty() || !typing || !writable {
                continue;
            }
            for led in wanted {
                let Ok(mut device) = evdev::Device::open(&path) else {
                    continue;
                };
                let was = device.get_led_state().is_ok_and(|s| s.contains(led));
                let name = device.name().unwrap_or("keyboard").to_owned();
                if let Some(i) = codes.iter().position(|c| *c == led) {
                    found[i] = true;
                }
                leds.push(Led::new(
                    &format!("{name}: {}", led_name(led)),
                    was,
                    Box::new(move |on| {
                        device.send_events(&[InputEvent::new(
                            EventType::LED.0,
                            led.0,
                            i32::from(on),
                        )])
                    }),
                ));
            }
        }
        if chosen[3] {
            let macros = macro_leds(razer);
            found[3] = !macros.is_empty();
            leds.extend(macros);
        }
        let missing = crate::status::KEY_LEDS
            .iter()
            .zip(chosen.into_iter().zip(found))
            .filter(|(_, (on, found))| *on && !found)
            .map(|(name, _)| match *name {
                "macro" => "M light: unavailable: OpenRazer's razerkbd driver isn't loaded, or its macro_led_state isn't writable (VibeConsole never installs it)".to_owned(),
                other => format!("{other} lock LED: no writable keyboard has one"),
            })
            .collect::<Vec<_>>();
        if leds.is_empty() {
            return Err(match missing.first() {
                Some(reason) => format!("keyboard light unavailable: {reason}").into(),
                None => "keyboard light unavailable: no keyboard with a Scroll Lock or Caps Lock LED can be written (check /dev/input permissions)".into(),
            });
        }
        let mut lights = Self::with(leds);
        lights.missing = missing;
        Ok(lights)
    }
    pub fn with(leds: Vec<Led>) -> Self {
        Self {
            leds,
            lit: None,
            missing: Vec::new(),
        }
    }
    pub fn names(&self) -> Vec<&str> {
        self.leds.iter().map(|l| l.name.as_str()).collect()
    }
    /// Light every LED or put it out; nothing is sent when it already shows that. A keyboard
    /// whose write fails (unplugged) is dropped and the others keep showing (v7 ticket 114);
    /// only when none is left is it an error.
    pub fn show(&mut self, on: bool) -> io::Result<()> {
        if self.lit == Some(on) {
            return Ok(());
        }
        let mut failed = None;
        self.leds.retain_mut(|led| match (led.write)(on) {
            Ok(()) => true,
            Err(e) => {
                failed = Some(e);
                false
            }
        });
        if let (Some(error), true) = (failed, self.leds.is_empty()) {
            return Err(error);
        }
        self.lit = Some(on);
        Ok(())
    }
    /// Each LED back to what it showed before VibeConsole lit it.
    pub fn restore(&mut self) {
        if self.lit.take().is_some() {
            for led in &mut self.leds {
                let _ = (led.write)(led.was);
            }
        }
    }
}
impl Drop for LockLights {
    fn drop(&mut self) {
        self.restore();
    }
}

/// v7 ticket 128: Razer keyboards' RGB backlight through OpenRazer's razerkbd files
/// (github.com/openrazer/openrazer `driver/razerkbd_driver.c`): `matrix_effect_static` and
/// `matrix_effect_breath` take 3 raw RGB bytes, `matrix_effect_reactive` a speed byte (1–4)
/// then RGB, `matrix_effect_wave` a direction `1`/`2`, `matrix_effect_spectrum` and
/// `matrix_effect_none` any write; `matrix_brightness` is 0–255 as text and can be read.
/// The effect files are write-only, so the effect from before can't be read back: by the
/// user's choice (15 Nov 2026) the resting effect they pick is what goes back on pause, quit
/// and error, and the brightness read at the start is written back.
pub struct RgbKeyboards {
    boards: Vec<RgbBoard>,
    shown: Option<(crate::status::Rgb, u8)>,
    /// v7.7 ticket 188: the resting effect last written, so idle shows it once and a changed
    /// one shows at once.
    resting: Option<crate::status::Rgb>,
    pub rest: crate::status::Rgb,
    /// v7.7 ticket 189: set when a write skipped a device that lacks the effect, for the
    /// caller to show once.
    pub unavailable: Option<String>,
}
struct RgbBoard {
    dir: std::path::PathBuf,
    name: String,
    /// Its own brightness, read at the start; None: left alone (v7.7 ticket 189).
    brightness: Option<u8>,
    /// The effect files it has and can write.
    can: Vec<&'static str>,
}
const EFFECT_FILES: [&str; 6] = [
    "matrix_effect_none",
    "matrix_effect_spectrum",
    "matrix_effect_wave",
    "matrix_effect_static",
    "matrix_effect_breath",
    "matrix_effect_reactive",
];
/// The effect file and the bytes OpenRazer's driver takes for an effect (v7.7 ticket 193's
/// table: OpenRazer 3.12.4). A colour is 3 raw bytes; breath takes 3 (one colour), 6 (two)
/// or 1 (random) bytes; reactive a speed byte (1–4, clamped by the driver) then RGB; wave
/// its direction as text.
fn rgb_write(effect: crate::status::Rgb) -> (&'static str, Vec<u8>) {
    use crate::status::Rgb;
    match effect {
        Rgb::Off => ("matrix_effect_none", b"1".to_vec()),
        Rgb::Spectrum(_) => ("matrix_effect_spectrum", b"1".to_vec()),
        // Direction 1 unless one was picked, as before ticket 199.
        Rgb::Wave(d) => (
            "matrix_effect_wave",
            d.unwrap_or(1).to_string().into_bytes(),
        ),
        Rgb::Static(c) => ("matrix_effect_static", c.to_vec()),
        Rgb::Breath(c, _) => ("matrix_effect_breath", c.to_vec()),
        Rgb::BreathTwo(a, b) => ("matrix_effect_breath", [a, b].concat()),
        Rgb::BreathRandom => ("matrix_effect_breath", b"1".to_vec()),
        // Speed 2 unless one was picked, as before ticket 199.
        Rgb::Reactive(c, speed) => (
            "matrix_effect_reactive",
            [&[speed.unwrap_or(2)][..], &c].concat(),
        ),
    }
}
impl RgbBoard {
    /// Ok(false): this device lacks the effect, so nothing is written.
    fn write(&self, effect: crate::status::Rgb) -> io::Result<bool> {
        let (file, bytes) = rgb_write(effect);
        if !self.can.contains(&file) {
            return Ok(false);
        }
        std::fs::write(self.dir.join(file), bytes)?;
        Ok(true)
    }
}
/// v7.7 ticket 189: OpenRazer's driver for speakers, mousepads and other accessories.
pub const RAZER_ACCESSORY: &str = "/sys/bus/hid/drivers/razeraccessory";
impl RgbKeyboards {
    /// Every keyboard under `driver` whose brightness can be read and whose effect files can
    /// be written. Nothing is sent until `show`.
    pub fn open(driver: &Path, rest: crate::status::Rgb) -> Result<Self> {
        off_when_emulated("RGB keyboard")?;
        Self::open_with(driver, rest, None).map_err(|()| {
            "RGB keyboard: unavailable: OpenRazer's razerkbd driver isn't loaded, or its matrix effect files aren't writable (VibeConsole never installs it)".into()
        })
    }
    /// v7.7 ticket 189: the Razer accessories under `driver` whose `device_type` contains
    /// `model` (`status::ACCESSORIES`). Their brightness is left alone: OpenRazer reads the
    /// Nommo Chroma's from another LED than it writes (it reads 0), so writing it back would
    /// turn them dark. An effect file a device lacks is never written.
    pub fn open_accessory(
        driver: &Path,
        rest: crate::status::Rgb,
        label: &str,
        model: &str,
    ) -> Result<Self> {
        off_when_emulated(label)?;
        Self::open_with(driver, rest, Some(model)).map_err(|()| {
            format!("{label}: unavailable: no Razer {model} under OpenRazer's razeraccessory driver, or its matrix effect files aren't writable (VibeConsole never installs it)").into()
        })
    }
    fn open_with(
        driver: &Path,
        rest: crate::status::Rgb,
        model: Option<&str>,
    ) -> std::result::Result<Self, ()> {
        let mut dirs = std::fs::read_dir(driver)
            .map(|entries| entries.flatten().map(|e| e.path()).collect::<Vec<_>>())
            .unwrap_or_default();
        dirs.sort();
        let writable = |dir: &Path, file: &str| {
            std::fs::OpenOptions::new()
                .write(true)
                .open(dir.join(file))
                .is_ok()
        };
        let boards = dirs
            .into_iter()
            .filter_map(|dir| {
                let name = std::fs::read_to_string(dir.join("device_type"))
                    .map(|t| t.trim().to_owned())
                    .ok();
                let brightness = match model {
                    Some(model) => {
                        name.as_deref().filter(|n| n.contains(model))?;
                        None
                    }
                    None => {
                        let level = std::fs::read_to_string(dir.join("matrix_brightness"))
                            .ok()?
                            .trim()
                            .parse()
                            .ok()?;
                        if !writable(&dir, "matrix_brightness") {
                            return None;
                        }
                        Some(level)
                    }
                };
                let can = EFFECT_FILES
                    .into_iter()
                    .filter(|f| writable(&dir, f))
                    .collect::<Vec<_>>();
                // A keyboard has them all (v7 ticket 128); an accessory at least `none`.
                let enough = match model {
                    None => can.len() == EFFECT_FILES.len(),
                    Some(_) => can.contains(&"matrix_effect_none"),
                };
                enough.then(|| RgbBoard {
                    dir,
                    name: name.unwrap_or_else(|| "Razer keyboard".into()),
                    brightness,
                    can,
                })
            })
            .collect::<Vec<_>>();
        if boards.is_empty() {
            return Err(());
        }
        Ok(Self {
            boards,
            shown: None,
            resting: None,
            rest,
            unavailable: None,
        })
    }
    pub fn names(&self) -> Vec<&str> {
        self.boards.iter().map(|b| b.name.as_str()).collect()
    }
    /// v7.7 ticket 189: each device and the effects it can show, for `doctor`.
    pub fn abilities(&self) -> Vec<String> {
        self.boards
            .iter()
            .map(|b| {
                let words = b
                    .can
                    .iter()
                    .map(|f| match *f {
                        "matrix_effect_none" => "off",
                        f => f.trim_start_matches("matrix_effect_"),
                    })
                    .collect::<Vec<_>>();
                format!("{}: {}", b.name, words.join(", "))
            })
            .collect()
    }
    /// Write `effect` to every device (with `brightness` first where it's ours to set). A
    /// device that lacks it is skipped and named in `unavailable`; one whose write fails
    /// (unplugged) is dropped, and only when none is left is it an error.
    fn send(&mut self, effect: crate::status::Rgb, brightness: Option<u8>) -> io::Result<()> {
        let mut failed = None;
        let mut lacking = Vec::new();
        // v7.7 ticket 212: an in-step frame keeps the brightness already written.
        let written = self.shown.map(|(_, level)| level);
        self.boards.retain(|board| {
            let level = brightness.filter(|l| board.brightness.is_some() && written != Some(*l));
            let sent = level
                .map_or(Ok(()), |l| {
                    std::fs::write(board.dir.join("matrix_brightness"), l.to_string())
                })
                .and_then(|()| board.write(effect))
                .and_then(|written| {
                    if !written {
                        lacking.push(board.name.clone());
                    }
                    // The resting effect goes back with its own brightness.
                    match (brightness, board.brightness) {
                        (None, Some(own)) => {
                            std::fs::write(board.dir.join("matrix_brightness"), own.to_string())
                        }
                        _ => Ok(()),
                    }
                });
            sent.map_err(|e| failed = Some(e)).is_ok()
        });
        if !lacking.is_empty() {
            self.unavailable = Some(format!(
                "{} unavailable on {}: it has no {} effect",
                crate::status::rgb_name(effect),
                lacking.join(", "),
                rgb_write(effect).0.trim_start_matches("matrix_effect_")
            ));
        }
        match (failed, self.boards.is_empty()) {
            (Some(error), true) => Err(error),
            _ => Ok(()),
        }
    }
    /// Show an effect at a brightness; nothing is sent when it already shows them.
    pub fn show(&mut self, effect: crate::status::Rgb, brightness: u8) -> io::Result<()> {
        if self.shown == Some((effect, brightness)) {
            return Ok(());
        }
        self.send(effect, Some(brightness))?;
        self.shown = Some((effect, brightness));
        self.resting = None;
        Ok(())
    }
    /// v7.7 ticket 188: running and idle, the resting effect with each keyboard's own
    /// brightness: from the start, before any state showed, and at once when it changes.
    /// Sent once; errors as `show`.
    pub fn show_rest(&mut self) -> io::Result<()> {
        if self.shown.is_none() && self.resting == Some(self.rest) {
            return Ok(());
        }
        self.send(self.rest, None)?;
        self.shown = None;
        self.resting = Some(self.rest);
        Ok(())
    }
    /// The resting effect and each keyboard's own brightness back, once something was shown
    /// (or a resting effect VibeConsole wrote has since changed). Pause, quit and error.
    pub fn restore(&mut self) {
        let stale = self.resting.is_some_and(|r| r != self.rest);
        if self.shown.is_some() || stale {
            let _ = self.show_rest();
        }
    }
}
/// v7.7 ticket 190: the Logitech G502 (SE) HERO's G logo light through `logirgb`
/// (github.com/mevdschee/logirgb, tested with v0.6.0), as one argument per value. A state is
/// shown with `--preview`, which never stores into the mouse's onboard profiles. logirgb's
/// `release` does nothing on the G502 (it only acts on 0x8071 devices; the G502 has 0x8070,
/// seen 17 Nov 2026), so the restore reads the active profile's stored logo effect back and
/// shows that. The mouse's own lighting is its resting effect: nothing to pick.
pub struct MouseLight {
    run: crate::gamepad::Runner,
    shown: Option<crate::status::Rgb>,
    /// Set when an effect the logo lacks was asked for, for the caller to show once.
    pub unavailable: Option<String>,
}
/// v7.7 ticket 190: the real runner for logirgb: found on PATH, else where `go install`
/// puts it (`~/go/bin`), which not every shell has on PATH. One argument per value.
pub fn logirgb_runner() -> crate::gamepad::Runner {
    let mut run = crate::gamepad::run_program();
    let on_path = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| dir.join("logirgb").is_file());
    let go_bin = std::env::var_os("HOME")
        .map(|home| Path::new(&home).join("go/bin/logirgb"))
        .filter(|p| !on_path && p.is_file())
        .map(|p| p.to_string_lossy().into_owned());
    Box::new(move |args: &[&str]| match (&go_bin, args.split_first()) {
        (Some(program), Some((&"logirgb", rest))) => {
            let mut full = vec![program.as_str()];
            full.extend(rest);
            run(&full)
        }
        _ => run(args),
    })
}
/// logirgb's name for the mouse (`-d` takes a name fragment) and the logo zone.
const MOUSE: [&str; 4] = ["-d", "g502", "-z", "1"];
impl MouseLight {
    /// Reachable only when `logirgb profiles` names the G502's active profile; nothing is
    /// sent until `show`.
    pub fn open(mut run: crate::gamepad::Runner) -> Result<Self> {
        off_when_emulated("Mouse RGB")?;
        active_profile(&mut run).map_err(|e| {
            format!(
                "Mouse light (Logitech G502): unavailable: {e} (VibeConsole never installs logirgb)"
            )
        })?;
        Ok(Self {
            run,
            shown: None,
            unavailable: None,
        })
    }
    /// The logo's effect for a state; nothing is sent when it already shows it. Its own
    /// breathe and cycle settings go along when set (v7.7 ticket 199).
    pub fn show(&mut self, effect: crate::status::Rgb) -> Result<()> {
        use crate::status::{FORMS, Rgb, Timing, colour_word};
        if self.shown == Some(effect) {
            return Ok(());
        }
        let timing = |t: Timing, form: bool| {
            let mut out = Vec::new();
            if let Some(p) = t.period {
                out.extend(["--period".to_owned(), p.to_string()]);
            }
            if let Some(f) = t.form.filter(|_| form) {
                out.extend(["--form".to_owned(), FORMS[usize::from(f)].to_owned()]);
            }
            if let Some(i) = t.intensity {
                out.extend(["--intensity".to_owned(), i.to_string()]);
            }
            out
        };
        let words: Vec<String> = match effect {
            Rgb::Off => vec!["off".into()],
            Rgb::Static(c) => vec!["static".into(), "--color".into(), colour_word(c)],
            Rgb::Breath(c, t) => [
                vec!["breathe".into(), "--color".into(), colour_word(c)],
                timing(t, true),
            ]
            .concat(),
            Rgb::Spectrum(t) => [vec!["cycle".into()], timing(t, false)].concat(),
            Rgb::BreathTwo(..) | Rgb::BreathRandom | Rgb::Wave(_) | Rgb::Reactive(..) => {
                self.unavailable = Some(format!(
                    "{} unavailable on the G502 logo: it shows off, static, breath with one colour and spectrum only",
                    crate::status::rgb_name(effect)
                ));
                return Ok(());
            }
        };
        let mut args = vec!["logirgb", "set"];
        args.extend(MOUSE);
        args.extend(words.iter().map(String::as_str));
        args.push("--preview");
        (self.run)(&args)?;
        self.shown = Some(effect);
        Ok(())
    }
    /// The active profile's stored logo effect back, once something was shown. Pause, quit,
    /// error; a failure is ignored (the next profile switch or replug restores it too).
    pub fn restore(&mut self) {
        if self.shown.take().is_some() {
            let _ = self.put_back();
        }
    }
    fn put_back(&mut self) -> Result<()> {
        let profile = active_profile(&mut self.run)?;
        let list = (self.run)(&["logirgb", "list"])?;
        let words = stored_logo(&list, profile)
            .ok_or_else(|| format!("logirgb list has no logo effect for profile {profile}"))?;
        let mut args = vec!["logirgb", "set"];
        args.extend(MOUSE);
        args.extend(words.iter().map(String::as_str));
        args.push("--preview");
        (self.run)(&args).map(|_| ())
    }
}
impl Drop for MouseLight {
    fn drop(&mut self) {
        self.restore();
    }
}
/// v7.7 ticket 191: what the G502 logo should show.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MouseWant {
    Show(crate::status::Rgb),
    /// The stored lighting back: idle or paused.
    Rest,
    /// Switched off: the stored lighting back, and a missing logirgb is asked again next time.
    Off,
}
/// v7.7 ticket 191: logirgb takes about half a second per call on the G502 (17 Nov 2026:
/// `profiles` 515 ms, `list` 507 ms, `set` 111 ms before it even reaches the mouse), so the
/// logo is driven on its own thread and the screen never waits. Only the latest want is acted
/// on; older ones are dropped, never replayed. Dropping the worker puts the stored lighting
/// back and waits for it, so quit restores before VibeConsole exits.
pub struct MouseWorker {
    wants: Option<std::sync::mpsc::Sender<MouseWant>>,
    notices: std::sync::mpsc::Receiver<String>,
    thread: Option<std::thread::JoinHandle<()>>,
    sent: Option<MouseWant>,
    /// v7.7 ticket 192: logirgb couldn't open the mouse (until switched off).
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// v7.7 ticket 239: the last effect logirgb finished setting, and when it returned.
    done: std::sync::Arc<std::sync::Mutex<Option<(MouseWant, std::time::Instant)>>>,
}
impl MouseWorker {
    /// `runner` makes a fresh logirgb runner for each open (again after a failed `set`).
    pub fn start(mut runner: impl FnMut() -> crate::gamepad::Runner + Send + 'static) -> Self {
        let (wants, wanted) = std::sync::mpsc::channel::<MouseWant>();
        let (notify, notices) = std::sync::mpsc::channel();
        let unavailable = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = unavailable.clone();
        let done = std::sync::Arc::new(std::sync::Mutex::new(None));
        let finished = done.clone();
        let thread = std::thread::spawn(move || {
            let mut mouse: Option<MouseLight> = None;
            let mut failed = false;
            while let Ok(mut want) = wanted.recv() {
                while let Ok(newer) = wanted.try_recv() {
                    want = newer;
                }
                let MouseWant::Show(effect) = want else {
                    if let Some(mouse) = &mut mouse {
                        mouse.restore();
                    }
                    failed &= want != MouseWant::Off;
                    seen.store(failed, std::sync::atomic::Ordering::Relaxed);
                    continue;
                };
                if mouse.is_none() && !failed {
                    match MouseLight::open(runner()) {
                        Ok(opened) => mouse = Some(opened),
                        Err(error) => {
                            failed = true;
                            seen.store(true, std::sync::atomic::Ordering::Relaxed);
                            let _ = notify.send(error.to_string());
                        }
                    }
                }
                let Some(open) = &mut mouse else { continue };
                let shown = open.show(effect);
                if shown.is_ok() {
                    *finished.lock().unwrap() = Some((want, std::time::Instant::now()));
                }
                if let Some(missing) = open.unavailable.take() {
                    let _ = notify.send(format!("Mouse light (Logitech G502): {missing}"));
                }
                if let Err(error) = shown {
                    mouse = None; // dropping it restores what it can
                    let _ = notify.send(format!("Mouse light (Logitech G502): {error}; given up"));
                }
            }
            // The wants closed: `mouse` drops here and puts back what it showed.
        });
        Self {
            wants: Some(wants),
            notices,
            thread: Some(thread),
            sent: None,
            failed: unavailable,
            done,
        }
    }
    /// v7.7 ticket 239: when logirgb finished setting the latest want (None until it has),
    /// so the other lights can start their cycle with the mouse's. No logirgb call is added.
    pub fn shown_since(&self) -> Option<std::time::Instant> {
        match (*self.done.lock().unwrap(), self.sent) {
            (Some((want, at)), Some(sent)) if want == sent => Some(at),
            _ => None,
        }
    }
    /// Whether logirgb couldn't open the mouse, as far as the worker has got.
    pub fn unavailable(&self) -> bool {
        self.failed.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// Hands over a want when it differs from the last one; never waits for logirgb.
    pub fn want(&mut self, want: MouseWant) {
        if self.sent != Some(want) {
            self.sent = Some(want);
            if let Some(wants) = &self.wants {
                let _ = wants.send(want);
            }
        }
    }
    /// What the worker reported since the last call (unavailable, given up), oldest first.
    pub fn notices(&self) -> Vec<String> {
        self.notices.try_iter().collect()
    }
}
impl Drop for MouseWorker {
    fn drop(&mut self) {
        if let Some(wants) = self.wants.take() {
            let _ = wants.send(MouseWant::Rest);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
/// The G502's active onboard profile from `logirgb profiles -d g502` ("… 5 on (active)").
fn active_profile(run: &mut crate::gamepad::Runner) -> Result<u8> {
    let out = run(&["logirgb", "profiles", "-d", "g502"])?;
    out.split(',')
        .find(|p| p.contains("(active)"))
        .and_then(|p| p.split_whitespace().rev().nth(2)?.parse().ok())
        .ok_or_else(|| format!("no G502 active profile in logirgb's answer: {}", out.trim()).into())
}
/// The `set` words for a profile's stored logo effect in `logirgb list`, e.g.
/// "profile 5: static color=#8000ff" under "zone 1 (logo)" → `static --color #8000ff`.
fn stored_logo(list: &str, profile: u8) -> Option<Vec<String>> {
    let zone = list.split("zone 1 (logo)").nth(1)?;
    let line = zone
        .lines()
        .find_map(|l| l.trim().strip_prefix(&format!("profile {profile}: ")))?;
    let mut words = line.split_whitespace();
    let mut out = vec![words.next()?.to_owned()];
    for pair in words {
        let (key, value) = pair.split_once('=')?;
        let value = value.trim_end_matches("ms").trim_end_matches('%');
        if key == "form" && value == "default" {
            continue;
        }
        out.extend([format!("--{key}"), value.to_owned()]);
    }
    Some(out)
}

impl Drop for RgbKeyboards {
    fn drop(&mut self) {
        self.restore();
    }
}

impl Output {
    pub fn new(mappings: &[Mapping]) -> Result<Self> {
        if crate::emulated().is_some() {
            return Self::recording(mappings); // v7.8 ticket 01: no uinput keyboard
        }
        let mut keyboard = Keyboard::new(mappings)?;
        keyboard.running = false;
        let mut device = create_output()?;
        let started = std::time::Instant::now();
        let node = loop {
            match device.enumerate_dev_nodes_blocking() {
                Ok(mut nodes) => {
                    if let Some(path) = nodes.next().transpose()? {
                        if path.exists() {
                            break path;
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            if started.elapsed() > Duration::from_secs(2) {
                return Err("keyboard registration timed out".into());
            }
            thread::sleep(Duration::from_millis(20));
        };
        Ok(Self {
            keyboard,
            device: Some(device),
            node,
            recorded: Vec::new(),
        })
    }
    /// An output with no uinput device that records its keys instead (v7 ticket 51): lets a
    /// session test drive real events without keyboard output on the desktop.
    pub fn recording(mappings: &[Mapping]) -> Result<Self> {
        let mut keyboard = Keyboard::new(mappings)?;
        keyboard.running = false;
        Ok(Self {
            keyboard,
            device: None,
            node: std::path::PathBuf::new(),
            recorded: Vec::new(),
        })
    }
    /// v7 ticket 99: see [`Keyboard::take_early`].
    pub fn take_early(&mut self) -> Option<Control> {
        self.keyboard.take_early()
    }
    pub fn fire_due(&mut self) -> Result<Vec<(Control, bool)>> {
        let (device, recorded) = (&mut self.device, &mut self.recorded);
        Ok(self.keyboard.fire_due(Instant::now(), &mut |key, down| {
            send(device, recorded, key, down)
        })?)
    }
    pub fn observe(&mut self, event: Event) -> Result<Option<(Control, bool)>> {
        let (device, recorded) = (&mut self.device, &mut self.recorded);
        Ok(self
            .keyboard
            .observe(event, &mut |key, down| send(device, recorded, key, down))?)
    }
    pub fn tap(&mut self, keys: &[KeyCode]) -> Result<()> {
        let (device, recorded) = (&mut self.device, &mut self.recorded);
        Ok(self
            .keyboard
            .tap(keys, &mut |key, down| send(device, recorded, key, down))?)
    }
    pub fn pause(&mut self) -> Result<()> {
        let (device, recorded) = (&mut self.device, &mut self.recorded);
        self.keyboard
            .pause(&mut |key, down| send(device, recorded, key, down))?;
        Ok(())
    }
    pub fn clear(&mut self) -> Result<()> {
        let (device, recorded) = (&mut self.device, &mut self.recorded);
        self.keyboard
            .clear(&mut |key, down| send(device, recorded, key, down))?;
        Ok(())
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        if let Err(error) = self.pause() {
            eprintln!("Key release failed: {error}; desktop recovery is unverified.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Detector, ReleaseType};
    use std::sync::mpsc;

    /// v7.8 ticket 10: a media key taps through the keyboard like any key: down, then up,
    /// once, and nothing stays held.
    #[test]
    fn media_keys_tap_down_and_up_once() {
        for media in crate::actions::Media::ALL {
            let mut output = Output::recording(&[]).unwrap();
            output.keyboard.resume();
            output.tap(&[media.key()]).unwrap();
            assert_eq!(output.recorded, [(media.key(), true), (media.key(), false)]);
            output.pause().unwrap();
            assert_eq!(output.recorded.len(), 2, "pause releases nothing more");
        }
    }

    #[test]
    fn trigger_pad_piano_knob_duplicates_pause_and_slow_action_never_delay_key_up() {
        use crate::{
            actions::{Action, Dispatcher},
            mappings::Input,
        };
        let hold = Control::from_note(10, 36).unwrap();
        let pad = Control::from_note(10, 37).unwrap();
        let piano = Control::note(1, 48).unwrap();
        let knob = Control {
            channel: 1,
            id: 70,
            message: crate::Message::Cc,
            direction: 1,
        };
        let maps = vec![
            Mapping::new(hold, "Shift", "hold").unwrap(),
            Mapping::new_action(
                pad,
                Input::Pad,
                Action::Application("/test/one.desktop".into()),
            )
            .unwrap(),
            Mapping::new_action(piano, Input::Piano, Action::OutputMute).unwrap(),
            Mapping::new_action(
                knob,
                Input::Knob { step: 4 },
                Action::Volume { up: true, step: 5 },
            )
            .unwrap(),
        ];
        let mut keyboard = Keyboard::new(&maps).unwrap();
        let mut emitted = Vec::new();
        let mut emit = |k, d| {
            emitted.push((k, d));
            Ok(())
        };
        assert_eq!(
            keyboard.observe(Event::Press(pad), &mut emit).unwrap(),
            Some((pad, true))
        ); // the first startup press fires
        keyboard
            .observe(Event::Release(pad, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        assert_eq!(
            keyboard.observe(Event::Press(pad), &mut emit).unwrap(),
            Some((pad, true))
        );
        for event in [
            Event::Press(pad),
            Event::DuplicatePress(pad),
            Event::Pressure(pad),
        ] {
            assert_eq!(keyboard.observe(event, &mut emit).unwrap(), None);
        }
        keyboard
            .observe(Event::Release(pad, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard
            .observe(Event::UnmatchedRelease(piano), &mut emit)
            .unwrap();
        assert_eq!(
            keyboard.observe(Event::Press(piano), &mut emit).unwrap(),
            Some((piano, true))
        );
        assert_eq!(
            keyboard.observe(Event::Press(piano), &mut emit).unwrap(),
            None
        );
        let mut motion = crate::motion::Motion::new(&maps).unwrap();
        assert!(motion.observe([0xb0, 70, 0]).unwrap().is_empty());
        let pulse = motion.observe([0xb0, 70, 4]).unwrap().pop().unwrap();
        assert_eq!(
            keyboard.observe(pulse, &mut emit).unwrap(),
            Some((knob, true))
        );
        assert!(motion.observe([0xb0, 70, 4]).unwrap().is_empty());
        let (started, rx) = mpsc::channel();
        let (completed, done) = mpsc::channel();
        let dispatch = Dispatcher::with(move |action, _, cancelled| {
            started.send(action.clone()).unwrap();
            while !cancelled() {
                thread::sleep(Duration::from_millis(1));
            }
            completed.send(()).unwrap();
            Err("slow launch cancelled".into())
        });
        keyboard.observe(Event::Press(hold), &mut emit).unwrap();
        dispatch.submit(maps[1].action.clone(), None).unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        dispatch.submit(Action::OutputMute, None).unwrap();
        let start = std::time::Instant::now();
        keyboard
            .observe(Event::Release(hold, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        assert!(start.elapsed() < Duration::from_millis(100));
        keyboard.pause(&mut emit).unwrap();
        dispatch.cancel();
        done.recv_timeout(Duration::from_secs(1)).unwrap();
        keyboard
            .observe(Event::Release(piano, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        assert_eq!(
            keyboard.observe(Event::Press(pad), &mut emit).unwrap(),
            None
        );
        keyboard.resume();
        assert_eq!(
            keyboard.observe(Event::Press(pad), &mut emit).unwrap(),
            None
        );
        keyboard
            .observe(Event::Release(pad, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        assert_eq!(
            keyboard.observe(Event::Press(pad), &mut emit).unwrap(),
            Some((pad, true))
        );
        keyboard.pause(&mut emit).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err()); // queued mute cancelled
        assert_eq!(
            emitted,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTSHIFT, false)
            ]
        );
    }

    #[test]
    fn toggle_bulbs_follow_real_latches_and_cleanup_without_desktop_output() {
        let control = Control::from_note(10, 36).unwrap();
        let mapping = Mapping::new(control, "Shift", "toggle").unwrap();
        let mut keyboard = Keyboard::new(&[mapping.clone()]).unwrap();
        let mut emit = |_, _| Ok(());
        let view = |keyboard: &Keyboard| {
            crate::screen::toggle_line(
                1,
                &mapping.label,
                &mapping.action_label(),
                keyboard.active(control),
            )
        };
        assert!(view(&keyboard).starts_with("[○]"));
        keyboard.observe(Event::Press(control), &mut emit).unwrap();
        keyboard
            .observe(
                Event::Release(control, crate::ReleaseType::NoteOff),
                &mut emit,
            )
            .unwrap();
        assert!(view(&keyboard).starts_with("[●]"));
        assert!(view(&keyboard).contains("Shift"));
        keyboard.observe(Event::Press(control), &mut emit).unwrap();
        assert!(view(&keyboard).starts_with("[○]"));
        keyboard
            .observe(
                Event::Release(control, crate::ReleaseType::NoteOnVelocityZero),
                &mut emit,
            )
            .unwrap();
        keyboard.observe(Event::Press(control), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        assert!(view(&keyboard).starts_with("[○]"));
        keyboard.replace(&[mapping.clone()]).unwrap();
        assert!(view(&keyboard).starts_with("[○]"));
    }

    #[test]
    fn observed_piano_note_release_and_pad_share_keys_and_cleanup() {
        // 2026-10-07 passive capture: 90 30 7f, 80 30 00; emitted pitch is the identity.
        let piano = Control::note(1, 48).unwrap();
        let pad = Control::from_note(10, 36).unwrap();
        let mut mapping = Mapping::new(piano, "Shift+K", "hold").unwrap();
        mapping.input = crate::mappings::Input::Piano;
        let mut keyboard =
            Keyboard::new(&[Mapping::new(pad, "Shift", "toggle").unwrap(), mapping]).unwrap();
        let mut detector = Detector::default();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        for bytes in [
            [0x99, 36, 127],
            [0x90, 48, 127],
            [0x90, 48, 64],
            [0x80, 48, 0],
        ] {
            if let Some(event) = detector.observe(bytes) {
                keyboard.observe(event, &mut emit).unwrap();
            }
        }
        keyboard.pause(&mut emit).unwrap();
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_K, true),
                (KeyCode::KEY_K, false),
                (KeyCode::KEY_LEFTSHIFT, false)
            ]
        );
        assert!(keyboard.held.is_empty());
    }

    #[test]
    fn context_switch_releases_latch_and_new_context_fires_on_first_press() {
        let old = Control::from_note(10, 36).unwrap();
        let new = Control::note(3, 60).unwrap();
        let mut keyboard = Keyboard::new(&[Mapping::new(old, "Shift", "toggle").unwrap()]).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(old), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        keyboard
            .replace(&[Mapping::new(new, "Ctrl", "hold").unwrap()])
            .unwrap();
        keyboard.resume();
        keyboard.observe(Event::Press(new), &mut emit).unwrap();
        keyboard
            .observe(Event::UnmatchedRelease(new), &mut emit)
            .unwrap();
        keyboard.observe(Event::Press(new), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTSHIFT, false),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_LEFTCTRL, false),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_LEFTCTRL, false)
            ]
        );
    }

    #[test]
    fn disconnect_reconnect_starts_inactive_and_first_press_fires_without_replaying_toggles() {
        let a = Control::from_note(10, 36).unwrap();
        let b = Control::from_note(10, 44).unwrap();
        let mappings = [
            Mapping::new(a, "Shift", "toggle").unwrap(),
            Mapping::new(b, "Ctrl", "hold").unwrap(),
        ];
        let mut keyboard = Keyboard::new(&mappings).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        let mut reconnected = true;
        assert!(
            keyboard
                .prepare_connection(&mappings, &HashSet::new(), &mut reconnected)
                .unwrap()
        );
        keyboard.resume();
        // The toggle latched before the disconnect is not replayed; the first press fires; repeats do not.
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard
            .observe(Event::Release(a, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard
            .observe(Event::UnmatchedRelease(b), &mut emit)
            .unwrap(); // a release whose press was not observed sends nothing
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        // Ordinary resume: released pads fire on their next press.
        keyboard
            .observe(Event::Release(a, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard
            .observe(Event::Release(b, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        assert!(
            !keyboard
                .prepare_connection(&mappings, &HashSet::new(), &mut reconnected)
                .unwrap()
        );
        keyboard.resume();
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTSHIFT, false),
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTSHIFT, false),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_LEFTCTRL, false),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_LEFTCTRL, false)
            ]
        );
        assert!(!keyboard.assignments.iter().any(|a| a.active));
    }

    #[test]
    fn session_toggle_pause_edit_resume_and_release_all_wait_for_fresh_presses() {
        let a = Control::from_note(10, 36).unwrap();
        let b = Control::from_note(10, 44).unwrap();
        let mut keyboard = Keyboard::new(&[Mapping::new(a, "Shift", "toggle").unwrap()]).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        assert!(!keyboard.assignments[0].active);
        // Configuration occurs while paused and tracks controls pressed during editing.
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard
            .replace(&[
                Mapping::new(a, "Ctrl+K", "hold").unwrap(),
                Mapping::new(b, "Shift", "toggle").unwrap(),
            ])
            .unwrap();
        keyboard.resume();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard
            .observe(Event::Release(a, crate::ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard
            .observe(Event::Release(b, crate::ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard.clear(&mut emit).unwrap();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard
            .observe(Event::Release(a, crate::ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard
            .observe(Event::Release(b, crate::ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard.observe(Event::Press(b), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTSHIFT, false),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_K, true),
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_K, false),
                (KeyCode::KEY_LEFTCTRL, false),
                (KeyCode::KEY_LEFTSHIFT, false),
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTSHIFT, false),
            ]
        );
        let previous = keyboard.assignments.len();
        let mut invalid = Mapping::new(a, "Shift", "hold").unwrap();
        invalid.keys = vec!["Unknown".into()];
        assert!(keyboard.replace(&[invalid]).is_err());
        assert_eq!(keyboard.assignments.len(), previous);
    }

    #[test]
    fn stream_release_arrives_during_silence_before_eof() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let pad = Control::from_note(10, 36).unwrap();
        let mut keyboard = Keyboard::new(&[Mapping::new(pad, "Shift", "hold").unwrap()]).unwrap();
        let (mut writer, reader) = UnixStream::pair().unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut detector = Detector::default();
            let mut emit = |key, down| {
                sender.send((key, down)).unwrap();
                Ok(())
            };
            for message in crate::midi::messages(reader) {
                if let Some(event) = detector.observe(message.unwrap()) {
                    keyboard.observe(event, &mut emit).unwrap();
                }
            }
        });
        writer
            .write_all(&[
                0x99, 36, 32, 0xA9, 36, 64, 0xF0, 0x47, 0, 0x49, 0x67, 1, 0xF8, 0xF7, 0x89, 36, 0,
            ])
            .unwrap();
        let press = receiver.recv_timeout(Duration::from_secs(1));
        let release = receiver.recv_timeout(Duration::from_secs(1));
        // Close only after checking: EOF must not be what delivers the release.
        drop(writer);
        worker.join().unwrap();
        assert_eq!(press.unwrap(), (KeyCode::KEY_LEFTSHIFT, true));
        assert_eq!(
            release.expect("release must arrive without a later message or EOF"),
            (KeyCode::KEY_LEFTSHIFT, false)
        );
    }

    #[test]
    fn holds_toggles_overlap_order_cleanup_and_failed_output() {
        let a = Control::from_note(10, 36).unwrap();
        let b = Control::from_note(10, 37).unwrap();
        let c = Control::from_note(10, 44).unwrap();
        let mappings = vec![
            Mapping::new(a, "Ctrl", "hold").unwrap(),
            Mapping::new(b, "C+Ctrl", "toggle").unwrap(),
            Mapping::new(c, "Shift+Ctrl", "toggle").unwrap(),
        ];
        let mut keyboard = Keyboard::new(&mappings).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        let mut detector = Detector::default();
        for bytes in [
            [0x99, 36, 20],
            [0x99, 37, 20],
            [0x99, 37, 30],
            [0xA9, 37, 40],
            [0x89, 37, 0],
            [0x99, 44, 20],
            [0x99, 44, 0],
            [0x89, 36, 0],
            [0x99, 37, 20],
            [0x89, 37, 0],
        ] {
            if let Some(event) = detector.observe(bytes) {
                keyboard.observe(event, &mut emit).unwrap();
            }
        }
        assert_eq!(
            keyboard
                .assignments
                .iter()
                .map(|item| item.active)
                .collect::<Vec<_>>(),
            [false, false, true]
        );
        keyboard.cleanup(&mut emit).unwrap();
        assert!(keyboard.held.is_empty());
        assert!(keyboard.down.is_empty());
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_C, true),
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_C, false),
                (KeyCode::KEY_LEFTCTRL, false),
                (KeyCode::KEY_LEFTSHIFT, false)
            ]
        );

        // A fresh engine starts inactive and releases nonmodifiers before modifiers.
        let mut keyboard =
            Keyboard::new(&[Mapping::new(a, "K+Shift+Ctrl", "hold").unwrap()]).unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.observe(Event::Pressure(a), &mut emit).unwrap();
        keyboard
            .observe(Event::Release(a, ReleaseType::NoteOff), &mut emit)
            .unwrap();
        keyboard.observe(Event::Press(a), &mut emit).unwrap();
        keyboard.cleanup(&mut emit).unwrap();
        assert_eq!(
            recorded[..6],
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_K, true),
                (KeyCode::KEY_K, false),
                (KeyCode::KEY_LEFTCTRL, false),
                (KeyCode::KEY_LEFTSHIFT, false)
            ]
        );
        assert_eq!(recorded[9].0, KeyCode::KEY_K);

        // Failed key-down remains a cleanup candidate, with no successful activation reported.
        let mut fail = |_, _| Err(io::Error::other("output failed"));
        assert!(keyboard.observe(Event::Press(a), &mut fail).is_err());
        assert!(!keyboard.assignments[0].active);
        let mut released = Vec::new();
        keyboard
            .cleanup(&mut |key, down| {
                released.push((key, down));
                Ok(())
            })
            .unwrap();
        assert_eq!(released, [(KeyCode::KEY_LEFTSHIFT, false)]);

        // Two holds share Ctrl until its last owner lifts.
        let mut keyboard = Keyboard::new(&[
            Mapping::new(a, "Ctrl", "hold").unwrap(),
            Mapping::new(b, "Ctrl+C", "hold").unwrap(),
        ])
        .unwrap();
        let mut recorded = Vec::new();
        let mut emit = |key, down| {
            recorded.push((key, down));
            Ok(())
        };
        for event in [
            Event::Press(a),
            Event::Press(b),
            Event::Release(b, ReleaseType::NoteOff),
            Event::Release(a, ReleaseType::NoteOff),
        ] {
            keyboard.observe(event, &mut emit).unwrap();
        }
        assert_eq!(
            recorded,
            [
                (KeyCode::KEY_LEFTCTRL, true),
                (KeyCode::KEY_C, true),
                (KeyCode::KEY_C, false),
                (KeyCode::KEY_LEFTCTRL, false)
            ]
        );

        // Cleanup attempts every key even if the first release fails, then permits a retry.
        keyboard
            .observe(Event::Press(b), &mut |_, _| Ok(()))
            .unwrap();
        let mut attempts = Vec::new();
        assert!(
            keyboard
                .cleanup(&mut |key, _| {
                    attempts.push(key);
                    if key == KeyCode::KEY_C {
                        Err(io::Error::other("release failed"))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
        assert_eq!(attempts, [KeyCode::KEY_C, KeyCode::KEY_LEFTCTRL]);
        assert_eq!(keyboard.held, HashSet::from([KeyCode::KEY_C]));
        keyboard.cleanup(&mut |_, _| Ok(())).unwrap();
    }

    #[test]
    fn focused_paste_taps_shortcut_then_enter_keeps_held_keys_and_stops_when_paused() {
        let hold = Control::from_note(10, 36).unwrap();
        let mut keyboard = Keyboard::new(&[Mapping::new(hold, "Ctrl", "hold").unwrap()]).unwrap();
        let mut sent = Vec::new();
        let mut emit = |key, down| {
            sent.push((key, down));
            Ok(())
        };
        keyboard.tap(&paste_keys(false), &mut emit).unwrap();
        keyboard.tap(&[KeyCode::KEY_ENTER], &mut emit).unwrap();
        use KeyCode as K;
        assert_eq!(
            sent,
            [
                (K::KEY_LEFTCTRL, true),
                (K::KEY_V, true),
                (K::KEY_V, false),
                (K::KEY_LEFTCTRL, false),
                (K::KEY_ENTER, true),
                (K::KEY_ENTER, false),
            ]
        );
        sent.clear();
        let mut emit = |key, down| {
            sent.push((key, down));
            Ok(())
        };
        keyboard.observe(Event::Press(hold), &mut emit).unwrap();
        keyboard.tap(&paste_keys(true), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        assert!(keyboard.tap(&[KeyCode::KEY_ENTER], &mut emit).is_err());
        assert_eq!(
            sent,
            [
                (K::KEY_LEFTCTRL, true), // the hold mapping's Ctrl stays down through the paste
                (K::KEY_LEFTSHIFT, true),
                (K::KEY_V, true),
                (K::KEY_V, false),
                (K::KEY_LEFTSHIFT, false),
                (K::KEY_LEFTCTRL, false), // released by pause; paused tap sends nothing
            ]
        );
        assert!(keyboard.held.is_empty());
    }

    #[test]
    fn risky_controls_fire_after_one_second_and_cancel_on_release_pause_or_clear() {
        let hold = Control::from_note(10, 36).unwrap();
        let trigger = Control::from_note(10, 37).unwrap();
        let mut shift = Mapping::new(hold, "Shift", "hold").unwrap();
        shift.risky = true;
        let mut command = Mapping::new_action(
            trigger,
            crate::mappings::Input::Pad,
            crate::actions::Action::OutputMute,
        )
        .unwrap();
        command.risky = true;
        let mut keyboard = Keyboard::new(&[shift, command]).unwrap();
        let mut sent = Vec::new();
        let mut emit = |key, down| {
            sent.push((key, down));
            Ok(())
        };
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);
        // Hold-shortcut: nothing until 1 s, then key down; key up on release.
        assert_eq!(
            keyboard
                .observe_at(Event::Press(hold), t, &mut emit)
                .unwrap(),
            None
        );
        assert!(keyboard.fire_due(ms(999), &mut emit).unwrap().is_empty());
        assert_eq!(
            keyboard.fire_due(ms(1000), &mut emit).unwrap(),
            [(hold, true)]
        );
        assert!(keyboard.fire_due(ms(5000), &mut emit).unwrap().is_empty()); // fires once
        let release = Event::Release(hold, crate::ReleaseType::NoteOff);
        assert_eq!(
            keyboard.observe_at(release, ms(1500), &mut emit).unwrap(),
            Some((hold, false))
        );
        // Trigger: early release sends nothing, even later.
        keyboard
            .observe_at(Event::Press(trigger), ms(2000), &mut emit)
            .unwrap();
        let early = Event::Release(trigger, crate::ReleaseType::NoteOff);
        assert_eq!(
            keyboard.observe_at(early, ms(2500), &mut emit).unwrap(),
            None
        );
        assert!(keyboard.fire_due(ms(9000), &mut emit).unwrap().is_empty());
        // Trigger held: fires once at 1 s.
        keyboard
            .observe_at(Event::Press(trigger), ms(3000), &mut emit)
            .unwrap();
        assert_eq!(
            keyboard.fire_due(ms(4000), &mut emit).unwrap(),
            [(trigger, true)]
        );
        let late = Event::Release(trigger, crate::ReleaseType::NoteOff);
        assert_eq!(
            keyboard.observe_at(late, ms(4100), &mut emit).unwrap(),
            None
        );
        // Clear (program change) and pause both drop an armed press.
        keyboard
            .observe_at(Event::Press(trigger), ms(5000), &mut emit)
            .unwrap();
        keyboard.clear(&mut emit).unwrap();
        assert!(keyboard.fire_due(ms(9000), &mut emit).unwrap().is_empty());
        keyboard
            .observe_at(
                Event::Release(trigger, crate::ReleaseType::NoteOff),
                ms(5100),
                &mut emit,
            )
            .unwrap();
        keyboard
            .observe_at(Event::Press(trigger), ms(6000), &mut emit)
            .unwrap();
        keyboard.pause(&mut emit).unwrap();
        assert!(keyboard.fire_due(ms(9000), &mut emit).unwrap().is_empty());
        assert!(keyboard.armed.is_empty());
        assert_eq!(
            sent,
            [
                (KeyCode::KEY_LEFTSHIFT, true),
                (KeyCode::KEY_LEFTSHIFT, false)
            ]
        );
    }

    /// Ticket 03: the first press after start, resume, reconnect or program change fires;
    /// a pad held through the transition fires only when pressed again.
    #[test]
    fn pads_fire_on_first_press_after_every_transition_but_held_pads_wait() {
        let trigger = Control::from_note(10, 37).unwrap();
        let hold = Control::from_note(10, 36).unwrap();
        let maps = [
            Mapping::new(hold, "Shift", "hold").unwrap(),
            Mapping::new_action(
                trigger,
                crate::mappings::Input::Pad,
                crate::actions::Action::OutputMute,
            )
            .unwrap(),
        ];
        let mut sent = Vec::new();
        let mut emit = |key, down| {
            sent.push((key, down));
            Ok(())
        };
        let release = |pad| Event::Release(pad, ReleaseType::NoteOff);
        let mut keyboard = Keyboard::new(&maps).unwrap(); // start
        for transition in ["start", "resume", "reconnect", "program change"] {
            match transition {
                "resume" => keyboard.pause(&mut emit).unwrap(),
                "reconnect" => {
                    keyboard.pause(&mut emit).unwrap();
                    let mut reconnected = true;
                    keyboard
                        .prepare_connection(&maps, &HashSet::new(), &mut reconnected)
                        .unwrap();
                }
                "program change" => {
                    keyboard.pause(&mut emit).unwrap();
                    keyboard.replace(&maps).unwrap();
                }
                _ => {}
            }
            keyboard.resume();
            assert_eq!(
                keyboard.observe(Event::Press(trigger), &mut emit).unwrap(),
                Some((trigger, true)),
                "{transition}"
            );
            keyboard.observe(release(trigger), &mut emit).unwrap();
        }
        // Held through a pause: its release sends nothing; the next press fires.
        keyboard.observe(Event::Press(hold), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        keyboard.resume();
        assert_eq!(keyboard.observe(release(hold), &mut emit).unwrap(), None);
        // Down when a fresh connection is prepared: the same.
        keyboard.pause(&mut emit).unwrap();
        let mut reconnected = true;
        keyboard
            .prepare_connection(&maps, &HashSet::from([hold]), &mut reconnected)
            .unwrap();
        keyboard.resume();
        assert_eq!(
            keyboard.observe(Event::Press(hold), &mut emit).unwrap(),
            None
        );
        assert_eq!(keyboard.observe(release(hold), &mut emit).unwrap(), None);
        assert_eq!(
            keyboard.observe(Event::Press(hold), &mut emit).unwrap(),
            Some((hold, true))
        );
        keyboard.pause(&mut emit).unwrap();
        use KeyCode as K;
        assert_eq!(
            sent,
            [
                (K::KEY_LEFTSHIFT, true),
                (K::KEY_LEFTSHIFT, false),
                (K::KEY_LEFTSHIFT, true),
                (K::KEY_LEFTSHIFT, false)
            ]
        );
    }

    /// v7 ticket 96: a one-tap control sends its shortcut once per press, however long it's
    /// held; a key a held assignment shares stays held; a transition leaves nothing held.
    #[test]
    fn one_tap_sends_once_per_press_and_never_holds_a_key() {
        use evdev::KeyCode as K;
        let tap = Control::from_note(10, 36).unwrap();
        let ctrl = Control::from_note(10, 37).unwrap();
        let maps = [
            Mapping::new(tap, "Ctrl+T", "tap").unwrap(),
            Mapping::new(ctrl, "Ctrl", "hold").unwrap(),
        ];
        let sent = std::cell::RefCell::new(Vec::new());
        let mut emit = |key, down| {
            sent.borrow_mut().push((key, down));
            Ok(())
        };
        let release = |pad| Event::Release(pad, ReleaseType::NoteOff);
        let mut keyboard = Keyboard::new(&maps).unwrap();
        keyboard.resume();
        // Pressed and held (a held pad sends nothing more), then released: one tap.
        assert_eq!(
            keyboard.observe(Event::Press(tap), &mut emit).unwrap(),
            Some((tap, true))
        );
        assert_eq!(
            keyboard
                .observe(Event::DuplicatePress(tap), &mut emit)
                .unwrap(),
            None
        );
        assert_eq!(keyboard.observe(release(tap), &mut emit).unwrap(), None);
        assert_eq!(
            *sent.borrow(),
            [
                (K::KEY_LEFTCTRL, true),
                (K::KEY_T, true),
                (K::KEY_T, false),
                (K::KEY_LEFTCTRL, false)
            ]
        );
        assert!(!keyboard.active(tap));
        // Ctrl held by another pad: the tap leaves it down.
        sent.borrow_mut().clear();
        keyboard.observe(Event::Press(ctrl), &mut emit).unwrap();
        keyboard.observe(Event::Press(tap), &mut emit).unwrap();
        keyboard.observe(release(tap), &mut emit).unwrap();
        assert_eq!(
            *sent.borrow(),
            [(K::KEY_LEFTCTRL, true), (K::KEY_T, true), (K::KEY_T, false)]
        );
        keyboard.observe(release(ctrl), &mut emit).unwrap();
        assert_eq!(sent.borrow().last(), Some(&(K::KEY_LEFTCTRL, false)));
        // Paused while pressed: nothing to release, and its release after resume sends nothing.
        sent.borrow_mut().clear();
        keyboard.observe(Event::Press(tap), &mut emit).unwrap();
        keyboard.pause(&mut emit).unwrap();
        keyboard.resume();
        assert_eq!(keyboard.observe(release(tap), &mut emit).unwrap(), None);
        assert_eq!(
            sent.borrow().len(),
            4,
            "one tap, nothing on pause or release: {:?}",
            sent.borrow()
        );
    }

    /// A one-tap whose second key-down fails leaves Ctrl for `pause` to release.
    #[test]
    fn failed_one_tap_releases_pressed_keys_on_pause() {
        use evdev::KeyCode as K;
        let tap = Control::from_note(10, 36).unwrap();
        let maps = [Mapping::new(tap, "Ctrl+T", "tap").unwrap()];
        let sent = std::cell::RefCell::new(Vec::new());
        let mut failing = |key, down| {
            if key == K::KEY_T {
                return Err(io::Error::other("uinput"));
            }
            sent.borrow_mut().push((key, down));
            Ok(())
        };
        let mut keyboard = Keyboard::new(&maps).unwrap();
        keyboard.resume();
        assert!(keyboard.observe(Event::Press(tap), &mut failing).is_err());
        let _ = keyboard.pause(&mut failing); // T's release fails too; Ctrl's must not be lost
        assert_eq!(
            *sent.borrow(),
            [(K::KEY_LEFTCTRL, true), (K::KEY_LEFTCTRL, false)]
        );
    }

    /// v7 ticket 96: LT on one tap: squeezed, held and eased off it taps once; it taps again
    /// only after dropping below its fire point.
    #[test]
    fn a_one_tap_trigger_taps_once_per_squeeze() {
        use crate::gamepad::{Analog, Change, Motion, analog};
        let lt = crate::gamepad::control(gilrs::Button::LeftTrigger2).unwrap();
        let maps = [Mapping::new(lt, "F6", "tap").unwrap()];
        let tuning = crate::mappings::Tuning::default(); // fires at 30%
        let mut keyboard = Keyboard::new(&maps).unwrap();
        keyboard.resume();
        let (mut down, mut pad) = (HashSet::new(), Analog::default());
        let mut downs = 0;
        for value in [0.1, 0.5, 0.8, 1.0, 0.8, 0.27, 0.6, 0.1, 0.5, 0.0] {
            for change in analog(
                &mut down,
                &mut pad,
                Motion::Trigger(gilrs::Button::LeftTrigger2, value),
                &tuning,
            ) {
                let Change::Event(event) = change else {
                    continue;
                };
                keyboard
                    .observe(event, &mut |key, pressed| {
                        if pressed {
                            assert_eq!(key, evdev::KeyCode::KEY_F6);
                            downs += 1;
                        }
                        Ok(())
                    })
                    .unwrap();
            }
        }
        assert_eq!(
            downs, 2,
            "one per squeeze; hovering near the fire point taps nothing more"
        );
    }

    /// v7 ticket 99: a risky control let go before one second is reported once, so the
    /// session can say "hold 1 s"; a full hold, or a pause during the hold, reports nothing.
    #[test]
    fn a_risky_control_let_go_early_is_reported_once() {
        let pad = Control::from_note(10, 37).unwrap();
        let mut command = Mapping::new_action(
            pad,
            crate::mappings::Input::Pad,
            crate::actions::Action::OutputMute,
        )
        .unwrap();
        command.risky = true;
        let mut keyboard = Keyboard::new(&[command]).unwrap();
        keyboard.resume();
        let mut emit = |_, _| Ok(());
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);
        let release = || Event::Release(pad, ReleaseType::NoteOff);
        keyboard
            .observe_at(Event::Press(pad), t, &mut emit)
            .unwrap();
        assert_eq!(
            keyboard.observe_at(release(), ms(400), &mut emit).unwrap(),
            None
        );
        assert_eq!(keyboard.take_early(), Some(pad));
        assert_eq!(keyboard.take_early(), None, "once");
        // Held a full second: it fires, and letting go is no "early" release.
        keyboard
            .observe_at(Event::Press(pad), ms(2000), &mut emit)
            .unwrap();
        assert_eq!(
            keyboard.fire_due(ms(3000), &mut emit).unwrap(),
            [(pad, true)]
        );
        keyboard.observe_at(release(), ms(3100), &mut emit).unwrap();
        assert_eq!(keyboard.take_early(), None);
        // Paused during the hold: cancelled quietly.
        keyboard
            .observe_at(Event::Press(pad), ms(4000), &mut emit)
            .unwrap();
        keyboard.pause(&mut emit).unwrap();
        keyboard.resume();
        keyboard.observe_at(release(), ms(4200), &mut emit).unwrap();
        assert_eq!(keyboard.take_early(), None);
    }

    /// v7 ticket 113: the keyboard LED is written only when it changes, and put back to what
    /// it showed before on restore and on drop; a stand-in records every write.
    /// v7 ticket 128: a Razer keyboard's M light comes from OpenRazer's `macro_led_state`:
    /// read first, lit with `1`, put back on restore. No driver: the chosen M light is
    /// unavailable with the reason, and nothing is written.
    /// v7 ticket 128: the RGB backlight through fake OpenRazer files: each effect is written
    /// in the driver's format, the brightness read first goes back with the resting effect on
    /// restore (and on drop), nothing is sent before `show` or twice, an unplugged keyboard is
    /// dropped, and no driver is named as unavailable.
    #[test]
    fn razer_rgb_shows_each_effect_and_restores_the_resting_effect_and_brightness() {
        use crate::status::Rgb;
        let dir = std::env::temp_dir().join(format!("vibeconsole-rgb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let error = RgbKeyboards::open(&dir, Rgb::SPECTRUM)
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("unavailable: OpenRazer's razerkbd driver"),
            "{error}"
        );
        let board = |name: &str| {
            let kbd = dir.join(name);
            std::fs::create_dir_all(&kbd).unwrap();
            std::fs::write(kbd.join("matrix_brightness"), "180\n").unwrap();
            std::fs::write(kbd.join("device_type"), format!("{name} board\n")).unwrap();
            for f in ["none", "spectrum", "wave", "static", "breath", "reactive"] {
                std::fs::write(kbd.join(format!("matrix_effect_{f}")), "").unwrap();
            }
            kbd
        };
        let one = board("0003:1532:021A.0001");
        let two = board("0003:1532:021A.0002");
        let read = |kbd: &Path, f: &str| std::fs::read(kbd.join(f)).unwrap();
        let mut rgb = RgbKeyboards::open(&dir, Rgb::Wave(None)).unwrap();
        assert_eq!(rgb.names().len(), 2);
        assert!(
            read(&one, "matrix_effect_static").is_empty(),
            "nothing before show"
        );
        rgb.show(Rgb::Static([255, 0, 0]), 255).unwrap();
        assert_eq!(read(&one, "matrix_effect_static"), [255, 0, 0]);
        assert_eq!(read(&two, "matrix_brightness"), b"255");
        std::fs::write(one.join("matrix_effect_static"), "").unwrap();
        rgb.show(Rgb::Static([255, 0, 0]), 255).unwrap();
        assert!(
            read(&one, "matrix_effect_static").is_empty(),
            "not sent twice"
        );
        rgb.show(Rgb::Breath([0, 0, 255], Default::default()), 255)
            .unwrap();
        assert_eq!(read(&one, "matrix_effect_breath"), [0, 0, 255]);
        rgb.show(Rgb::Reactive([0, 255, 0], None), 100).unwrap();
        assert_eq!(read(&one, "matrix_effect_reactive"), [2, 0, 255, 0]);
        assert_eq!(read(&one, "matrix_brightness"), b"100");
        rgb.show(Rgb::SPECTRUM, 100).unwrap();
        assert_eq!(read(&one, "matrix_effect_spectrum"), b"1");
        rgb.show(Rgb::Off, 100).unwrap();
        assert_eq!(read(&one, "matrix_effect_none"), b"1");
        rgb.restore();
        assert_eq!(read(&one, "matrix_effect_wave"), b"1", "the resting effect");
        assert_eq!(
            read(&one, "matrix_brightness"),
            b"180",
            "the brightness from before"
        );
        // Restored already: a second restore (or the drop) sends nothing.
        std::fs::write(one.join("matrix_effect_wave"), "").unwrap();
        rgb.restore();
        assert!(read(&one, "matrix_effect_wave").is_empty());
        // One keyboard unplugged: the other keeps showing; quitting (drop) restores it.
        std::fs::remove_dir_all(&two).unwrap();
        rgb.show(Rgb::Static([255, 255, 255]), 50).unwrap();
        assert_eq!(rgb.names().len(), 1);
        assert_eq!(read(&one, "matrix_effect_static"), [255, 255, 255]);
        drop(rgb);
        assert_eq!(read(&one, "matrix_effect_wave"), b"1");
        assert_eq!(read(&one, "matrix_brightness"), b"180");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// v7.7 ticket 189: the Nommo and the Firefly under fake `razeraccessory` files: each is
    /// found by its model, written its own effect, never its brightness; the Nommo's missing
    /// reactive is named unavailable and nothing is written for it; the resting effect goes
    /// back on restore and on drop; a model that isn't there is named.
    #[test]
    fn razer_accessories_show_their_effects_skip_what_they_lack_and_rest() {
        use crate::status::Rgb;
        let dir = std::env::temp_dir().join(format!("vibeconsole-acc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let device = |id: &str, name: &str, effects: &[&str]| {
            let d = dir.join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("device_type"), format!("{name}\n")).unwrap();
            std::fs::write(d.join("matrix_brightness"), "0\n").unwrap();
            for f in effects {
                std::fs::write(d.join(format!("matrix_effect_{f}")), "").unwrap();
            }
            d
        };
        let all = ["none", "spectrum", "wave", "static", "breath", "reactive"];
        let nommo = device("0003:1532:0517.000B", "Razer Nommo Chroma", &all[..5]);
        let firefly = device("0003:1532:0C04.0001", "Razer Firefly V2", &all);
        let read = |d: &Path, f: &str| std::fs::read(d.join(f)).unwrap();
        let mut speakers =
            RgbKeyboards::open_accessory(&dir, Rgb::Wave(None), "Speakers RGB", "Nommo").unwrap();
        let mut pad =
            RgbKeyboards::open_accessory(&dir, Rgb::SPECTRUM, "Mousepad RGB", "Firefly").unwrap();
        assert_eq!(speakers.names(), ["Razer Nommo Chroma"]);
        assert_eq!(pad.names(), ["Razer Firefly V2"]);
        assert_eq!(
            speakers.abilities(),
            ["Razer Nommo Chroma: off, spectrum, wave, static, breath"]
        );
        speakers.show(Rgb::Static([255, 255, 255]), 90).unwrap();
        pad.show(Rgb::Breath([0, 0, 255], Default::default()), 90)
            .unwrap();
        assert_eq!(read(&nommo, "matrix_effect_static"), [255, 255, 255]);
        assert_eq!(read(&firefly, "matrix_effect_breath"), [0, 0, 255]);
        assert!(
            read(&firefly, "matrix_effect_static").is_empty(),
            "its own effect"
        );
        assert_eq!(
            read(&nommo, "matrix_brightness"),
            b"0\n",
            "brightness left alone"
        );
        assert_eq!(speakers.unavailable, None);
        speakers.show(Rgb::Reactive([255, 0, 0], None), 90).unwrap();
        assert_eq!(
            speakers.unavailable.take().as_deref(),
            Some("Reactive · Red unavailable on Razer Nommo Chroma: it has no reactive effect")
        );
        assert!(
            !nommo.join("matrix_effect_reactive").exists(),
            "never written"
        );
        pad.show(Rgb::Reactive([255, 0, 0], None), 90).unwrap();
        assert_eq!(read(&firefly, "matrix_effect_reactive"), [2, 255, 0, 0]);
        speakers.restore();
        assert_eq!(
            read(&nommo, "matrix_effect_wave"),
            b"1",
            "the resting effect"
        );
        drop(pad);
        assert_eq!(
            read(&firefly, "matrix_effect_spectrum"),
            b"1",
            "quit: resting"
        );
        assert_eq!(read(&firefly, "matrix_brightness"), b"0\n");
        let error = RgbKeyboards::open_accessory(&dir, Rgb::Off, "Mousepad RGB", "Goliathus")
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.starts_with("Mousepad RGB: unavailable: no Razer Goliathus"),
            "{error}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// v7.7 ticket 190: the G502 logo through a fake `logirgb`: a state is a `--preview`
    /// set with the colour, sent once; wave and reactive are named unavailable, not sent;
    /// restore (and drop) reads the active profile's stored effect and previews it; a
    /// missing or failing logirgb is named unavailable at open.
    #[test]
    fn g502_logo_previews_each_state_and_restores_the_stored_effect() {
        use crate::status::Rgb;
        use std::sync::{Arc, Mutex};
        const LIST: &str = "1. G502 HERO Gaming Mouse  [/dev/hidraw8, 046d:c08b]\n   zone 0 (primary): DPI LEDs, flash on DPI button press\n     profile 1: cycle period=8000ms intensity=100%\n     profile 2: static color=#ff0000\n   zone 1 (logo): G logo\n     profile 1: cycle period=8000ms intensity=100%\n     profile 2: breathe color=#00dcff period=8000ms form=default intensity=100%\n     - off\n";
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = calls.clone();
        let run: crate::gamepad::Runner = Box::new(move |args: &[&str]| {
            log.lock().unwrap().push(args.join(" "));
            Ok(match args[1] {
                "profiles" => "G502 HERO Gaming Mouse profiles: 1 on, 2 on (active)\n".into(),
                "list" => LIST.into(),
                _ => String::new(),
            })
        });
        let mut mouse = MouseLight::open(run).unwrap();
        mouse.show(Rgb::Static([255, 128, 0])).unwrap();
        mouse.show(Rgb::Static([255, 128, 0])).unwrap();
        mouse
            .show(Rgb::Breath([0, 0, 255], Default::default()))
            .unwrap();
        mouse.show(Rgb::SPECTRUM).unwrap();
        mouse.show(Rgb::Wave(None)).unwrap();
        assert_eq!(
            mouse.unavailable.take().as_deref(),
            Some(
                "Wave unavailable on the G502 logo: it shows off, static, breath with one colour and spectrum only"
            )
        );
        drop(mouse);
        assert_eq!(
            *calls.lock().unwrap(),
            [
                "logirgb profiles -d g502",
                "logirgb set -d g502 -z 1 static --color #ff8000 --preview",
                "logirgb set -d g502 -z 1 breathe --color #0000ff --preview",
                "logirgb set -d g502 -z 1 cycle --preview",
                "logirgb profiles -d g502",
                "logirgb list",
                "logirgb set -d g502 -z 1 breathe --color #00dcff --period 8000 --intensity 100 --preview",
            ]
        );
        let missing: crate::gamepad::Runner =
            Box::new(|_: &[&str]| Err("logirgb: No such file or directory (os error 2)".into()));
        let error = MouseLight::open(missing).err().unwrap().to_string();
        assert!(
            error.starts_with("Mouse light (Logitech G502): unavailable: logirgb: No such file"),
            "{error}"
        );
    }

    #[test]
    fn razer_m_light_is_read_lit_and_put_back_and_missing_is_named() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-razer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let kbd = dir.join("0003:1532:021A.0001");
        std::fs::create_dir_all(&kbd).unwrap();
        std::fs::write(kbd.join("macro_led_state"), "0").unwrap();
        std::fs::write(kbd.join("game_led_state"), "0").unwrap();
        std::fs::write(
            kbd.join("device_type"),
            "Razer BlackWidow X Chroma Tournament Edition\n",
        )
        .unwrap();
        let mut lights = LockLights::open_with([false, false, false, true], &dir).unwrap();
        assert_eq!(
            lights.names(),
            ["Razer BlackWidow X Chroma Tournament Edition: M light"]
        );
        assert!(lights.missing.is_empty());
        let state = || std::fs::read_to_string(kbd.join("macro_led_state")).unwrap();
        lights.show(true).unwrap();
        assert_eq!(state(), "1");
        drop(lights);
        assert_eq!(state(), "0", "quit puts it back");
        assert_eq!(
            std::fs::read_to_string(kbd.join("game_led_state")).unwrap(),
            "0",
            "G is never touched"
        );
        let error = LockLights::open_with([false, false, false, true], &dir.join("none"))
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("M light: unavailable") && error.contains("razerkbd"),
            "{error}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lock_lights_write_changes_only_and_put_the_leds_back() {
        let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let led = |name: &str, was| {
            let writes = writes.clone();
            let name_owned = name.to_owned();
            Led::new(
                name,
                was,
                Box::new(move |on| {
                    writes.lock().unwrap().push((name_owned.clone(), on));
                    Ok(())
                }),
            )
        };
        let mut lights = LockLights::with(vec![led("a", false), led("b", true)]);
        assert_eq!(lights.names(), ["a", "b"]);
        lights.restore();
        assert!(
            writes.lock().unwrap().is_empty(),
            "nothing lit, nothing to put back"
        );
        lights.show(true).unwrap();
        lights.show(true).unwrap();
        assert_eq!(
            *writes.lock().unwrap(),
            [("a".to_string(), true), ("b".to_string(), true)]
        );
        lights.restore();
        assert_eq!(
            writes.lock().unwrap()[2..],
            [("a".to_string(), false), ("b".to_string(), true)]
        );
        writes.lock().unwrap().clear();
        lights.show(false).unwrap();
        drop(lights);
        assert_eq!(
            *writes.lock().unwrap(),
            [
                ("a".to_string(), false),
                ("b".to_string(), false),
                ("a".to_string(), false),
                ("b".to_string(), true)
            ],
            "drop puts each LED back"
        );
    }
}
