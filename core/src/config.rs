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
/// v7.7 ticket 209: lights.json's own version: 2 holds the keyboard RGB brightness per
/// state; a version 1 file's single brightness loads into all four. Ticket 212: 3 holds
/// `rgb_in_step`; older files load it on (the shipped default). Ticket 237: 4 holds
/// `rgb_step_period` (ms); older files load 4000.
pub const LIGHTS_VERSION: u64 = 4;
/// The files, in the order they are applied: each one's references (variables, MIDI
/// devices, idle settings) are in place before it is checked.
pub const FILES: [&str; 5] = [
    "settings.json",
    "devices.json",
    "lights.json",
    "mappings.json",
    "agents.json",
];

/// The config folder of the main config path (`…/vibeconsole/mappings.tsv`).
fn folder(path: &Path) -> Result<&Path> {
    path.parent()
        .ok_or_else(|| "configuration path has no parent".into())
}

/// Read the config: the files over the shipped defaults, or, when none exists yet, the
/// one-time move from `mappings.tsv`. After the move `mappings.tsv` is only the backup it was
/// moved from (ticket 197): never read or written again.
pub fn load(path: &Path) -> Result<Config> {
    let dir = folder(path)?;
    if !FILES.iter().any(|f| dir.join(f).exists()) && path.exists() {
        let config = mappings::load_tsv(path)?;
        move_from_tsv(dir, &config)?;
        return Ok(config);
    }
    read_files(dir, Config::default())
}
/// The same config, reading only: a `mappings.tsv` not moved yet is read as it is and left
/// for the session (which holds the owner lock) to move. For hooks and read-only commands.
pub fn peek(path: &Path) -> Result<Config> {
    let dir = folder(path)?;
    match FILES.iter().any(|f| dir.join(f).exists()) {
        true => read_files(dir, Config::default()),
        false => mappings::load_tsv(path),
    }
}

/// Apply each file that exists onto `config`, checking the whole config after each.
fn read_files(dir: &Path, mut config: Config) -> Result<Config> {
    for file in FILES {
        read_file(dir, file, &mut config)?;
    }
    Ok(config)
}
/// Apply one file onto `config` and check the whole config; false when it doesn't exist.
fn read_file(dir: &Path, file: &str, config: &mut Config) -> Result<bool> {
    let path = dir.join(file);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", path.display()).into()),
    };
    let named = |e: Box<dyn std::error::Error>| format!("{}: {e}", path.display());
    let value: Value = serde_json::from_str(&text).map_err(|e| named(e.into()))?;
    apply(file, &value, config).map_err(named)?;
    mappings::serialize_config(config).map_err(named)?;
    Ok(true)
}

/// v7.7 ticket 197: for `doctor`: each config file and whether it loaded, then the mapping
/// count. Only reads: a `mappings.tsv` not moved yet is left for the next start.
pub fn report(path: &Path) -> Vec<String> {
    let Ok(dir) = folder(path) else {
        return vec!["Config: the configuration path has no folder".into()];
    };
    let count =
        |config: &Config| format!("Configuration: {} valid mappings", config.mappings.len());
    if !FILES.iter().any(|f| dir.join(f).exists()) {
        return match path.exists() {
            false => vec!["Config: no files yet; the shipped defaults are in use".into()],
            true => vec![
                format!(
                    "Config: {} not moved yet; VibeConsole moves it into {} at its next start",
                    path.display(),
                    FILES.join(", ")
                ),
                match mappings::load_tsv(path) {
                    Ok(config) => count(&config),
                    Err(e) => format!("Configuration: {e}"),
                },
            ],
        };
    }
    let mut lines = Vec::new();
    let mut config = Config::default();
    let mut refused = false;
    for file in FILES {
        let state = if refused {
            "not checked (an earlier file was refused)".to_owned()
        } else {
            match read_file(dir, file, &mut config) {
                Ok(true) => "loaded".to_owned(),
                Ok(false) => "missing: its shipped defaults are in use".to_owned(),
                Err(e) => {
                    refused = true;
                    format!("refused: {e}")
                }
            }
        };
        lines.push(format!("Config {file}: {state}"));
    }
    if path.exists() {
        lines.push("Config mappings.tsv: the backup it was moved from; no longer read".into());
    }
    if !refused {
        lines.push(count(&config));
    }
    lines
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

/// Save the config: every file whose text changed, each atomically, all or none. The files
/// it replaced go to history as one step (ticket 197).
pub fn save(path: &Path, config: &Config) -> Result<()> {
    let dir = folder(path)?;
    // The whole config is checked with every rule before any file is written.
    mappings::serialize_config(config)?;
    let mut files = Vec::new();
    for file in FILES {
        files.push((dir.join(file), text(file, config)?));
    }
    write_all(&files, Some(&mappings::history_dir(path)))
}

/// Write each changed file through a staging file, then rename them all; if a rename fails
/// the files already renamed get their old text back, so nothing is saved partially.
fn write_all(files: &[(PathBuf, String)], history: Option<&Path>) -> Result<()> {
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
        // `mappings.tsv` stages as `mappings.tmp` (as before); a JSON file as `<name>.json.tmp`.
        let temporary = match path.extension().is_some_and(|e| e == "json") {
            true => path.with_extension("json.tmp"),
            false => path.with_extension("tmp"),
        };
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
    for ((path, _), temporary) in changed.iter().zip(&staged) {
        let old = fs::read(path).ok();
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
    // v7.7 ticket 163: what this save replaced goes to history; best effort, never blocks.
    if let Some(dir) = history.filter(|_| !done.is_empty()) {
        let _ = keep_history(dir, &done);
    }
    Ok(())
}

/// One history step: the text each changed file had before. A file that didn't exist yet
/// has nothing to put back, so a first save leaves no step.
fn keep_history(dir: &Path, replaced: &[(&PathBuf, Option<Vec<u8>>)]) -> Result<()> {
    let files = replaced
        .iter()
        .filter_map(|(path, old)| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            Some((name, json!(String::from_utf8_lossy(old.as_ref()?))))
        })
        .collect::<Map<_, _>>();
    if files.is_empty() {
        return Ok(());
    }
    let snapshot = json!({"version": VERSION, "files": files});
    fs::create_dir_all(dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    write_new(
        &dir.join(format!("{stamp:020}.json")),
        serde_json::to_string_pretty(&snapshot)?.as_bytes(),
    )?;
    for (old, _) in mappings::history_in(dir).iter().skip(mappings::HISTORY) {
        fs::remove_file(old)?;
    }
    Ok(())
}

/// The config before the save that wrote this history entry, from the config after it: a
/// JSON entry holds the files that save replaced; an old `.tsv` entry the whole config.
pub fn before(entry: &Path, after: &Config) -> Result<Config> {
    let raw = fs::read_to_string(entry).map_err(|e| format!("history: {e}"))?;
    if entry.extension().is_some_and(|e| e == "tsv") {
        return mappings::parse_config(&raw).map_err(|e| format!("history: {e}").into());
    }
    let mut config = after.clone();
    let snapshot: Value = serde_json::from_str(&raw).map_err(|e| format!("history: {e}"))?;
    let top = Obj::top(&snapshot).map_err(|e| format!("history: {e}"))?;
    top.only(&["version", "files"])?;
    let files = top.obj("files")?;
    files.only(&FILES)?;
    // In load order, so each file's references are in place.
    for file in FILES.iter().filter(|f| files.map.contains_key(**f)) {
        let value =
            serde_json::from_str(files.str(file)?).map_err(|e| format!("history: {file}: {e}"))?;
        apply(file, &value, &mut config).map_err(|e| format!("history: {file}: {e}"))?;
    }
    mappings::serialize_config(&config).map_err(|e| format!("history: {e}"))?;
    Ok(config)
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
        "devices.json" => devices(config),
        "mappings.json" => mappings_json(config)?,
        "agents.json" => agents(config),
        _ => return Err(format!("unknown config file {file}").into()),
    };
    Ok(serde_json::to_string_pretty(&value)? + "\n")
}

fn apply(file: &str, value: &Value, config: &mut Config) -> Result<()> {
    let at = if file == "lights.json" {
        Obj::top_of(value, LIGHTS_VERSION)?
    } else {
        Obj::top(value)?
    };
    match file {
        "settings.json" => apply_settings(&at, config),
        "devices.json" => apply_devices(&at, config),
        "mappings.json" => apply_mappings(&at, config),
        "agents.json" => apply_agents(&at, config),
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
        Self::top_of(value, VERSION)
    }
    /// A file whose versions 1 to `newest` all load (older ones with safe defaults).
    fn top_of(value: &'a Value, newest: u64) -> Result<Self> {
        let obj = Self::new(value, "")?;
        match obj.map.get("version").and_then(Value::as_u64) {
            Some(n) if (1..=newest).contains(&n) => Ok(obj),
            Some(n) if n > newest => Err(format!(
                "field `version`: {n} is from a newer VibeConsole (this one reads {newest})"
            )
            .into()),
            _ if newest == 1 => Err("field `version`: must be 1".into()),
            _ => Err(format!("field `version`: must be 1 to {newest}").into()),
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

/// v7.7 ticket 198: a preset file: every config file's JSON in one file. Presets hold the
/// whole setup as before; loading one still merges only what `apply_preset` takes.
pub fn preset_text(config: &Config) -> Result<String> {
    mappings::serialize_config(config)?;
    let mut files = Map::new();
    for file in FILES {
        files.insert(file.into(), serde_json::from_str(&text(file, config)?)?);
    }
    let preset = json!({"version": VERSION, "files": files});
    Ok(serde_json::to_string_pretty(&preset)? + "\n")
}
/// A preset file read back: its files over the shipped defaults, checked like a load.
pub fn from_preset(text: &str) -> Result<Config> {
    let value: Value = serde_json::from_str(text)?;
    let top = Obj::top(&value)?;
    top.only(&["version", "files"])?;
    let files = top.obj("files")?;
    files.only(&FILES)?;
    let mut config = Config::default();
    for file in FILES {
        if let Some(part) = files.map.get(file) {
            let named = |e: Box<dyn std::error::Error>| format!("{file}: {e}");
            apply(file, part, &mut config).map_err(named)?;
            mappings::serialize_config(&config).map_err(named)?;
        }
    }
    Ok(config)
}
/// Save a preset atomically (never into history: a preset isn't the config).
pub fn save_preset(path: &Path, config: &Config) -> Result<()> {
    write_all(&[(path.to_path_buf(), preset_text(config)?)], None)
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

// ---- devices.json (ticket 196): MIDI devices and the active one, idle settings and each
// program's, the Xbox programs' names, stick tuning and the headset.

fn idle_json(idle: crate::feedback::Settings) -> Value {
    json!({"octave": idle.octave, "tempo": idle.tempo, "arp": idle.arp})
}
fn idle(at: &Obj, key: &str) -> Result<crate::feedback::Settings> {
    let idle = at.obj(key)?;
    idle.only(&["octave", "tempo", "arp"])?;
    let optional = |field: &str| -> Result<Option<&Value>> {
        let value = idle.get(field)?;
        Ok((!value.is_null()).then_some(value))
    };
    let settings = crate::feedback::Settings {
        octave: optional("octave")?
            .map(|_| idle.int("octave"))
            .transpose()?,
        tempo: optional("tempo")?.map(|_| idle.int("tempo")).transpose()?,
        arp: optional("arp")?.map(|_| idle.bool("arp")).transpose()?,
    };
    at.check(key, settings.validate())
}
const MOUSE: [(&str, mappings::MouseStick); 3] = [
    ("off", mappings::MouseStick::Off),
    ("left", mappings::MouseStick::Left),
    ("right", mappings::MouseStick::Right),
];

fn devices(config: &Config) -> Value {
    let t = &config.tuning;
    let programs = (1..8u8)
        .map(|c| (c.to_string(), idle_json(config.idle_for(c))))
        .collect::<Map<_, _>>();
    json!({
        "version": VERSION,
        "midi_devices": config.midi_devices,
        "midi_active": config.midi_active,
        "idle": idle_json(config.idle),
        "program_idle": programs,
        "xbox_programs": config.xbox_names,
        "tuning": {
            "dead_zone": t.dead_zone,
            "trigger": t.trigger,
            "mouse_speed": t.mouse_speed,
            "rumble": t.rumble,
            "mouse": t.mouse.text(),
            "sticks": [t.sticks[0].text(), t.sticks[1].text()],
            "headset": t.headset,
        },
    })
}
fn apply_devices(at: &Obj, config: &mut Config) -> Result<()> {
    at.only(&[
        "version",
        "midi_devices",
        "midi_active",
        "idle",
        "program_idle",
        "xbox_programs",
        "tuning",
    ])?;
    let strings = |key: &str| -> Result<Vec<String>> {
        at.list(key)?
            .iter()
            .enumerate()
            .map(|(i, v)| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| at.err(&format!("{key}[{i}]"), "must be a string"))
            })
            .collect()
    };
    let midi_devices = strings("midi_devices")?;
    let midi_active = match at.get("midi_active")? {
        Value::Null => None,
        _ => Some(at.str("midi_active")?.to_owned()),
    };
    at.check(
        "midi_devices",
        mappings::validate_devices(&midi_devices, midi_active.as_ref()),
    )?;
    let idle_settings = idle(at, "idle")?;
    let programs = at.obj("program_idle")?;
    let keys = (1..8).map(|c: u8| c.to_string()).collect::<Vec<_>>();
    programs.only(&keys.iter().map(String::as_str).collect::<Vec<_>>())?;
    let mut program_idle = std::collections::BTreeMap::new();
    for (context, key) in (1..8u8).zip(&keys) {
        // As in the TSV: a program whose idle changes nothing follows the original idle.
        let settings = idle(&programs, key)?;
        if settings.enabled() {
            program_idle.insert(context, settings);
        }
    }
    let names = strings("xbox_programs")?;
    let xbox_names: [String; mappings::XBOX_PROGRAMS as usize] = names
        .try_into()
        .map_err(|_| at.err("xbox_programs", "needs 8 names (\"\" for Program N)"))?;
    at.check("xbox_programs", mappings::validate_xbox_names(&xbox_names))?;
    let t = at.obj("tuning")?;
    t.only(&[
        "dead_zone",
        "trigger",
        "mouse_speed",
        "rumble",
        "mouse",
        "sticks",
        "headset",
    ])?;
    let sticks = t.list("sticks")?;
    let stick = |i: usize| -> Result<mappings::StickMode> {
        let text = sticks
            .get(i)
            .and_then(Value::as_str)
            .ok_or_else(|| t.err("sticks", "must be two strings, left then right"))?;
        t.check(&format!("sticks[{i}]"), mappings::StickMode::parse(text))
    };
    if sticks.len() != 2 {
        return Err(t.err("sticks", "must be two strings, left then right"));
    }
    let tuning = mappings::Tuning {
        dead_zone: t.int("dead_zone")?,
        trigger: t.int("trigger")?,
        mouse_speed: t.int("mouse_speed")?,
        rumble: t.bool("rumble")?,
        mouse: t.pick("mouse", &MOUSE)?,
        sticks: [stick(0)?, stick(1)?],
        headset: t.bool("headset")?,
    };
    config.tuning = at.check("tuning", tuning.validate())?;
    config.midi_devices = midi_devices;
    config.midi_active = midi_active;
    config.idle = idle_settings;
    config.program_idle = program_idle;
    config.xbox_names = xbox_names;
    Ok(())
}

// ---- mappings.json (ticket 196): what each control does: mappings, profiles and the lock,
// favourites, saved sequences and user variables.

/// A mapping's fields, in its TSV record's column order; `device` follows from `context`
/// and isn't saved.
const COLUMNS: [&str; 16] = [
    "context",
    "label",
    "input",
    "device",
    "message",
    "channel",
    "id",
    "direction",
    "behavior",
    "keys",
    "octave",
    "tempo",
    "arp",
    "action",
    "risky",
    "status",
];
const NUMBERS: [&str; 5] = ["context", "channel", "id", "direction", "tempo"];
/// Columns whose `-` (nothing) is saved as null.
const OPTIONAL: [&str; 4] = ["keys", "octave", "tempo", "arp"];

fn mapping_json(mapping: &mappings::Mapping) -> Result<Value> {
    let line = mappings::mapping_line(mapping)?;
    let mut out = Map::new();
    for (column, field) in COLUMNS.iter().zip(line.trim_end_matches('\n').split('\t')) {
        let value = match *column {
            "device" => continue,
            "risky" | "status" => Value::Bool(field != "-"),
            c if OPTIONAL.contains(&c) && field == "-" => Value::Null,
            c if NUMBERS.contains(&c) => json!(field.parse::<i64>()?),
            _ => Value::String(field.into()),
        };
        out.insert(column.to_string(), value);
    }
    Ok(out.into())
}
/// A mapping object read back through the TSV record's own parser, so every rule stays.
fn mapping(at: &Obj, midi_devices: &[String]) -> Result<mappings::Mapping> {
    at.only(
        &COLUMNS
            .iter()
            .filter(|c| **c != "device")
            .copied()
            .collect::<Vec<_>>(),
    )?;
    let context: u8 = at.int("context")?;
    let mut fields = Vec::new();
    for column in COLUMNS {
        let field = match column {
            "device" => mappings::device_field(context).to_owned(),
            "risky" => (if at.bool(column)? { "risky" } else { "-" }).to_owned(),
            "status" => (if at.bool(column)? { "status" } else { "-" }).to_owned(),
            c if OPTIONAL.contains(&c) && at.get(c)?.is_null() => "-".to_owned(),
            c if NUMBERS.contains(&c) => at.int::<i64>(c)?.to_string(),
            c => {
                let text = at.str(c)?;
                if text.contains(['\t', '\n', '\r']) {
                    return Err(at.err(c, "must be one line without tabs"));
                }
                text.to_owned()
            }
        };
        fields.push(field);
    }
    let line = fields.join("\t");
    mappings::parse_mapping_line(&line, 34, midi_devices).map_err(|e| {
        let at = at.at.trim_end_matches('.');
        format!("field `{at}`: {e}").into()
    })
}

fn mappings_json(config: &Config) -> Result<Value> {
    let list = |m: &[mappings::Mapping]| m.iter().map(mapping_json).collect::<Result<Vec<_>>>();
    let profiles = config
        .profiles
        .iter()
        .map(|p| Ok(json!({"name": p.name, "apps": p.apps, "mappings": list(&p.mappings)?})))
        .collect::<Result<Vec<_>>>()?;
    let sequences = config
        .sequences
        .iter()
        .map(|(name, steps)| {
            let encoded = crate::actions::Action::PromptSequence(steps.clone()).encode();
            json!({"name": name, "sequence": encoded})
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "version": VERSION,
        "vars": pairs_json(&config.vars),
        "mappings": list(&config.mappings)?,
        "profiles": profiles,
        "lock": match &config.lock {
            None => Value::Null,
            Some(mappings::Lock::Default) => "Default".into(),
            Some(mappings::Lock::Profile(name)) => name.as_str().into(),
        },
        "favourites": config.favourites,
        "sequences": sequences,
    }))
}
fn apply_mappings(at: &Obj, config: &mut Config) -> Result<()> {
    at.only(&[
        "version",
        "vars",
        "mappings",
        "profiles",
        "lock",
        "favourites",
        "sequences",
    ])?;
    let vars = pairs(at, "vars")?;
    at.check("vars", mappings::validate_variables(&vars, "user"))?;
    let read = |at: &Obj, key: &str| -> Result<Vec<mappings::Mapping>> {
        at.objects(key)?
            .iter()
            .map(|m| mapping(m, &config.midi_devices))
            .collect()
    };
    let mut list = read(at, "mappings")?;
    list.sort_by_key(|m| (m.context, m.control));
    let mut profiles = Vec::new();
    for p in at.objects("profiles")? {
        p.only(&["name", "apps", "mappings"])?;
        let apps = p
            .list("apps")?
            .iter()
            .map(|a| {
                a.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| p.err("apps", "must be strings"))
            })
            .collect::<Result<Vec<_>>>()?;
        profiles.push(mappings::Profile {
            name: p.str("name")?.to_owned(),
            apps,
            mappings: read(&p, "mappings")?,
        });
    }
    at.check("profiles", mappings::validate_profiles(&profiles))?;
    let lock = match at.get("lock")? {
        Value::Null => None,
        _ => match at.str("lock")? {
            "Default" => Some(mappings::Lock::Default),
            name if profiles.iter().any(|p| p.name == name) => {
                Some(mappings::Lock::Profile(name.into()))
            }
            _ => return Err(at.err("lock", "the locked profile doesn't exist")),
        },
    };
    let favourites = at
        .list("favourites")?
        .iter()
        .map(|f| {
            f.as_str()
                .map(str::to_owned)
                .ok_or_else(|| at.err("favourites", "must be strings"))
        })
        .collect::<Result<Vec<_>>>()?;
    at.check("favourites", mappings::validate_favourites(&favourites))?;
    let mut sequences = Vec::new();
    for item in at.objects("sequences")? {
        item.only(&["name", "sequence"])?;
        let action = item.check(
            "sequence",
            crate::actions::Action::parse(item.str("sequence")?),
        )?;
        let crate::actions::Action::PromptSequence(steps) = action else {
            return Err(item.err("sequence", "a saved sequence holds prompt steps"));
        };
        sequences.push((item.str("name")?.to_owned(), steps));
    }
    at.check("sequences", mappings::validate_sequences(&sequences))?;
    config.vars = vars;
    config.mappings = list;
    config.profiles = profiles;
    config.lock = lock;
    config.favourites = favourites;
    config.sequences = sequences;
    Ok(())
}

// ---- agents.json (ticket 197): the agents, their default folder and the default agent.

fn agents(config: &Config) -> Value {
    let agents = config
        .agents
        .iter()
        .map(|a| {
            json!({
                "name": a.name,
                "launch": a.launch,
                "window": a.window,
                "dir": a.dir,
                "octave": a.octave,
                "host": a.host.text(),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "version": VERSION,
        "default_agent": config.look.default_agent,
        "agent_model": config.look.agent_model,
        "agent_dir": config.agent_dir,
        "agents": agents,
    })
}
fn apply_agents(at: &Obj, config: &mut Config) -> Result<()> {
    at.only(&[
        "version",
        "default_agent",
        "agent_model",
        "agent_dir",
        "agents",
    ])?;
    let optional = |at: &Obj, key: &str| -> Result<Option<String>> {
        match at.get(key)? {
            Value::Null => Ok(None),
            _ => Ok(Some(at.str(key)?.to_owned())),
        }
    };
    let agent_dir = optional(at, "agent_dir")?;
    if let Some(dir) = &agent_dir {
        at.check("agent_dir", mappings::directory_syntax(dir))?;
    }
    let mut agents = Vec::new();
    for a in at.objects("agents")? {
        a.only(&["name", "launch", "window", "dir", "octave", "host"])?;
        let mut agent = a.check(
            "name",
            mappings::Agent::new(a.str("name")?, a.str("launch")?, a.str("window")?),
        )?;
        agent.dir = optional(&a, "dir")?;
        if let Some(dir) = &agent.dir {
            a.check("dir", mappings::directory_syntax(dir))?;
        }
        agent.octave = a.int("octave")?;
        agent.host = a.check("host", mappings::Host::parse(a.str("host")?))?;
        agents.push(agent);
    }
    at.check("agents", mappings::validate_agents(&agents))?;
    // Lenient as in ticket 149: an agent this build doesn't know loads as the default and
    // says so; a model belongs to Ollama only.
    let (agent, model) = (at.str("default_agent")?, at.str("agent_model")?);
    let known = mappings::AGENT_NAMES.contains(&agent);
    if known && agent != "Ollama" && !model.is_empty() {
        return Err(at.err("agent_model", "only Ollama has a model; leave it \"\""));
    }
    let look = &mut config.look;
    if known && (agent != "Ollama" || mappings::valid_model(model)) {
        look.default_agent = agent.into();
        look.agent_model = model.into();
    } else {
        look.default_agent = mappings::AGENT_NAMES[0].into();
        look.agent_model = String::new();
        let unknown = format!(
            "agents.json: unknown default_agent {agent:?} with model {model:?} (using {})",
            mappings::AGENT_NAMES[0]
        );
        look.notice = Some(match look.notice.take() {
            Some(notice) => format!("{notice}; {unknown}"),
            None => unknown,
        });
    }
    config.agent_dir = agent_dir;
    config.agents = agents;
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
        Obj::top_of(&value, LIGHTS_VERSION)
            .and_then(|at| Lights::read(&at))
            .expect("shipped lights.json is valid (checked by a test)")
    })
}

/// The shipped defaults besides the lights (`core/defaults/`, built in).
const SHIPPED: [(&str, &str); 4] = [
    ("settings.json", include_str!("../defaults/settings.json")),
    ("devices.json", include_str!("../defaults/devices.json")),
    ("mappings.json", include_str!("../defaults/mappings.json")),
    ("agents.json", include_str!("../defaults/agents.json")),
];
/// v7.7 ticket 196: `Config::default()`: the shipped files over a blank config, so no
/// default value is a code literal.
pub fn shipped() -> &'static Config {
    static SHIPPED_CONFIG: std::sync::OnceLock<Config> = std::sync::OnceLock::new();
    SHIPPED_CONFIG.get_or_init(|| {
        let mut config = Config::blank();
        for (file, text) in SHIPPED {
            let value = serde_json::from_str(text).expect("shipped files are JSON (a test checks)");
            apply(file, &value, &mut config).expect("shipped files are valid (a test checks)");
        }
        config
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
    let set = |set: &crate::status::RgbSet| {
        json!({
            "rest": rgb_word(set.rest),
            "states": by_state(set.states.iter().map(|e| rgb_word(*e).into()).collect()),
        })
    };
    let leds = KEY_LEDS
        .iter()
        .zip(d.keyboard_leds)
        .filter(|(_, on)| *on)
        .map(|(name, _)| *name)
        .collect::<Vec<_>>();
    // v7.7 ticket 199: one shared set; each device's switch and its own set or null.
    let device = |i: usize| {
        let own = d.rgb_devices[i].own.as_ref().map(set);
        let mut out = json!({"on": d.rgb_devices[i].on, "own": own});
        if i == 0 {
            out["brightness"] = by_state(d.rgb_brightness.map(|b| json!(b)).to_vec());
        }
        out
    };
    json!({
        "version": LIGHTS_VERSION,
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
        "rgb_shared": set(&d.rgb_shared),
        "rgb_in_step": d.rgb_in_step,
        "rgb_step_period": d.rgb_step_period,
        "keyboard_rgb": device(0),
        "speakers_rgb": device(1),
        "mousepad_rgb": device(2),
        "mouse_rgb": device(3),
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
            "rgb_shared",
            "rgb_in_step",
            "rgb_step_period",
            "keyboard_rgb",
            "speakers_rgb",
            "mousepad_rgb",
            "mouse_rgb",
        ])?;
        let rgb = |at: &Obj, key: &str| at.check(key, crate::status::parse_rgb(at.str(key)?));
        // v7.7 ticket 199: a set: the resting effect and one effect per state.
        let set = |at: &Obj| -> Result<crate::status::RgbSet> {
            at.only(&["rest", "states"])?;
            let states = at.states("states")?;
            let mut out = [crate::status::Rgb::Off; 4];
            for (slot, state) in out.iter_mut().zip(BUSY) {
                *slot = rgb(&states, state)?;
            }
            Ok(crate::status::RgbSet {
                rest: rgb(at, "rest")?,
                states: out,
            })
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
        let rgb_shared = set(&at.obj("rgb_shared")?)?;
        let mut rgb_devices = [crate::status::RgbDevice {
            on: false,
            own: None,
        }; 4];
        let mut rgb_brightness = [0; 4];
        for (i, (slot, (key, ..))) in rgb_devices
            .iter_mut()
            .zip(crate::status::RGB_DEVICES)
            .enumerate()
        {
            let d = at.obj(key)?;
            d.only(if i == 0 {
                &["on", "own", "brightness"]
            } else {
                &["on", "own"]
            })?;
            if i == 0 {
                // v7.7 ticket 209: one per state; version 1 held one for all four.
                rgb_brightness = if at.int::<u64>("version")? == 1 {
                    [d.int("brightness")?; 4]
                } else {
                    let levels = d.states("brightness")?;
                    let mut out = [0; 4];
                    for (slot, state) in out.iter_mut().zip(BUSY) {
                        *slot = levels.int(state)?;
                    }
                    out
                };
            }
            let own = match d.get("own")? {
                Value::Null => None,
                _ => Some(set(&d.obj("own")?)?),
            };
            *slot = crate::status::RgbDevice {
                on: d.bool("on")?,
                own,
            };
        }

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
        let devices = crate::status::DevicePatterns {
            on: [mpk.bool("on")?, xbox.bool("on")?, ps5.bool("on")?],
            xbox: xbox_patterns,
            ps5: ps5_patterns,
            keyboard_on: keys.bool("on")?,
            keyboard,
            keyboard_leds,
            xbox_brightness: brightness,
            rgb_shared,
            rgb_devices,
            rgb_brightness,
            // v7.7 ticket 212: versions 1 and 2 had none: on, as shipped.
            rgb_in_step: at.int::<u64>("version")? < 3 || at.bool("rgb_in_step")?,
            // v7.7 ticket 237: 1000–20000 ms; versions 1–3 had none: the shipped 4000.
            rgb_step_period: if at.int::<u64>("version")? < 4 {
                crate::status::STEP_PERIOD_MS
            } else {
                let period: u16 = at.int("rgb_step_period")?;
                if !(1000..=20000).contains(&period) {
                    return Err(at.err("rgb_step_period", "must be 1000 to 20000 (ms)"));
                }
                period
            },
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
        fs::write(dir.join("settings.json.tmp"), "stale").unwrap();
        let mut next = changed();
        next.look.theme = "Dracula".into();
        next.start_flow = true;
        assert!(mappings::save_config(&path, &next).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), good);
        assert!(!path.exists(), "mappings.tsv is never written (ticket 197)");
        fs::remove_file(dir.join("settings.json.tmp")).unwrap();
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
        // Ticket 199: a shared set with own colours and settings, an override on the speakers
        // and the mouse, every device on.
        d.rgb_shared = crate::status::RgbSet {
            rest: Rgb::Static([18, 52, 86]),
            states: [
                Rgb::Wave(Some(2)),
                Rgb::Reactive([0, 0, 255], Some(4)),
                Rgb::Breath(
                    [255, 136, 0],
                    crate::status::Timing {
                        period: Some(3000),
                        form: Some(1),
                        intensity: Some(50),
                    },
                ),
                Rgb::BreathTwo([1, 2, 3], [250, 251, 252]),
            ],
        };
        d.rgb_brightness = [200, 120, 60, 20];
        d.rgb_in_step = false; // not the default, so the round trip shows it (ticket 212)
        d.rgb_step_period = 6000; // nor this (ticket 237)
        for device in &mut d.rgb_devices {
            device.on = true;
        }
        let mut own = d.rgb_shared;
        own.states[0] = Rgb::BreathRandom;
        d.rgb_devices[1].own = Some(own);
        own.states = [Rgb::Static([9, 9, 9]); 4];
        d.rgb_devices[3].own = Some(own);
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
        assert!(good.contains("\"reactive #0000ff speed=4\""), "{good}");
        for (bad, field) in [
            (
                good.replace(
                    "\"reactive #0000ff speed=4\"",
                    "\"reactive #0000ff speed=5\"",
                ),
                "rgb_shared.states.needs-input",
            ),
            (
                good.replace("\"brightness\": 50", "\"brightness\": 51"),
                "xbox",
            ),
            (
                good.replace("\"needs-decision\": 200", "\"needs-decision\": 300"),
                "keyboard_rgb.brightness.needs-decision",
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

        // v7.7 ticket 209: a version 1 file's one keyboard brightness loads into all four
        // states, and the next save writes version 2 with one per state.
        let v1 = good
            .replace("\"version\": 4", "\"version\": 1")
            .replace("  \"rgb_in_step\": false,\n", "")
            .replace("  \"rgb_step_period\": 6000,\n", "");
        let at = v1.find("\"brightness\": {").unwrap();
        let end = at + v1[at..].find('}').unwrap() + 1;
        let v1 = format!("{}\"brightness\": 77{}", &v1[..at], &v1[end..]);
        fs::write(&file, &v1).unwrap();
        let old = mappings::load_config(&path).unwrap();
        assert_eq!(old.device_lights.rgb_brightness, [77; 4]);
        mappings::save_config(&path, &old).unwrap();
        let saved = fs::read_to_string(&file).unwrap();
        assert!(
            saved.contains("\"version\": 4") && saved.contains("\"thinking\": 77"),
            "{saved}"
        );
        // v7.7 ticket 212: a version 2 file (no rgb_in_step) loads it on, and the next save
        // writes version 3 with it; a version 3 file without it is refused, naming the field.
        assert!(
            good.contains("  \"rgb_in_step\": false,\n"),
            "the test's own value"
        );
        let v2 = good
            .replace("\"version\": 4", "\"version\": 2")
            .replace("  \"rgb_in_step\": false,\n", "")
            .replace("  \"rgb_step_period\": 6000,\n", "");
        fs::write(&file, &v2).unwrap();
        let old = mappings::load_config(&path).unwrap();
        assert!(old.device_lights.rgb_in_step);
        mappings::save_config(&path, &old).unwrap();
        let saved = fs::read_to_string(&file).unwrap();
        assert!(
            saved.contains("\"version\": 4") && saved.contains("\"rgb_in_step\": true"),
            "{saved}"
        );
        // v7.7 ticket 237: a version 3 file (no rgb_step_period) loads 4000; a version 4
        // file with 25000, or without it, is refused naming the field.
        assert!(good.contains("  \"rgb_step_period\": 6000,\n"), "{good}");
        let v3 = good
            .replace("\"version\": 4", "\"version\": 3")
            .replace("  \"rgb_step_period\": 6000,\n", "");
        fs::write(&file, &v3).unwrap();
        assert_eq!(
            mappings::load_config(&path)
                .unwrap()
                .device_lights
                .rgb_step_period,
            4000
        );
        for bad in [
            good.replace("\"rgb_step_period\": 6000", "\"rgb_step_period\": 25000"),
            good.replace("  \"rgb_step_period\": 6000,\n", ""),
        ] {
            fs::write(&file, &bad).unwrap();
            let error = mappings::load_config(&path).unwrap_err().to_string();
            assert!(
                error.contains("lights.json") && error.contains("rgb_step_period"),
                "{error}"
            );
        }
        let missing = good.replace("  \"rgb_in_step\": false,\n", "");
        fs::write(&file, &missing).unwrap();
        let error = mappings::load_config(&path).unwrap_err().to_string();
        assert!(
            error.contains("lights.json") && error.contains("rgb_in_step"),
            "{error}"
        );
        fs::write(&file, &good).unwrap();

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

    /// Tickets 195–197: every shipped default parses, validates as a whole config, and is
    /// exactly what a fresh config saves.
    #[test]
    fn shipped_defaults_parse_and_validate() {
        let config = Config::default();
        mappings::serialize_config(&config).unwrap();
        for (file, shipped) in SHIPPED {
            let mut blank = Config::blank();
            apply(file, &serde_json::from_str(shipped).unwrap(), &mut blank).unwrap();
            assert_eq!(text(file, &config).unwrap(), shipped, "{file}");
        }
        let value: Value = serde_json::from_str(include_str!("../defaults/lights.json")).unwrap();
        let lights = Lights::read(&Obj::top_of(&value, LIGHTS_VERSION).unwrap()).unwrap();
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

    /// A config using every part of mappings.json and devices.json: an Akai pad with
    /// feedback, an Xbox button in program 2, another MIDI device, a profile and the lock, a
    /// favourite, a saved sequence a pad uses, a user variable a command folder uses, idle
    /// settings, Xbox names and stick tuning.
    fn controls() -> Config {
        use crate::actions::{Action, Target};
        use mappings::{Input, Lock, Mapping, Profile, StickMode};
        let mut config = Config::default();
        config.midi_devices = vec!["Launchkey".into()];
        config.midi_active = Some("Launchkey".into());
        config.idle.tempo = Some(90);
        config
            .set_idle(
                3,
                crate::feedback::Settings {
                    octave: Some(-2),
                    ..Default::default()
                },
            )
            .unwrap();
        config.xbox_names[1] = "Browser".into();
        config.tuning.dead_zone = 20;
        config.tuning.mouse = mappings::MouseStick::Right;
        config.tuning.sticks[0] = StickMode::Angle(vec![(50, 80), (330, 30)]);
        config.tuning.headset = true;
        config.vars = vec![("PROJ".into(), "/home/me/project".into())];
        let pad = |note| crate::Control::from_note(10, note).unwrap();
        let mut akai = Mapping::new(pad(36), "Ctrl+T", "tap").unwrap();
        akai.label = "Bank A Pad 1".into();
        akai.context = 1;
        akai.feedback.tempo = Some(120);
        let step = |file: &str| Action::Prompt {
            file: file.into(),
            target: Target::Agent("claude".into()),
            enter: true,
        };
        config.sequences = vec![("notes".into(), vec![step("plan"), step("notes")])];
        let seq = Mapping::new_action(pad(37), Input::Pad, Action::SavedSequence("notes".into()))
            .unwrap();
        let command = Mapping::new_action(
            pad(38),
            Input::Pad,
            Action::parse("command:dev:${PROJ}/web:make").unwrap(),
        )
        .unwrap();
        let mut xbox = Mapping::new(crate::Control::button(0), "Enter", "tap").unwrap();
        xbox.context = mappings::xbox_context(2, false);
        let mut other = Mapping::new(crate::Control::note(1, 60).unwrap(), "F5", "hold").unwrap();
        other.context = mappings::DEVICE_CONTEXT;
        other.label = "Launchkey note 60".into();
        config.mappings = vec![akai, seq, command, xbox, other];
        config.mappings.sort_by_key(|m| (m.context, m.control));
        let mut firefox = Mapping::new(pad(36), "Ctrl+W", "tap").unwrap();
        firefox.label = "Bank A Pad 1".into();
        config.profiles = vec![Profile {
            name: "Firefox".into(),
            apps: vec!["firefox.desktop".into()],
            mappings: vec![firefox],
        }];
        config.lock = Some(Lock::Profile("Firefox".into()));
        config.favourites = vec![mappings::profile_favourite_key("Firefox")];
        mappings::serialize_config(&config).unwrap();
        config
    }

    /// Ticket 196: mappings.json and devices.json round-trip, move from a v34 and an older
    /// `mappings.tsv` (byte for byte unchanged), and a broken one is refused by file and
    /// field without being rewritten.
    #[test]
    fn mappings_and_devices_json_round_trip_move_and_refuse() {
        let dir = folder("controls");
        let path = dir.join("mappings.tsv");
        mappings::save_config(&path, &controls()).unwrap();
        assert_eq!(mappings::load_config(&path).unwrap(), controls());
        let read = |file: &str| fs::read_to_string(dir.join(file)).unwrap();
        let (good_m, good_d) = (read("mappings.json"), read("devices.json"));
        assert!(good_m.contains("\"label\": \"Bank A Pad 1\""), "{good_m}");
        assert!(good_d.contains("\"Browser\""), "{good_d}");
        let cases = [
            (
                "mappings.json",
                good_m.replace("\"message\": \"note\"", "\"message\": \"drum\""),
                "mappings[0]",
            ),
            (
                "mappings.json",
                good_m.replace("\"lock\": \"Firefox\"", "\"lock\": \"Chrome\""),
                "lock",
            ),
            (
                "mappings.json",
                good_m.replace("\"Bank A Pad 1\"", "\"Bank\\tA\""),
                "label",
            ),
            (
                "mappings.json",
                good_m.replace("\"name\": \"notes\"", "\"name\": \"other\""),
                "mappings.json",
            ),
            (
                "mappings.json",
                good_m.replace("/home/me/project", "relative"),
                "${PROJ}/web",
            ),
            (
                "devices.json",
                good_d.replace("\"Browser\",", ""),
                "xbox_programs",
            ),
            (
                "devices.json",
                good_d.replace("\"dead_zone\": 20", "\"dead_zone\": 95"),
                "tuning",
            ),
            (
                "devices.json",
                good_d.replace(
                    "\"midi_active\": \"Launchkey\"",
                    "\"midi_active\": \"nano\"",
                ),
                "midi_devices",
            ),
        ];
        for (file, bad, field) in cases {
            fs::write(dir.join(file), &bad).unwrap();
            let error = mappings::load_config(&path).unwrap_err().to_string();
            assert!(
                error.contains(file) && error.contains(field),
                "{field}: {error}"
            );
            assert_eq!(read(file), bad);
            fs::write(
                dir.join(file),
                if file == "devices.json" {
                    &good_d
                } else {
                    &good_m
                },
            )
            .unwrap();
        }
        assert_eq!(mappings::load_config(&path).unwrap(), controls());

        let v34 = mappings::serialize_config(&controls()).unwrap();
        for (name, tsv) in [("v34", v34.as_str()), ("v15", OLDER)] {
            let dir = folder(&format!("controls-move-{name}"));
            let path = dir.join("mappings.tsv");
            fs::write(&path, tsv).unwrap();
            let from_tsv = mappings::load_tsv(&path).unwrap();
            assert_eq!(mappings::load_config(&path).unwrap(), from_tsv, "{name}");
            assert!(dir.join("mappings.json").exists() && dir.join("devices.json").exists());
            assert_eq!(mappings::load_config(&path).unwrap(), from_tsv, "{name}");
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                tsv,
                "{name}: TSV untouched"
            );
        }
    }

    /// Every part of agents.json changed: an agent with its own folder on Orca, one on
    /// herdr, the default folder, and Ollama with a model as the default agent.
    fn agents_changed() -> Config {
        let mut config = controls();
        let mut orca = mappings::Agent::new("review", "claude", "review").unwrap();
        orca.host = mappings::Host::Orca;
        orca.dir = Some("${PROJ}/api".into());
        orca.octave = 3;
        let mut herdr = mappings::Agent::new("helper", "codex", "helper").unwrap();
        herdr.host = mappings::Host::Herdr;
        herdr.octave = -2;
        config.agents.extend([orca, herdr]);
        config.agent_dir = Some("/home/me".into());
        config.look.default_agent = "Ollama".into();
        config.look.agent_model = "qwen2.5-coder:7b".into();
        mappings::serialize_config(&config).unwrap();
        config
    }

    /// Ticket 197: agents.json round-trips and refuses by file and field; a full v34
    /// `mappings.tsv` moves into the five files and loads back equal to it; after the move
    /// no save touches `mappings.tsv`, and doctor names each file.
    #[test]
    fn agents_json_and_the_full_move_retire_mappings_tsv() {
        let dir = folder("agents");
        let path = dir.join("mappings.tsv");
        mappings::save_config(&path, &agents_changed()).unwrap();
        assert_eq!(mappings::load_config(&path).unwrap(), agents_changed());
        let file = dir.join("agents.json");
        let good = fs::read_to_string(&file).unwrap();
        for (bad, field) in [
            (good.replace("\"orca\"", "\"t3\""), "agents[2].host"),
            (
                good.replace(
                    "\"helper\",\n      \"launch\"",
                    "\"review\",\n      \"launch\"",
                ),
                "agents",
            ),
            (good.replace("\"octave\": 3", "\"octave\": 9"), "agents"),
            (good.replace("\"/home/me\"", "\"\""), "agent_dir"),
            (good.replace("\"Ollama\"", "\"Codex\""), "agent_model"),
        ] {
            fs::write(&file, &bad).unwrap();
            let error = mappings::load_config(&path).unwrap_err().to_string();
            assert!(
                error.contains("agents.json") && error.contains(field),
                "{field}: {error}"
            );
            assert_eq!(fs::read_to_string(&file).unwrap(), bad);
        }
        // An agent this build doesn't know loads as the default and says so.
        fs::write(&file, good.replace("\"Ollama\"", "\"Grok\"")).unwrap();
        let lenient = mappings::load_config(&path).unwrap();
        assert_eq!(lenient.look.default_agent, mappings::AGENT_NAMES[0]);
        assert!(
            lenient
                .look
                .notice
                .unwrap()
                .contains("agents.json: unknown default_agent")
        );
        fs::write(&file, &good).unwrap();

        // The full move: every part from one v34 file, nothing lost, the TSV kept.
        let mut all = agents_changed();
        let (lights, settings) = (lit(), changed());
        all.status_lights = lights.status_lights;
        all.light_folders = lights.light_folders;
        all.light_other = lights.light_other;
        all.light_other_on = lights.light_other_on;
        all.light_patterns = lights.light_patterns;
        all.device_lights = lights.device_lights;
        // The TSV holds one keyboard brightness for every state (ticket 209).
        all.device_lights.rgb_brightness = [200; 4];
        // It has no Keep effects in step: on, as shipped (ticket 212).
        all.device_lights.rgb_in_step = true;
        all.device_lights.rgb_step_period = 4000; // nor an in-step period (ticket 237)
        all.look.theme = settings.look.theme;
        all.look.agent_cards = true;
        all.start_flow = false;
        all.env = settings.env;
        let tsv = mappings::serialize_config(&all).unwrap();
        let dir = folder("full-move");
        let path = dir.join("mappings.tsv");
        fs::write(&path, &tsv).unwrap();
        assert_eq!(mappings::load_config(&path).unwrap(), all);
        for file in FILES {
            assert!(dir.join(file).exists(), "{file}");
        }
        assert_eq!(
            mappings::load_config(&path).unwrap(),
            all,
            "loaded back from five files"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), tsv);
        // Saves after the move never touch it.
        let mut next = all.clone();
        next.look.theme = "Dracula".into();
        next.mappings.pop();
        next.device_lights.rgb_devices[0].on = false;
        mappings::save_config(&path, &next).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            tsv,
            "mappings.tsv is the backup"
        );
        assert_eq!(mappings::load_config(&path).unwrap(), next);
        let shown = report(&path).join("\n");
        for file in FILES {
            assert!(shown.contains(&format!("Config {file}: loaded")), "{shown}");
        }
        assert!(
            shown.contains("mappings.tsv: the backup it was moved from"),
            "{shown}"
        );
        fs::write(dir.join("lights.json"), "{}").unwrap();
        let shown = report(&path).join("\n");
        assert!(shown.contains("Config lights.json: refused: "), "{shown}");
        assert!(
            shown.contains("Config mappings.json: not checked"),
            "{shown}"
        );
    }

    /// Ticket 198: a preset is every config file's JSON in one file: it round-trips whole,
    /// a broken part is refused naming its file and field, and it never enters history.
    #[test]
    fn presets_hold_every_file_in_the_new_format() {
        let dir = folder("preset");
        let file = dir.join("Work.preset");
        let config = agents_changed();
        mappings::save_config(&file, &config).unwrap();
        assert_eq!(mappings::load_config(&file).unwrap(), config);
        let text = fs::read_to_string(&file).unwrap();
        for name in FILES {
            assert!(text.contains(&format!("\"{name}\"")), "{name}");
        }
        fs::write(&file, text.replace("\"headset\": true", "\"headset\": 1")).unwrap();
        let error = mappings::load_config(&file).unwrap_err().to_string();
        assert!(
            error.contains("devices.json: field `tuning.headset`"),
            "{error}"
        );
        assert!(!dir.join("history").exists(), "a preset isn't the config");
    }
}
