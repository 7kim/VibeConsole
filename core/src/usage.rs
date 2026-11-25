//! v7.7 ticket 164: what you use, counted per day, kept on this computer. Counts only:
//! never the text of a key or a prompt. One file per day, `YYYY-MM-DD.tsv`, rows of
//! `kind<TAB>key<TAB>count`.
use crate::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What is counted: presses per control, fired actions per family, keys typed for you,
/// prompts sent per prompt name, agents answered (key: seconds they waited).
pub const KINDS: [&str; 5] = ["press", "family", "keys", "prompt", "answered"];

/// One day's counts by (kind, key).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Day(pub BTreeMap<(String, String), u64>);

impl Day {
    /// Adds `n` to (kind, key). The key is a name (a control, a family, a prompt file),
    /// cut to one line of 64 characters.
    pub fn add(&mut self, kind: &'static str, key: &str, n: u64) {
        if n == 0 || !KINDS.contains(&kind) {
            return;
        }
        let key = key
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(64)
            .collect::<String>();
        *self.0.entry((kind.into(), key)).or_default() += n;
    }
    pub fn merge(&mut self, other: &Day) {
        for ((kind, key), n) in &other.0 {
            *self.0.entry((kind.clone(), key.clone())).or_default() += n;
        }
    }
    pub fn total(&self, kind: &str) -> u64 {
        self.of(kind).map(|(_, n)| n).sum()
    }
    pub fn of<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = (&'a str, u64)> + 'a {
        self.0
            .iter()
            .filter(move |((k, _), _)| k == kind)
            .map(|((_, key), n)| (key.as_str(), *n))
    }
    pub fn text(&self) -> String {
        self.0
            .iter()
            .map(|((kind, key), n)| format!("{kind}\t{key}\t{n}\n"))
            .collect()
    }
    /// A day file; any row that isn't `known kind<TAB>key<TAB>number` makes it corrupt.
    pub fn parse(text: &str) -> Result<Day> {
        let mut day = Day::default();
        for (number, line) in text.lines().enumerate() {
            let [kind, key, n] = line.split('\t').collect::<Vec<_>>()[..] else {
                return Err(format!("line {}: expected kind, key, count", number + 1).into());
            };
            let kind = KINDS
                .iter()
                .find(|k| **k == kind)
                .ok_or_else(|| format!("line {}: unknown kind {kind:?}", number + 1))?;
            day.add(kind, key, n.parse()?);
        }
        Ok(day)
    }
}

/// `$XDG_STATE_HOME/vibeconsole/usage` (default `~/.local/state`).
pub fn dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        Some(state) => PathBuf::from(state),
        None => {
            PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?).join(".local/state")
        }
    };
    if !base.is_absolute() {
        return Err("XDG_STATE_HOME must be absolute".into());
    }
    Ok(base.join("vibeconsole/usage"))
}

/// Days since 1970-01-01 from a civil date (H. Hinnant's algorithm).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
/// The civil date of a day number.
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}
/// 0 = Monday … 6 = Sunday (1970-01-01 was a Thursday).
pub fn weekday(days: i64) -> i64 {
    (days + 3).rem_euclid(7)
}
/// Today's day number in local time.
pub fn today() -> i64 {
    // SAFETY: time(NULL) and localtime_r fill a zeroed struct from a valid time_t.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    unsafe { libc::localtime_r(&now, &mut tm) };
    days_from_civil(
        i64::from(tm.tm_year) + 1900,
        (tm.tm_mon + 1) as u32,
        tm.tm_mday as u32,
    )
}
pub fn file(dir: &Path, day: i64) -> PathBuf {
    let (y, m, d) = civil(day);
    dir.join(format!("{y:04}-{m:02}-{d:02}.tsv"))
}

/// Adds `pending` to `day`'s file, atomically. A corrupt file is moved aside
/// (`.corrupt`) rather than overwritten, and the day starts again from `pending`.
pub fn flush(dir: &Path, day: i64, pending: &Day) -> Result<()> {
    if pending.0.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let path = file(dir, day);
    let mut total = match std::fs::read_to_string(&path) {
        Ok(text) => Day::parse(&text).unwrap_or_else(|_| {
            let _ = std::fs::rename(&path, path.with_extension("tsv.corrupt"));
            Day::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Day::default(),
        Err(e) => return Err(e.into()),
    };
    total.merge(pending);
    crate::actions::write_atomic(&path, &total.text())
}

/// The last `count` days up to `today`, oldest first, each with its counts (a missing day
/// is empty); a corrupt file is skipped and named in the notices.
pub fn load(dir: &Path, today: i64, count: i64) -> (Vec<(i64, Day)>, Vec<String>) {
    let mut notices = Vec::new();
    let days = (today - count + 1..=today)
        .map(|day| {
            let path = file(dir, day);
            let counts = match std::fs::read_to_string(&path) {
                Ok(text) => Day::parse(&text).unwrap_or_else(|e| {
                    notices.push(format!("usage: {} skipped ({e})", path.display()));
                    Day::default()
                }),
                Err(_) => Day::default(),
            };
            (day, counts)
        })
        .collect();
    (days, notices)
}

/// The heatmap's band for a day's presses: 0, 1–49, 50–149, 150–299, 300+.
pub fn band(presses: u64) -> usize {
    match presses {
        0 => 0,
        1..=49 => 1,
        50..=149 => 2,
        150..=299 => 3,
        _ => 4,
    }
}

/// (current, longest) runs of days with at least one press; the current run may end
/// yesterday (today isn't over).
pub fn streaks(presses: &[u64]) -> (usize, usize) {
    let mut longest = 0;
    let mut run = 0;
    for &n in presses {
        run = if n > 0 { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let current = match presses.split_last() {
        Some((0, rest)) => rest.iter().rev().take_while(|n| **n > 0).count(),
        _ => presses.iter().rev().take_while(|n| **n > 0).count(),
    };
    (current, longest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts add up across flushes into a day file that holds only names and numbers;
    /// dates, weekdays, bands and streaks come out right; a corrupt day is skipped.
    #[test]
    fn counts_flush_dates_bands_streaks_and_corrupt_days() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let day = days_from_civil(2026, 11, 10);
        assert_eq!(civil(day), (2026, 11, 10));
        assert_eq!(weekday(day), 1, "10 Nov 2026 is a Tuesday");
        assert_eq!(civil(days_from_civil(2024, 2, 29) + 1), (2024, 3, 1));
        let mut pending = Day::default();
        pending.add("press", "Bank A Pad 5", 2);
        pending.add("family", "prompt", 1);
        pending.add("prompt", "review", 1);
        pending.add("keys", "-", 3);
        pending.add("typed", "secret text", 1); // not a kind: never stored
        flush(&dir, day, &pending).unwrap();
        flush(&dir, day, &pending).unwrap();
        let text = std::fs::read_to_string(file(&dir, day)).unwrap();
        assert!(text.contains("press\tBank A Pad 5\t4\n"), "{text}");
        assert!(!text.contains("secret"));
        std::fs::write(file(&dir, day - 1), "press\tPad\tlots\n").unwrap();
        let (days, notices) = load(&dir, day, 3);
        assert_eq!(days.len(), 3);
        assert_eq!(days[2].1.total("press"), 4);
        assert_eq!(days[1].1, Day::default());
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(
            [0, 1, 49, 50, 149, 150, 299, 300].map(band),
            [0, 1, 1, 2, 2, 3, 3, 4]
        );
        assert_eq!(
            streaks(&[1, 1, 0, 2, 3, 4, 0]),
            (3, 3),
            "today not over yet"
        );
        assert_eq!(streaks(&[5, 5, 5, 0, 1]), (1, 3));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
