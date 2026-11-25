//! v7.8 ticket 26: Detect's link and speed test. Pure stats over times this computer stamped
//! (report gaps, and how long after the computer got a press VibeConsole handled it), their
//! 🟢/🟡/🔴 band, the USB link read from sysfs (read-only), and the last result per device and
//! connection kept in `speed-tests.tsv`. Nothing here talks to a device.

use std::path::Path;

/// Starting bands (spec): 🟢 below 10 ms, 🟡 10–20 ms, 🔴 above 20 ms or a dropped report.
/// ponytail: fixed until the user's first real measurements set the final bands.
pub const GREEN_BELOW_MS: f64 = 10.0;
pub const RED_ABOVE_MS: f64 = 20.0;
/// A delay outside 0..1 s means a clock changed (realtime clock set), not a slow press.
const MAX_DELAY_MS: f64 = 1000.0;
/// A gap this many times the median counts as a lost report (or a stick that stood still).
const GAP_FACTOR: f64 = 4.0;

pub const CLOCK_CHANGED: &str = "clock changed, not measured";

pub fn band(ms: f64, dropped: bool) -> &'static str {
    if dropped || ms > RED_ABOVE_MS {
        "🔴"
    } else if ms >= GREEN_BELOW_MS {
        "🟡"
    } else {
        "🟢"
    }
}

/// The link: reports while a stick or knob moves, from their stamps (microseconds).
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub per_second: f64,
    pub gap_ms: f64,
    pub jitter_ms: f64,
    /// The longest gap, when it looks like a lost report.
    pub dropped: Option<f64>,
}
impl Link {
    pub fn text(&self) -> String {
        let dropped = self
            .dropped
            .map(|ms| format!(" · a {ms:.0} ms gap (possible lost report or pause)"))
            .unwrap_or_default();
        format!(
            "{:.0} reports/s · one every {:.1} ms · jitter ±{:.1} ms{dropped} {}",
            self.per_second,
            self.gap_ms,
            self.jitter_ms,
            band(self.gap_ms, self.dropped.is_some())
        )
    }
}

/// Reports per second, average gap, jitter (standard deviation of the gaps) and the longest
/// gap when it is a dropout. Stamps going backwards are a clock change, never a result.
pub fn link(stamps_us: &[u64]) -> Result<Link, String> {
    if stamps_us.len() < 3 {
        return Err("too few reports: keep the control moving for the whole test".into());
    }
    if stamps_us.windows(2).any(|w| w[1] < w[0]) {
        return Err(CLOCK_CHANGED.into());
    }
    if stamps_us.first() == stamps_us.last() {
        return Err("the reports all have the same timestamp".into());
    }
    let gaps = stamps_us
        .windows(2)
        .map(|w| (w[1] - w[0]) as f64 / 1000.0)
        .collect::<Vec<_>>();
    let n = gaps.len() as f64;
    let gap_ms = gaps.iter().sum::<f64>() / n;
    let jitter_ms = (gaps.iter().map(|g| (g - gap_ms).powi(2)).sum::<f64>() / n).sqrt();
    let mut sorted = gaps.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    let longest = sorted[sorted.len() - 1];
    Ok(Link {
        per_second: 1000.0 / gap_ms,
        gap_ms,
        jitter_ms,
        dropped: (longest > median * GAP_FACTOR && longest > RED_ABOVE_MS).then_some(longest),
    })
}

/// One press: handled minus stamped, in ms; None when a clock changed in between.
pub fn delay_ms(handled_us: u64, stamped_us: u64) -> Option<f64> {
    let ms = (handled_us as f64 - stamped_us as f64) / 1000.0;
    (0.0..=MAX_DELAY_MS).contains(&ms).then_some(ms)
}

/// VibeConsole's own delay over the presses of a test: lowest, highest, average.
#[derive(Clone, Debug, PartialEq)]
pub struct Delays {
    pub low: f64,
    pub high: f64,
    pub average: f64,
}
impl Delays {
    pub fn text(&self, presses: usize) -> String {
        format!(
            "{presses} presses: handled after {:.0}–{:.0} ms, average {:.0} ms {}",
            self.low,
            self.high,
            self.average,
            band(self.average, false)
        )
    }
}
pub fn delays(ms: &[f64]) -> Option<Delays> {
    if ms.is_empty() {
        return None;
    }
    Some(Delays {
        low: ms.iter().copied().fold(f64::INFINITY, f64::min),
        high: ms.iter().copied().fold(0.0, f64::max),
        average: ms.iter().sum::<f64>() / ms.len() as f64,
    })
}

/// The USB link of the device `vendor:product` (lowercase hex, as sysfs writes it), read from
/// `<sys>/bus/usb/devices`: its speed and each input endpoint's kind and interval. None when
/// it isn't on USB here.
pub fn usb_link(sys: &Path, vendor: &str, product: &str) -> Option<String> {
    let read = |p: &Path| std::fs::read_to_string(p).ok().map(|s| s.trim().to_owned());
    let devices = sys.join("bus/usb/devices");
    let mut entries = std::fs::read_dir(&devices)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect::<Vec<_>>();
    entries.sort();
    let device = entries.iter().find(|d| {
        read(&d.join("idVendor")).as_deref() == Some(vendor)
            && read(&d.join("idProduct")).as_deref() == Some(product)
    })?;
    let name = device.file_name()?.to_str()?.to_owned();
    let speed = read(&device.join("speed")).unwrap_or_else(|| "?".into());
    let mut ins = Vec::new();
    for interface in entries.iter().filter(|e| {
        e.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(&format!("{name}:")))
    }) {
        let mut endpoints = std::fs::read_dir(interface)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("ep_"))
            })
            .collect::<Vec<_>>();
        endpoints.sort();
        for ep in endpoints {
            if read(&ep.join("direction")).as_deref() != Some("in") {
                continue;
            }
            let text = match read(&ep.join("type")).as_deref() {
                Some("Interrupt") => format!(
                    "polled every {}",
                    read(&ep.join("interval")).unwrap_or_else(|| "?".into())
                ),
                Some("Bulk") => "bulk, read on demand".into(),
                _ => continue,
            };
            if !ins.contains(&text) {
                ins.push(text);
            }
        }
    }
    let mut link = format!("USB {speed} Mb/s");
    for text in ins {
        link.push_str(&format!(" · {text}"));
    }
    Some(link)
}

/// The last result for one device and connection.
#[derive(Clone, Debug, PartialEq)]
pub struct Kept {
    pub device: String,
    pub connection: String,
    /// Seconds since the epoch.
    pub when: u64,
    pub result: Kind,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Link(Link),
    Presses { low: f64, high: f64, average: f64 },
}
impl Kept {
    fn line(&self) -> String {
        let (word, [a, b, c], dropped) = match self.result {
            Kind::Link(Link {
                per_second,
                gap_ms,
                jitter_ms,
                dropped,
            }) => ("link", [per_second, gap_ms, jitter_ms], dropped),
            Kind::Presses { low, high, average } => ("presses", [low, high, average], None),
        };
        let mut line = format!(
            "{}\t{}\t{}\t{word}\t{a}\t{b}\t{c}",
            self.device, self.connection, self.when
        );
        if let Some(ms) = dropped {
            line.push_str(&format!("\t{ms}"));
        }
        line
    }
    fn parse(line: &str) -> Option<Self> {
        let f = line.split('\t').collect::<Vec<_>>();
        let &[device, connection, when, word, a, b, c, ref extra @ ..] = f.as_slice() else {
            return None;
        };
        let number = |v: &str| v.parse::<f64>().ok().filter(|v| v.is_finite());
        let [a, b, c] = [a, b, c].map(number);
        let (a, b, c) = (a?, b?, c?);
        Some(Self {
            device: device.to_owned(),
            connection: connection.to_owned(),
            when: when.parse().ok()?,
            result: match (word, extra) {
                ("link", [] | [_]) => Kind::Link(Link {
                    per_second: a,
                    gap_ms: b,
                    jitter_ms: c,
                    dropped: match extra.first() {
                        Some(ms) => Some(number(ms)?),
                        None => None,
                    },
                }),
                ("presses", []) => Kind::Presses {
                    low: a,
                    high: b,
                    average: c,
                },
                _ => return None,
            },
        })
    }
    /// `USB (12 Nov): one every 4.0 ms`, dated from `now` (both epoch seconds).
    pub fn text(&self, now: u64) -> String {
        let what = match self.result {
            Kind::Link(Link { gap_ms, .. }) => format!("one every {gap_ms:.1} ms"),
            Kind::Presses { average, .. } => format!("handled after {average:.0} ms on average"),
        };
        format!("{} ({}): {what}", self.connection, date(self.when, now))
    }
}

/// The kept results; a missing or unreadable file (or line) is ignored, never an error.
pub fn load(path: &Path) -> Vec<Kept> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(Kept::parse)
        .collect()
}
/// Keep `result` as the last one for its device, connection and kind, saved atomically (a
/// temporary file renamed into place); a failed save leaves the old file as it was.
pub fn keep(path: &Path, result: Kept) -> crate::Result<Vec<Kept>> {
    let same_kind =
        |k: &Kept| std::mem::discriminant(&k.result) == std::mem::discriminant(&result.result);
    let mut all = load(path);
    all.retain(|k| {
        !(k.device == result.device && k.connection == result.connection && same_kind(k))
    });
    all.push(result);
    // Names can't break the file's lines or columns.
    if all.iter().any(|k| {
        [&k.device, &k.connection]
            .iter()
            .any(|s| s.is_empty() || s.contains(['\t', '\n', '\r']))
    }) {
        return Err("validation: a device or connection name can't be kept".into());
    }
    let text = all.iter().map(|k| k.line() + "\n").collect::<String>();
    let temporary = path.with_extension("tmp");
    // Synced before the rename, as the config's save: a crash never leaves an empty file.
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!(
            "file I/O: cannot save {}: {e}; kept results unchanged",
            path.display()
        )
    })?;
    Ok(text.lines().filter_map(Kept::parse).collect()) // as a reload reads them
}

/// `today 14:02`, or `12 Nov` for another day, in local time.
pub fn date(when: u64, now: u64) -> String {
    let local = |secs: u64| {
        let time = secs as libc::time_t;
        // SAFETY: localtime_r fills the zeroed tm it is given and touches nothing else.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&time, &mut tm) };
        tm
    };
    let (then, today) = (local(when), local(now));
    if (then.tm_year, then.tm_yday) == (today.tm_year, today.tm_yday) {
        format!("today {:02}:{:02}", then.tm_hour, then.tm_min)
    } else {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!(
            "{} {}",
            then.tm_mday,
            MONTHS[then.tm_mon.clamp(0, 11) as usize]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady(every_us: u64, n: u64) -> Vec<u64> {
        (0..n).map(|i| 1_000_000 + i * every_us).collect()
    }

    #[test]
    fn a_steady_link_is_green_with_no_jitter() {
        let link = link(&steady(8_000, 376)).unwrap();
        assert_eq!(link.gap_ms, 8.0);
        assert_eq!(link.jitter_ms, 0.0);
        assert_eq!(link.per_second, 125.0);
        assert_eq!(link.dropped, None);
        assert!(link.text().ends_with("🟢"), "{}", link.text());
    }

    #[test]
    fn jitter_is_the_spread_of_the_gaps() {
        // Gaps alternate 6 and 10 ms: average 8, standard deviation 2.
        let mut at = 0;
        let stamps = (0..101)
            .map(|i| {
                at += if i % 2 == 0 { 6_000 } else { 10_000 };
                at
            })
            .collect::<Vec<_>>();
        let link = link(&stamps).unwrap();
        assert!((link.gap_ms - 8.0).abs() < 0.05, "{link:?}");
        assert!((link.jitter_ms - 2.0).abs() < 0.05, "{link:?}");
        assert_eq!(band(link.gap_ms, false), "🟢");
    }

    #[test]
    fn a_dropped_report_is_red_and_named() {
        let mut stamps = steady(4_000, 50);
        let last = *stamps.last().unwrap();
        stamps.extend((1..50).map(|i| last + 60_000 + i * 4_000)); // one 64 ms gap
        let link = link(&stamps).unwrap();
        assert_eq!(link.dropped, Some(64.0));
        assert!(link.text().contains("a 64 ms gap"), "{}", link.text());
        assert!(link.text().ends_with("🔴"));
    }

    #[test]
    fn a_clock_jump_is_not_measured() {
        let mut stamps = steady(8_000, 10);
        stamps.push(500); // the realtime clock went back
        assert_eq!(link(&stamps), Err(CLOCK_CHANGED.into()));
        assert_eq!(
            delay_ms(1_000, 5_000),
            None,
            "handled before it was stamped"
        );
        assert_eq!(delay_ms(3_000_000, 1_000), None, "over a second");
        assert_eq!(delay_ms(15_000, 3_000), Some(12.0));
        assert!(link(&[1, 2]).is_err(), "too few reports");
        assert!(link(&[1, 1, 1]).is_err(), "no elapsed time");
    }

    #[test]
    fn press_delays_give_lowest_highest_average_and_band() {
        let d = delays(&[2.0, 31.0, 9.0]).unwrap();
        assert_eq!((d.low, d.high, d.average), (2.0, 31.0, 14.0));
        assert_eq!(
            d.text(3),
            "3 presses: handled after 2–31 ms, average 14 ms 🟡"
        );
        assert_eq!(delays(&[]), None);
        assert_eq!(band(25.0, false), "🔴");
    }

    #[test]
    fn kept_results_replace_their_own_connection_and_ignore_bad_lines() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-speed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("speed-tests.tsv");
        std::fs::write(&path, "garbage\nXbox\tUSB\tnot-a-date\tlink\t1\t2\t3\n").unwrap();
        assert!(load(&path).is_empty(), "unreadable lines are ignored");
        let kept = |connection: &str, when, gap_ms| Kept {
            device: "Xbox Wireless Controller".into(),
            connection: connection.into(),
            when,
            result: Kind::Link(Link {
                per_second: 1000.0 / gap_ms,
                gap_ms,
                jitter_ms: 1.0,
                dropped: None,
            }),
        };
        keep(&path, kept("USB", 100, 4.0)).unwrap();
        keep(&path, kept("Bluetooth", 200, 8.1)).unwrap();
        let all = keep(&path, kept("USB", 300, 4.5)).unwrap();
        assert_eq!(load(&path), all, "saved as returned");
        assert_eq!(all.len(), 2, "one per connection: {all:?}");
        assert!(all.iter().any(|k| k.connection == "USB" && k.when == 300));
        assert!(keep(&path, kept("US\tB", 1, 1.0)).is_err());
        assert_eq!(load(&path), all, "a refused save keeps the file");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn usb_link_reads_speed_and_input_endpoints() {
        let dir = std::env::temp_dir().join(format!("vibeconsole-usb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let devices = dir.join("bus/usb/devices");
        let write = |p: &str, text: &str| {
            let p = devices.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        write("3-3.3/idVendor", "09e8\n");
        write("3-3.3/idProduct", "1049\n");
        write("3-3.3/speed", "12\n");
        for (ep, dir_, kind, interval) in [
            ("3-3.3:1.0/ep_01", "out", "Interrupt", "1ms"),
            ("3-3.3:1.0/ep_81", "in", "Interrupt", "1ms"),
            ("3-3.3:1.2/ep_83", "in", "Bulk", "0ms"),
        ] {
            write(&format!("{ep}/direction"), dir_);
            write(&format!("{ep}/type"), kind);
            write(&format!("{ep}/interval"), interval);
        }
        assert_eq!(
            usb_link(&dir, "09e8", "1049").as_deref(),
            Some("USB 12 Mb/s · polled every 1ms · bulk, read on demand")
        );
        assert_eq!(usb_link(&dir, "045e", "0b12"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
