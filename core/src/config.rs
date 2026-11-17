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
pub const FILES: [&str; 1] = ["settings.json"];

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
        _ => return Err(format!("unknown config file {file}").into()),
    };
    Ok(serde_json::to_string_pretty(&value)? + "\n")
}

fn apply(file: &str, value: &Value, config: &mut Config) -> Result<()> {
    let at = Obj::top(value)?;
    match file {
        "settings.json" => apply_settings(&at, config),
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
}
