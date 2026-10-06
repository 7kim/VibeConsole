use crate::{DEVICE, Pad, Result};
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const HEADER: &str = "keyai-mappings-v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Behavior {
    Hold,
    Toggle,
}

impl fmt::Display for Behavior {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Hold => "hold",
            Self::Toggle => "toggle",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mapping {
    pub pad: Pad,
    pub keys: Vec<String>,
    pub behavior: Behavior,
}

impl Mapping {
    pub fn new(pad: Pad, shortcut: &str, behavior: &str) -> Result<Self> {
        if Pad::from_note(pad.channel, pad.note) != Some(pad) {
            return Err("unsupported pad identity; expected channel 10, notes 36-51".into());
        }
        let behavior = match behavior.trim().to_ascii_lowercase().as_str() {
            "hold" => Behavior::Hold,
            "toggle" => Behavior::Toggle,
            _ => return Err("behavior must be hold or toggle".into()),
        };
        Ok(Self {
            pad,
            keys: parse_shortcut(shortcut)?,
            behavior,
        })
    }
}

impl fmt::Display for Mapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Bank {} Pad {} | USB {DEVICE} | Note channel {} note {} | {} | {}",
            self.pad.bank,
            self.pad.number,
            self.pad.channel,
            self.pad.note,
            self.keys.join("+"),
            self.behavior
        )
    }
}

pub fn parse_shortcut(shortcut: &str) -> Result<Vec<String>> {
    let mut keys = Vec::new();
    for name in shortcut.split('+') {
        let name = name.trim().to_ascii_lowercase();
        // ponytail: named keys only; extend this subset when another physical key is needed.
        let alias = match name.as_str() {
            "control" | "leftctrl" => "Ctrl",
            "leftshift" => "Shift",
            "leftalt" => "Alt",
            "super" | "win" | "leftmeta" => "Meta",
            "altgr" => "RightAlt",
            "esc" => "Escape",
            "return" => "Enter",
            "del" => "Delete",
            "ins" => "Insert",
            _ => &name,
        };
        let named = [
            "Ctrl",
            "Shift",
            "Alt",
            "Meta",
            "RightCtrl",
            "RightShift",
            "RightAlt",
            "RightMeta",
            "Escape",
            "Enter",
            "Tab",
            "Space",
            "Backspace",
            "Delete",
            "Insert",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "Up",
            "Down",
            "Left",
            "Right",
            "CapsLock",
            "Minus",
            "Equal",
            "LeftBracket",
            "RightBracket",
            "Backslash",
            "Semicolon",
            "Apostrophe",
            "Grave",
            "Comma",
            "Period",
            "Slash",
        ];
        let key = if let Some(key) = named.iter().find(|key| key.eq_ignore_ascii_case(alias)) {
            (*key).to_owned()
        } else if (name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric())
            || (1..=12).any(|number| name == format!("f{number}"))
        {
            name.to_ascii_uppercase()
        } else {
            return Err(format!(
                "unknown or empty key name: {name:?}; see README.md for supported keys"
            )
            .into());
        };
        crate::keyboard::key_code(&key)?;
        if keys.contains(&key) {
            return Err(format!("duplicate key in shortcut: {key}").into());
        }
        keys.push(key);
    }
    keys.sort_by_key(|key| {
        !matches!(
            key.as_str(),
            "Ctrl"
                | "Shift"
                | "Alt"
                | "Meta"
                | "RightCtrl"
                | "RightShift"
                | "RightAlt"
                | "RightMeta"
        )
    });
    Ok(keys)
}

pub fn config_path() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => {
            PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unset; set XDG_CONFIG_HOME")?)
                .join(".config")
        }
    };
    if !base.is_absolute() {
        return Err("configuration directory must be absolute (XDG_CONFIG_HOME or HOME)".into());
    }
    Ok(base.join("keyai/mappings.tsv"))
}

fn parse(contents: &str) -> Result<Vec<Mapping>> {
    let mut lines = contents.lines();
    if lines.next() != Some(HEADER) {
        return Err("invalid configuration header; expected keyai-mappings-v1".into());
    }
    let mut mappings = Vec::new();
    let mut seen = HashSet::new();
    for (index, line) in lines.enumerate() {
        let result = (|| -> Result<Mapping> {
            let fields: Vec<_> = line.split('\t').collect();
            let [device, message, channel, note, behavior, shortcut] = fields.as_slice() else {
                return Err("expected six tab-separated fields".into());
            };
            if *device != DEVICE || *message != "note" {
                return Err("unsupported device or MIDI message type".into());
            }
            let pad = Pad::from_note(channel.parse()?, note.parse()?)
                .ok_or("unsupported pad channel or note")?;
            let mapping = Mapping::new(pad, shortcut, behavior)?;
            if !seen.insert(pad) {
                return Err("duplicate pad assignment".into());
            }
            Ok(mapping)
        })();
        mappings
            .push(result.map_err(|error| format!("configuration line {}: {error}", index + 2))?);
    }
    mappings.sort_by_key(|mapping| (mapping.pad.channel, mapping.pad.note));
    Ok(mappings)
}

pub fn load(path: &Path) -> Result<Vec<Mapping>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            parse(&contents).map_err(|error| format!("{}: {error}", path.display()).into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

pub fn save(path: &Path, mappings: &[Mapping]) -> Result<()> {
    let mut contents = format!("{HEADER}\n");
    for mapping in mappings {
        contents.push_str(&format!(
            "{DEVICE}\tnote\t{}\t{}\t{}\t{}\n",
            mapping.pad.channel,
            mapping.pad.note,
            mapping.behavior,
            mapping.keys.join("+")
        ));
        if Mapping::new(
            mapping.pad,
            &mapping.keys.join("+"),
            &mapping.behavior.to_string(),
        )? != *mapping
        {
            return Err("assignment contains noncanonical keys".into());
        }
    }
    parse(&contents)?;
    fs::create_dir_all(path.parent().ok_or("configuration path has no parent")?)?;
    // ponytail: one staging file; concurrent saves fail safely. Remove a stale file after an interrupted save.
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| {
            format!(
                "cannot create {}: {error}; saved mappings unchanged",
                temporary.display()
            )
        })?;
    let result = (|| -> Result<()> {
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_replacement_and_failures_preserve_saved_mappings() {
        let directory =
            std::env::temp_dir().join(format!("keyai-config-test-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("mappings.tsv");
        assert!(load(&path).unwrap().is_empty());
        let pad = Pad::from_note(10, 36).unwrap();
        let mapping = Mapping::new(pad, "shift", "hold").unwrap();
        save(&path, &[mapping.clone()]).unwrap();
        assert_eq!(load(&path).unwrap(), vec![mapping]);

        let replacement = Mapping::new(pad, "k + control + alt", "toggle").unwrap();
        assert_eq!(replacement.keys.join("+"), "Ctrl+Alt+K");
        let bank_b = Mapping::new(Pad::from_note(10, 44).unwrap(), "F12", "hold").unwrap();
        let valid = vec![replacement.clone(), bank_b];
        save(&path, &valid).unwrap();
        assert_eq!(load(&path).unwrap(), valid);
        let previous = fs::read(&path).unwrap();

        for shortcut in ["", "Ctrl++K", "Ctrl+Control", "F13", "shell:ls", "Ctrl+☃"] {
            assert!(Mapping::new(pad, shortcut, "hold").is_err());
        }
        assert!(Mapping::new(pad, "Shift", "macro").is_err());
        let mut invalid = replacement.clone();
        invalid.keys = vec!["Unknown".to_owned()];
        assert!(save(&path, &[invalid]).is_err());
        assert!(save(&path, &[replacement.clone(), replacement]).is_err());
        assert_eq!(fs::read(&path).unwrap(), previous);

        let temporary = path.with_extension("tmp");
        fs::write(&temporary, "occupied staging file").unwrap();
        assert!(save(&path, &[]).is_err());
        assert_eq!(fs::read(&path).unwrap(), previous);
        assert_eq!(
            fs::read_to_string(&temporary).unwrap(),
            "occupied staging file"
        );

        for malformed in [
            "",
            "keyai-mappings-v2\n",
            "keyai-mappings-v1\n09e8:1049\tnote\t10\t36\thold\tBogus\n",
            "keyai-mappings-v1\n09e8:1049\tcc\t10\t36\thold\tShift\n",
            "keyai-mappings-v1\nother\tnote\t10\t36\thold\tShift\n",
            "keyai-mappings-v1\n09e8:1049\tnote\t1\t36\thold\tShift\n",
            "keyai-mappings-v1\n09e8:1049\tnote\t10\t52\thold\tShift\n",
        ] {
            fs::write(&path, malformed).unwrap();
            assert!(load(&path).is_err());
        }
        fs::remove_dir_all(directory).unwrap();
    }
}
