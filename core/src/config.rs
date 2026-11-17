//! v7.7 tickets 194–198: the config as one JSON file per topic, beside where `mappings.tsv`
//! was. `Config` stays the one in-memory type: each file holds its part, read onto a base
//! and checked as it is applied, so an error names the file and the field.
use crate::Result;
use crate::mappings::{self, Config};
use serde_json::{Map, Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Every file's format version.
pub const VERSION: u64 = 1;
/// The files, in the order they are applied: each one's references (variables, MIDI
/// devices, idle settings) are in place before it is checked.
pub const FILES: [&str; 2] = ["settings.json", "lights.json"];

/// The config folder of the main config path (`…/vibeconsole/mappings.tsv`).
fn folder(path: &Path) -> Result<&Path> {
    path.parent()
        .ok_or_else(|| "configuration path has no parent".into())
}

/// Read the config: the files over the base, or, when none exists yet, the one-time move
/// from `mappings.tsv`.
pub fn load(path: &Path) -> Result<Config> {
    let dir = folder(path)?;
    let base = mappings::load_tsv(path)?;
    if !FILES.iter().any(|f| dir.join(f).exists()) {
        if path.exists() {
            move_from_tsv(dir, &base)?;
        }
        return Ok(base);
    }
    read_files(dir, base)
}

/// Apply each file that exists onto `config`, checking the whole config after each.
fn read_files(dir: &Path, mut config: Config) -> Result<Config> {
    for file in FILES {
        let path = dir.join(file);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", path.display()).into()),
        };
        let named = |e: Box<dyn std::error::Error>| format!("{}: {e}", path.display());
        let value: Value = serde_json::from_str(&text).map_err(|e| named(e.into()))?;
        apply(file, &value, &mut config).map_err(named)?;
        mappings::serialize_config(&config).map_err(named)?;
    }
    Ok(config)
}

/// The one-time move: write every file from the `mappings.tsv` config, load them back and
/// compare; any failure removes what it wrote. `mappings.tsv` is never touched.
fn move_from_tsv(dir: &Path, config: &Config) -> Result<()> {
    let mut written = Vec::new();
    let result = (|| -> Result<()> {
        for file in FILES {
            let path = dir.join(file);
            write_new(&path, text(file, config)?.as_bytes())?;
            written.push(path);
        }
        let back = read_files(dir, Config::default())?;
        for file in FILES {
            if text(file, &back)? != text(file, config)? {
                return Err(format!("{file} loads back different from mappings.tsv").into());
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        for path in written {
            let _ = fs::remove_file(path);
        }
        return Err(format!("config move from mappings.tsv: {e}; nothing was written").into());
    }
    Ok(())
}

/// Save the config: every file whose text changed, each atomically, all or none.
pub fn save(path: &Path, config: &Config) -> Result<()> {
    let dir = folder(path)?;
    let mut files = vec![(path.to_path_buf(), mappings::serialize_config(config)?)];
    for file in FILES {
        files.push((dir.join(file), text(file, config)?));
    }
    write_all(&files)
}

/// Write each changed file through a staging file, then rename them all; if a rename fails
/// the files already renamed get their old text back, so nothing is saved partially.
fn write_all(files: &[(PathBuf, String)]) -> Result<()> {
    let changed = files
        .iter()
        .filter(|(path, text)| fs::read(path).ok().as_deref() != Some(text.as_bytes()))
        .collect::<Vec<_>>();
    if let Some((first, _)) = changed.first() {
        fs::create_dir_all(first.parent().ok_or("configuration path has no parent")?)?;
    }
    let mut staged: Vec<PathBuf> = Vec::new();
    let unstage = |staged: &[PathBuf]| {
        for temporary in staged {
            let _ = fs::remove_file(temporary);
        }
    };
    for (path, text) in &changed {
        let temporary = path.with_extension("tmp");
        if let Err(e) = write_new(&temporary, text.as_bytes()) {
            unstage(&staged);
            return Err(format!(
                "cannot save {}: {e}; saved config unchanged",
                path.display()
            )
            .into());
        }
        staged.push(temporary);
    }
    let mut done: Vec<(&PathBuf, Option<Vec<u8>>)> = Vec::new();
    for ((path, text), temporary) in changed.iter().zip(&staged) {
        let old = fs::read(path).ok();
        // v7.7 ticket 163: the replaced `mappings.tsv` goes to history; best effort.
        if path.extension().is_some_and(|e| e == "tsv") {
            let _ = mappings::keep_history(path, text.as_bytes());
        }
        if let Err(e) = fs::rename(temporary, path) {
            unstage(&staged);
            for (path, old) in done.into_iter().rev() {
                let _ = match old {
                    Some(old) => fs::write(path, old),
                    None => fs::remove_file(path),
                };
            }
            return Err(format!(
                "cannot save {}: {e}; saved config unchanged",
                path.display()
            )
            .into());
        }
        done.push((path, old));
    }
    Ok(())
}

/// Create a new file (owner-only) with these bytes, synced; an existing file is an error.
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// A file's text: its part of the config, pretty JSON.
pub fn text(file: &str, config: &Config) -> Result<String> {
    let value = match file {
        "settings.json" => settings(config),
        "lights.json" => lights(config),
        _ => return Err(format!("unknown config file {file}").into()),
    };
    Ok(serde_json::to_string_pretty(&value)? + "\n")
}

fn apply(file: &str, value: &Value, config: &mut Config) -> Result<()> {
    let at = Obj::top(value)?;
    match file {
        "settings.json" => apply_settings(&at, config),
        "lights.json" => {
            let lights = Lights::read(&at)?;
            config.status_lights = lights.status_lights;
            config.light_patterns = lights.patterns;
            config.light_folders = lights.folders;
            config.light_other = lights.other;
            config.light_other_on = lights.other_on;
            config.device_lights = lights.devices;
            Ok(())
        }
        _ => Err(format!("unknown config file {file}").into()),
    }
}

/// A JSON object being read, with the path of its fields for error messages.
struct Obj<'a> {
    map: &'a Map<String, Value>,
    at: String,
}
impl<'a> Obj<'a> {
    fn top(value: &'a Value) -> Result<Self> {
        let obj = Self::new(value, "")?;
        match obj.map.get("version").and_then(Value::as_u64) {
            Some(VERSION) => Ok(obj),
            Some(n) if n > VERSION => Err(format!(
                "field `version`: {n} is from a newer VibeConsole (this one reads {VERSION})"
            )
            .into()),
            _ => Err(format!("field `version`: must be {VERSION}").into()),
        }
    }
    fn new(value: &'a Value, at: &str) -> Result<Self> {
        let map = value
            .as_object()
            .ok_or_else(|| format!("field `{}`: must be an object", at.trim_end_matches('.')))?;
        Ok(Self {
            map,
            at: at.to_owned(),
        })
    }
    fn name(&self, key: &str) -> String {
        format!("{}{key}", self.at)
    }
    fn err(&self, key: &str, what: &str) -> Box<dyn std::error::Error> {
        format!("field `{}`: {what}", self.name(key)).into()
    }
    /// The keys this object may have; any other is a typo and refused.
    fn only(&self, keys: &[&str]) -> Result<()> {
        match self.map.keys().find(|k| !keys.contains(&k.as_str())) {
            Some(k) => Err(self.err(k, "unknown field")),
            None => Ok(()),
        }
    }
    fn get(&self, key: &str) -> Result<&'a Value> {
        self.map.get(key).ok_or_else(|| self.err(key, "missing"))
    }
    fn bool(&self, key: &str) -> Result<bool> {
        self.get(key)?
            .as_bool()
            .ok_or_else(|| self.err(key, "must be true or false"))
    }
    fn str(&self, key: &str) -> Result<&'a str> {
        self.get(key)?
            .as_str()
            .ok_or_else(|| self.err(key, "must be a string"))
    }
    fn list(&self, key: &str) -> Result<&'a Vec<Value>> {
        self.get(key)?
            .as_array()
            .ok_or_else(|| self.err(key, "must be a list"))
    }
    /// Each item of a list of objects, with its path (`env[2].`).
    fn objects(&self, key: &str) -> Result<Vec<Obj<'a>>> {
        self.list(key)?
            .iter()
            .enumerate()
            .map(|(i, v)| Obj::new(v, &format!("{}[{i}].", self.name(key))))
            .collect()
    }
    /// An object field, with its path.
    fn obj(&self, key: &str) -> Result<Obj<'a>> {
        Obj::new(self.get(key)?, &format!("{}.", self.name(key)))
    }
    /// A whole number in the range of `T`.
    fn int<T: TryFrom<i64>>(&self, key: &str) -> Result<T> {
        self.get(key)?
            .as_i64()
            .and_then(|n| T::try_from(n).ok())
            .ok_or_else(|| self.err(key, "must be a whole number in range"))
    }
    /// One of a list's words, as its value.
    fn pick<T: Copy>(&self, key: &str, list: &[(&'static str, T)]) -> Result<T> {
        let word = self.str(key)?;
        list.iter()
            .find(|(w, _)| *w == word)
            .map(|(_, v)| *v)
            .ok_or_else(|| {
                let words = list.iter().map(|(w, _)| *w).collect::<Vec<_>>();
                self.err(
                    key,
                    &format!("must be one of {}, not {word:?}", words.join(", ")),
                )
            })
    }
    /// An object with exactly the four busy states as keys, in `status::BUSY` order.
    fn states(&self, key: &str) -> Result<Obj<'a>> {
        let states = self.obj(key)?;
        states.only(&crate::status::BUSY)?;
        for state in crate::status::BUSY {
            states.get(state)?;
        }
        Ok(states)
    }
    /// Run a check whose error is about this field.
    fn check<T>(&self, key: &str, result: Result<T>) -> Result<T> {
        result.map_err(|e| self.err(key, &e.to_string()))
    }
}

/// A list of `(name, value)` pairs as `[{"name": …, "value": …}]`.
fn pairs_json(list: &[(String, String)]) -> Value {
    list.iter()
        .map(|(name, value)| json!({"name": name, "value": value}))
        .collect()
}
fn pairs(at: &Obj, key: &str) -> Result<Vec<(String, String)>> {
    at.objects(key)?
        .iter()
        .map(|item| {
            item.only(&["name", "value"])?;
            Ok((item.str("name")?.to_owned(), item.str("value")?.to_owned()))
        })
        .collect()
}

// ---- settings.json (ticket 194): theme, Agents view, seen-7.7, AI usage sharing, Flow at
// start, environment.

fn settings(config: &Config) -> Value {
    let look = &config.look;
    json!({
        "version": VERSION,
        "theme": look.theme,
        "agents_view": if look.agent_cards { "gallery" } else { "table" },
        "share_usage": look.share_usage,
        "seen_7_7": look.seen_moved,
        "start_flow": config.start_flow,
        "env": pairs_json(&config.env),
    })
}
fn apply_settings(at: &Obj, config: &mut Config) -> Result<()> {
    at.only(&[
        "version",
        "theme",
        "agents_view",
        "share_usage",
        "seen_7_7",
        "start_flow",
        "env",
    ])?;
    let env = pairs(at, "env")?;
    at.check("env", mappings::validate_variables(&env, "startup"))?;
    let look = &mut config.look;
    // Lenient as in ticket 149: a theme or view this build doesn't know loads as its default
    // and says so, so a file from a newer build still opens.
    let mut unknown = Vec::new();
    let theme = at.str("theme")?;
    if mappings::THEMES.contains(&theme) {
        look.theme = theme.into();
    } else {
        look.theme = mappings::THEMES[0].into();
        unknown.push(format!("theme {theme:?} (using {})", mappings::THEMES[0]));
    }
    match at.str("agents_view")? {
        "table" => look.agent_cards = false,
        "gallery" => look.agent_cards = true,
        other => {
            look.agent_cards = false;
            unknown.push(format!("agents_view {other:?} (using table)"));
        }
    }
    look.share_usage = at.bool("share_usage")?;
    look.seen_moved = at.bool("seen_7_7")?;
    if !unknown.is_empty() {
        look.notice = Some(format!("settings.json: unknown {}", unknown.join(", ")));
    }
    config.start_flow = at.bool("start_flow")?;
    config.env = env;
    Ok(())
}

// ---- lights.json (ticket 195): status lights switch, the MPK's tempos and patterns, light
// folders and other, the Xbox, DualSense and keyboard LED patterns, and the RGB devices.

/// The light parts of the config; the shipped defaults are one of these.
pub struct Lights {
    pub status_lights: bool,
    pub patterns: crate::status::Patterns,
    pub folders: Vec<(String, i8)>,
    pub other: i8,
    pub other_on: bool,
    pub devices: crate::status::DevicePatterns,
}
/// The shipped light defaults (`core/defaults/lights.json`, built in): every default light
/// colour and effect; no code literal holds one.
pub fn shipped_lights() -> &'static Lights {
    static SHIPPED: std::sync::OnceLock<Lights> = std::sync::OnceLock::new();
    SHIPPED.get_or_init(|| {
        let value = serde_json::from_str(include_str!("../defaults/lights.json"))
            .expect("shipped lights.json is JSON (checked by a test)");
        Obj::top(&value)
            .and_then(|at| Lights::read(&at))
            .expect("shipped lights.json is valid (checked by a test)")
    })
}

fn lights(config: &Config) -> Value {
    use crate::status::{BUSY, KEY_LEDS, LIGHTS, RUMBLES, octave_word, rgb_word, tempo_word, word};
    let d = &config.device_lights;
    let p = &config.light_patterns;
    let by_state = |values: Vec<Value>| -> Value {
        BUSY.iter()
            .map(|s| s.to_string())
            .zip(values)
            .collect::<Map<_, _>>()
            .into()
    };
    let rgb_states = |effects: &[crate::status::Rgb; 4]| {
        by_state(effects.iter().map(|e| rgb_word(*e).into()).collect())
    };
    let leds = KEY_LEDS
        .iter()
        .zip(d.keyboard_leds)
        .filter(|(_, on)| *on)
        .map(|(name, _)| *name)
        .collect::<Vec<_>>();
    let [speakers, mousepad] = &d.accessories;
    let accessory = |a: &crate::status::AccessoryRgb| json!({"on": a.on, "rest": rgb_word(a.rest), "states": rgb_states(&a.effects)});
    json!({
        "version": VERSION,
        "status_lights": config.status_lights,
        "folders": config.light_folders.iter()
            .map(|(folder, octave)| json!({"folder": folder, "octave": octave}))
            .collect::<Vec<_>>(),
        "other": {"on": config.light_other_on, "octave": config.light_other},
        "mpk": {
            "on": d.on[0],
            "tempos": {"slow": p.slow, "fast": p.fast},
            "states": by_state(p.states.iter().map(|(t, o)| {
                json!({"tempo": tempo_word(*t), "octave": octave_word(*o)})
            }).collect()),
        },
        "xbox": {
            "on": d.on[1],
            "states": by_state(d.xbox.iter().zip(d.xbox_brightness).map(|((l, r), b)| {
                json!({"light": word(&LIGHTS, *l), "rumble": word(&RUMBLES, *r), "brightness": b})
            }).collect()),
        },
        "ps5": {
            "on": d.on[2],
            "states": by_state(d.ps5.iter().map(|(c, l, r)| {
                json!({"colour": c, "light": word(&LIGHTS, *l), "rumble": word(&RUMBLES, *r)})
            }).collect()),
        },
        "keyboard_leds": {
            "on": d.keyboard_on,
            "leds": leds,
            "states": by_state(d.keyboard.iter().map(|l| word(&LIGHTS, *l).into()).collect()),
        },
        "keyboard_rgb": {
            "on": d.rgb_on,
            "brightness": d.rgb_brightness,
            "rest": rgb_word(d.rgb_rest),
            "states": rgb_states(&d.rgb),
        },
        "speakers_rgb": accessory(speakers),
        "mousepad_rgb": accessory(mousepad),
        "mouse_rgb": {"on": d.mouse_on, "states": rgb_states(&d.mouse)},
    })
}

impl Lights {
    fn read(at: &Obj) -> Result<Self> {
        use crate::status::{BUSY, COLOURS, KEY_LEDS, LIGHTS, OCTAVES, RUMBLES, TEMPOS};
        at.only(&[
            "version",
            "status_lights",
            "folders",
            "other",
            "mpk",
            "xbox",
            "ps5",
            "keyboard_leds",
            "keyboard_rgb",
            "speakers_rgb",
            "mousepad_rgb",
            "mouse_rgb",
        ])?;
        let rgb = |at: &Obj, key: &str| at.check(key, crate::status::parse_rgb(at.str(key)?));
        let rgb_states = |at: &Obj| -> Result<[crate::status::Rgb; 4]> {
            let states = at.states("states")?;
            let mut out = [crate::status::Rgb::Off; 4];
            for (slot, state) in out.iter_mut().zip(BUSY) {
                *slot = rgb(&states, state)?;
            }
            Ok(out)
        };
        let folders = at
            .objects("folders")?
            .iter()
            .map(|f| {
                f.only(&["folder", "octave"])?;
                Ok((f.str("folder")?.to_owned(), f.int("octave")?))
            })
            .collect::<Result<Vec<_>>>()?;
        at.check("folders", mappings::validate_light_folders(&folders))?;
        let other = at.obj("other")?;
        other.only(&["on", "octave"])?;
        let other_octave = other.int("octave")?;
        other.check("octave", crate::status::valid_octave(other_octave))?;

        let mpk = at.obj("mpk")?;
        mpk.only(&["on", "tempos", "states"])?;
        let tempos = mpk.obj("tempos")?;
        tempos.only(&["slow", "fast"])?;
        let mpk_states = mpk.states("states")?;
        let mut states = [(crate::status::Tempo::Off, crate::status::Octave::Off); 4];
        for (slot, state) in states.iter_mut().zip(BUSY) {
            let s = mpk_states.obj(state)?;
            s.only(&["tempo", "octave"])?;
            *slot = (s.pick("tempo", &TEMPOS)?, s.pick("octave", &OCTAVES)?);
        }
        let patterns = crate::status::Patterns {
            slow: tempos.int("slow")?,
            fast: tempos.int("fast")?,
            states,
        };
        let patterns = mpk.check("tempos", patterns.validate())?;

        let xbox = at.obj("xbox")?;
        xbox.only(&["on", "states"])?;
        let xbox_states = xbox.states("states")?;
        let ps5 = at.obj("ps5")?;
        ps5.only(&["on", "states"])?;
        let ps5_states = ps5.states("states")?;
        let keys = at.obj("keyboard_leds")?;
        keys.only(&["on", "leds", "states"])?;
        let key_states = keys.states("states")?;
        let keyboard_rgb = at.obj("keyboard_rgb")?;
        keyboard_rgb.only(&["on", "brightness", "rest", "states"])?;
        let mouse = at.obj("mouse_rgb")?;
        mouse.only(&["on", "states"])?;

        let mut xbox_patterns = [(crate::status::Light::Off, crate::status::Rumble::Off); 4];
        let mut brightness = [0u8; 4];
        let mut ps5_patterns = [("", crate::status::Light::Off, crate::status::Rumble::Off); 4];
        let mut keyboard = [crate::status::Light::Off; 4];
        for (i, state) in BUSY.iter().enumerate() {
            let x = xbox_states.obj(state)?;
            x.only(&["light", "rumble", "brightness"])?;
            xbox_patterns[i] = (x.pick("light", &LIGHTS)?, x.pick("rumble", &RUMBLES)?);
            brightness[i] = x.int("brightness")?;
            let p = ps5_states.obj(state)?;
            p.only(&["colour", "light", "rumble"])?;
            let colours = COLOURS.map(|c| (c, c));
            ps5_patterns[i] = (
                p.pick("colour", &colours)?,
                p.pick("light", &LIGHTS)?,
                p.pick("rumble", &RUMBLES)?,
            );
            keyboard[i] = key_states.pick(state, &crate::status::KEYBOARD_LIGHTS)?;
        }
        let mut keyboard_leds = [false; 4];
        for (i, led) in keys.list("leds")?.iter().enumerate() {
            let name = led.as_str().unwrap_or_default();
            let index = KEY_LEDS.iter().position(|k| *k == name).ok_or_else(|| {
                keys.err(
                    &format!("leds[{i}]"),
                    &format!("must be one of {}", KEY_LEDS.join(", ")),
                )
            })?;
            keyboard_leds[index] = true;
        }
        let mut accessories = [crate::status::AccessoryRgb {
            on: false,
            rest: crate::status::Rgb::Off,
            effects: [crate::status::Rgb::Off; 4],
        }; 2];
        for (slot, key) in accessories.iter_mut().zip(["speakers_rgb", "mousepad_rgb"]) {
            let a = at.obj(key)?;
            a.only(&["on", "rest", "states"])?;
            *slot = crate::status::AccessoryRgb {
                on: a.bool("on")?,
                rest: rgb(&a, "rest")?,
                effects: rgb_states(&a)?,
            };
        }
        let devices = crate::status::DevicePatterns {
            on: [mpk.bool("on")?, xbox.bool("on")?, ps5.bool("on")?],
            xbox: xbox_patterns,
            ps5: ps5_patterns,
            keyboard_on: keys.bool("on")?,
            keyboard,
            keyboard_leds,
            xbox_brightness: brightness,
            rgb_on: keyboard_rgb.bool("on")?,
            rgb: rgb_states(&keyboard_rgb)?,
            rgb_rest: rgb(&keyboard_rgb, "rest")?,
            rgb_brightness: keyboard_rgb.int("brightness")?,
            accessories,
            mouse_on: mouse.bool("on")?,
            mouse: rgb_states(&mouse)?,
        };
        at.check("xbox", devices.validate())?;
        Ok(Self {
            status_lights: at.bool("status_lights")?,
            patterns,
            folders,
            other: other_octave,
            other_on: other.bool("on")?,
            devices,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vibeconsole-config-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A v15 file (from the mappings tests) with Flow off and one startup variable: older
    /// files have no theme or view lines, and later sections load as their defaults.
    pub(crate) const OLDER: &str = "vibeconsole-mappings-v15\nidle\t-\t-\t-\nprogram-idle\t1\t-\t-\t-\nprogram-idle\t2\t-\t-\t-\nprogram-idle\t3\t-\t-\t-\nprogram-idle\t4\t-\t-\t-\nprogram-idle\t5\t-\t-\t-\nprogram-idle\t6\t-\t-\t-\nprogram-idle\t7\t-\t-\t-\nstart-flow\toff\nagent-dir\t-\nenv\t1\nenv-var\tEDITOR\tvim\nvars\t0\nstatus-lights\ton\nlight-tempos\t50\t240\nlight-pattern\tneeds-decision\tfast\talternate\nlight-pattern\tneeds-input\tfast\tsteady\nlight-pattern\tthinking\tslow\toff\nlight-pattern\tfinished\toff\tblink\nlight-other\toff\t2\nlight-folders\t0\nagents\t2\nagent\tclaude\tclaude\tclaude\t-\t1\nagent\tcodex\tcodex\tcodex\t-\t-1\n0\tBank A Pad 1\tpad\t09e8:1049\tnote\t10\t36\t0\thold\tShift\t-\t-\t-\tshortcut\t-\t-\n";

    /// A config whose settings all differ from the defaults.
    fn changed() -> Config {
        let mut config = Config::default();
        config.look.theme = "Nord".into();
        config.look.agent_cards = true;
        config.look.share_usage = true;
        config.look.seen_moved = false;
        config.start_flow = false;
        config.env = vec![("EDITOR".into(), "hx".into()), ("A_1".into(), "x y".into())];
        config
    }

    /// Ticket 194: settings.json saves and loads back equal, and holds what it says.
    #[test]
    fn settings_json_round_trips() {
        let dir = folder("round-trip");
        let path = dir.join("mappings.tsv");
        let config = changed();
        mappings::save_config(&path, &config).unwrap();
        assert_eq!(mappings::load_config(&path).unwrap(), config);
        let saved: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(saved["version"], 1);
        assert_eq!(saved["theme"], "Nord");
        assert_eq!(saved["agents_view"], "gallery");
        assert_eq!(saved["env"][0]["name"], "EDITOR");
        // A change in settings.json alone is what loads.
        let edited = fs::read_to_string(dir.join("settings.json"))
            .unwrap()
            .replace("\"Nord\"", "\"Dracula\"");
        fs::write(dir.join("settings.json"), edited).unwrap();
        assert_eq!(mappings::load_config(&path).unwrap().look.theme, "Dracula");
    }

    /// Ticket 194: a v34 and an older `mappings.tsv` move into settings.json once, equal to
    /// what the TSV held, and the TSV stays byte for byte.
    #[test]
    fn settings_move_from_v34_and_older_tsv() {
        let v34 = mappings::serialize_config(&changed()).unwrap();
        let v15 = OLDER;
        for (name, tsv) in [("v34", v34.as_str()), ("v15", v15)] {
            let dir = folder(&format!("move-{name}"));
            let path = dir.join("mappings.tsv");
            fs::write(&path, tsv).unwrap();
            let from_tsv = mappings::load_tsv(&path).unwrap();
            let loaded = mappings::load_config(&path).unwrap();
            assert_eq!(loaded, from_tsv, "{name}");
            assert!(dir.join("settings.json").exists(), "{name}");
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                tsv,
                "{name}: TSV untouched"
            );
            // Loaded again, now from settings.json: the same.
            assert_eq!(mappings::load_config(&path).unwrap(), from_tsv, "{name}");
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                tsv,
                "{name}: TSV untouched"
            );
        }
        let dir = folder("move-v15-check");
        let path = dir.join("mappings.tsv");
        fs::write(&path, v15).unwrap();
        let loaded = mappings::load_config(&path).unwrap();
        assert!(!loaded.start_flow);
        assert_eq!(loaded.env, vec![("EDITOR".into(), "vim".into())]);
    }

    /// Ticket 194: an invalid settings.json is refused naming the file and the field, and
    /// stays as it was; a failed save keeps the last valid files; a failed move writes
    /// nothing.
    #[test]
    fn invalid_settings_json_is_refused_and_failed_saves_keep_the_last_files() {
        let dir = folder("invalid");
        let path = dir.join("mappings.tsv");
        mappings::save_config(&path, &changed()).unwrap();
        let file = dir.join("settings.json");
        let good = fs::read_to_string(&file).unwrap();
        for (bad, field) in [
            (
                good.replace("\"start_flow\": false", "\"start_flow\": \"no\""),
                "start_flow",
            ),
            (good.replace("\"EDITOR\"", "\"1BAD\""), "env"),
            (good.replace("\"seen_7_7\"", "\"seen_77\""), "seen_77"),
            (good.replace("\"version\": 1", "\"version\": 2"), "version"),
            (good.replace("\"start_flow\": false,", ""), "start_flow"),
        ] {
            fs::write(&file, &bad).unwrap();
            let error = mappings::load_config(&path).unwrap_err().to_string();
            assert!(
                error.contains("settings.json") && error.contains(&format!("`{field}")),
                "{field}: {error}"
            );
            assert_eq!(fs::read_to_string(&file).unwrap(), bad, "nothing rewritten");
        }
        fs::write(&file, "{ not json").unwrap();
        let error = mappings::load_config(&path).unwrap_err().to_string();
        assert!(error.contains("settings.json"), "{error}");
        fs::write(&file, &good).unwrap();

        // A save that can't be staged changes no file.
        let tsv = fs::read_to_string(&path).unwrap();
        fs::write(dir.join("settings.tmp"), "stale").unwrap();
        let mut next = changed();
        next.look.theme = "Dracula".into();
        next.start_flow = true;
        assert!(mappings::save_config(&path, &next).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), good);
        assert_eq!(fs::read_to_string(&path).unwrap(), tsv);
        assert!(!dir.join("mappings.tmp").exists(), "staged TSV removed");
        fs::remove_file(dir.join("settings.tmp")).unwrap();
        mappings::save_config(&path, &next).unwrap();
        assert_eq!(mappings::load_config(&path).unwrap(), next);

        // A move that can't write leaves no file behind and names the stage.
        let dir = folder("failed-move");
        let path = dir.join("mappings.tsv");
        fs::write(&path, mappings::serialize_config(&changed()).unwrap()).unwrap();
        let before = fs::read_dir(&dir).unwrap().count();
        let mut perms = fs::metadata(&dir).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o500);
        fs::set_permissions(&dir, perms.clone()).unwrap();
        let result = mappings::load_config(&path);
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        fs::set_permissions(&dir, perms).unwrap();
        let error = result.unwrap_err().to_string();
        assert!(error.contains("config move from mappings.tsv"), "{error}");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), before);
    }

    /// Lights that differ from the shipped ones in every part.
    fn lit() -> Config {
        use crate::status::{Light, Octave, Rgb, Rumble, Tempo};
        let mut config = Config::default();
        config.status_lights = false;
        config.light_folders = vec![("/home/me/work".into(), -3)];
        config.light_other = 4;
        config.light_other_on = true;
        config.light_patterns.slow = 33;
        config.light_patterns.states[2] = (Tempo::Fast, Octave::Alternate);
        let d = &mut config.device_lights;
        d.on = [false, true, false];
        d.xbox[0] = (Light::TempoSlow, Rumble::Long);
        d.xbox_brightness = [1, 2, 3, 50];
        d.ps5[3] = ("purple", Light::Steady, Rumble::Off);
        d.keyboard_on = true;
        d.keyboard = [Light::Steady; 4];
        d.keyboard_leds = [true, false, false, true];
        d.rgb_on = true;
        d.rgb = [
            Rgb::Wave,
            Rgb::Reactive("blue"),
            Rgb::Breath("white"),
            Rgb::Off,
        ];
        d.rgb_rest = Rgb::Static("purple");
        d.rgb_brightness = 128;
        d.accessories[0].on = true;
        d.accessories[1].rest = Rgb::Reactive("white");
        d.mouse_on = true;
        d.mouse[3] = Rgb::Static("white");
        config
    }

    /// Ticket 195: lights.json round-trips every light part, moves from a v34 and an older
    /// `mappings.tsv` (leaving it byte for byte), and a broken one is refused by file and
    /// field without being rewritten.
    #[test]
    fn lights_json_round_trips_moves_and_refuses() {
        let dir = folder("lights");
        let path = dir.join("mappings.tsv");
        mappings::save_config(&path, &lit()).unwrap();
        assert_eq!(mappings::load_config(&path).unwrap(), lit());
        let file = dir.join("lights.json");
        let good = fs::read_to_string(&file).unwrap();
        assert!(good.contains("\"reactive-blue\""), "{good}");
        for (bad, field) in [
            (
                good.replace("\"reactive-blue\"", "\"reactive-pink\""),
                "keyboard_rgb.states.needs-input",
            ),
            (
                good.replace("\"brightness\": 50", "\"brightness\": 51"),
                "xbox",
            ),
            (
                good.replace("\"brightness\": 128", "\"brightness\": 300"),
                "keyboard_rgb.brightness",
            ),
            (good.replace("\"slow\": 33", "\"slow\": 0"), "mpk.tempos"),
            (good.replace("\"octave\": -3", "\"octave\": 7"), "folders"),
            (good.replace("\"finished\"", "\"done\""), "mpk.states.done"),
            (
                good.replace("\"tempo\": \"fast\"", "\"tempo\": \"quick\""),
                "mpk.states.needs-decision.tempo",
            ),
        ] {
            fs::write(&file, &bad).unwrap();
            let error = mappings::load_config(&path).unwrap_err().to_string();
            assert!(
                error.contains("lights.json") && error.contains(&format!("`{field}")),
                "{field}: {error}"
            );
            assert_eq!(fs::read_to_string(&file).unwrap(), bad);
        }

        let v34 = mappings::serialize_config(&lit()).unwrap();
        for (name, tsv) in [("v34", v34.as_str()), ("v15", OLDER)] {
            let dir = folder(&format!("lights-move-{name}"));
            let path = dir.join("mappings.tsv");
            fs::write(&path, tsv).unwrap();
            let from_tsv = mappings::load_tsv(&path).unwrap();
            assert_eq!(mappings::load_config(&path).unwrap(), from_tsv, "{name}");
            assert!(dir.join("lights.json").exists());
            assert_eq!(mappings::load_config(&path).unwrap(), from_tsv, "{name}");
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                tsv,
                "{name}: TSV untouched"
            );
        }
    }

    /// Ticket 195: the shipped light defaults parse and validate, and are what a fresh
    /// config has.
    #[test]
    fn shipped_light_defaults_parse_and_validate() {
        let value: Value = serde_json::from_str(include_str!("../defaults/lights.json")).unwrap();
        let lights = Lights::read(&Obj::top(&value).unwrap()).unwrap();
        let config = Config::default();
        assert_eq!(config.device_lights, lights.devices);
        assert_eq!(config.light_patterns, lights.patterns);
        mappings::serialize_config(&config).unwrap();
        assert_eq!(
            text("lights.json", &config).unwrap(),
            include_str!("../defaults/lights.json"),
            "the shipped file is in the saved format"
        );
    }
}
