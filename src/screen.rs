use std::sync::OnceLock;

unsafe extern "C" {
    fn wcwidth(character: libc::wchar_t) -> libc::c_int;
}

fn cells(text: &str) -> Vec<(char, usize)> {
    // ponytail: native widths count code points; use grapheme-aware rendering if labels need joined emoji.
    static LOCALE: OnceLock<usize> = OnceLock::new();
    // Use a thread-local locale while measuring, without changing other workers' locale.
    let locale = *LOCALE.get_or_init(|| unsafe {
        libc::newlocale(
            libc::LC_CTYPE_MASK,
            c"C.UTF-8".as_ptr(),
            std::ptr::null_mut(),
        ) as usize
    }) as libc::locale_t;
    // SAFETY: the locale remains allocated for the process; restore this thread's previous locale.
    let previous = unsafe { libc::uselocale(locale) };
    let cells = text
        .chars()
        .map(|c| {
            let c = if c.is_control() { ' ' } else { c };
            let width = unsafe { wcwidth(c as libc::wchar_t) };
            (c, if width < 0 { 1 } else { width as usize })
        })
        .collect();
    unsafe { libc::uselocale(previous) };
    cells
}

pub fn fit(text: &str, width: usize) -> String {
    let cells = cells(text);
    let total: usize = cells.iter().map(|(_, width)| width).sum();
    let limit = if total > width {
        width.saturating_sub(1)
    } else {
        width
    };
    let mut used = 0;
    let mut result = String::new();
    for (c, size) in cells {
        if used + size > limit {
            break;
        }
        result.push(c);
        used += size;
    }
    if total > width && width > 0 {
        result.push('…');
        used += 1;
    }
    result.push_str(&" ".repeat(width.saturating_sub(used)));
    result
}

pub fn bulb(on: bool) -> &'static str {
    if on { "💡 ON" } else { "💡 OFF" }
}

/// One toggle as `[●] Toggle 1: Bank A Pad 1 (Shift)`; `[○]` when not latched.
pub fn toggle_line(number: usize, name: &str, action: &str, on: bool) -> String {
    format!(
        "[{}] Toggle {number}: {name} ({action})",
        if on { '●' } else { '○' }
    )
}

/// `items` in as many 24-column cells as fit `width`.
pub fn grid(items: &[String], width: usize) -> Vec<String> {
    let columns = (width / 24).max(1);
    items
        .chunks(columns)
        .map(|row| {
            let row: String = row.iter().map(|item| fit(item, width / columns)).collect();
            row.trim_end().into()
        })
        .collect()
}

/// Plain titled box `width` columns wide, for grouping lines inside a pane.
pub fn group(title: &str, lines: &[String], width: usize) -> Vec<String> {
    let inner = width.saturating_sub(4);
    let title = fit(&format!(" {title} "), inner.min(width_of(title) + 2));
    let mut rows = vec![format!(
        "┌─{title}{}┐",
        "─".repeat(width.saturating_sub(3 + width_of(&title)))
    )];
    rows.extend(lines.iter().map(|line| format!("│ {} │", fit(line, inner))));
    rows.push(format!("└{}┘", "─".repeat(width.saturating_sub(2))));
    rows
}

pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(2);
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut row = String::new();
        let mut used = 0;
        for (c, size) in cells(line) {
            if used + size > width {
                rows.push(row);
                row = String::new();
                used = 0;
            }
            row.push(c);
            used += size;
        }
        rows.push(row);
    }
    rows
}

// Tokyo Night–like palette (24-bit foreground/background); plain text when colour is off.
const BORDER: &str = "38;2;65;72;104";
const FOCUS: &str = "1;38;2;122;162;247";
const SELECTED: &str = "1;38;2;26;27;38;48;2;122;162;247";
const HEADING: &str = "1;38;2;187;154;247";
const TITLE: &str = "1;38;2;125;207;255";
const RUNNING: &str = "38;2;158;206;106";
const PAUSED: &str = "38;2;224;175;104";
const ERROR: &str = "38;2;247;118;142";
const DIM: &str = "38;2;86;95;137";
const BANK_A: &str = "1;38;2;247;118;142";
const BANK_B: &str = "1;38;2;158;206;106";

fn paint(text: &str, style: &str, color: bool) -> String {
    if color {
        format!("\x1b[{style}m{text}\x1b[0m")
    } else {
        text.into()
    }
}

fn paint_bulbs(text: &str, color: bool) -> String {
    text.replace(bulb(true), &paint(bulb(true), PAUSED, color))
        .replace(bulb(false), &paint(bulb(false), DIM, color))
        .replace("[●]", &paint("[●]", PAUSED, color))
        .replace("Bank A", &paint("Bank A", BANK_A, color))
        .replace("Bank B", &paint("Bank B", BANK_B, color))
}

fn width_of(text: &str) -> usize {
    cells(text).iter().map(|(_, width)| width).sum()
}

/// Wrap status text at its ` | ` separators, splitting a segment only when it alone is too wide.
fn pack(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut row = String::new();
        for segment in line.split(" | ") {
            if !row.is_empty() && width_of(&row) + 3 + width_of(segment) <= width {
                row = format!("{row} | {segment}");
                continue;
            }
            if !row.is_empty() {
                rows.push(row);
            }
            let mut parts = wrap(segment, width);
            row = parts.pop().unwrap_or_default();
            rows.extend(parts);
        }
        rows.push(row);
    }
    rows
}

/// Outer (width, height) of the commands, details and live panes for `body` rows.
/// 160+ columns: side by side; 100–159: commands | details over live; narrower: stacked.
pub fn panes(
    columns: usize,
    body: usize,
    commands: usize,
    live: usize,
    focus: bool,
) -> [(usize, usize); 3] {
    const SIDE: usize = 30;
    const LIVE: usize = 44;
    let width = columns.saturating_sub(1); // Leave the last column to avoid autowrap.
    let live = if live == 0 { 0 } else { live + 2 };
    if columns >= 160 {
        [(SIDE, body), (width - SIDE - LIVE, body), (LIVE, body)]
    } else if columns >= 100 {
        let live = live.min(body / 3);
        [
            (SIDE, body),
            (width - SIDE, body - live),
            (width - SIDE, live),
        ]
    } else {
        // Stacked, a screen opened from the menu gets the space: commands keep only the
        // active row and live controls show at the menu (as V2's toggle panel did).
        // At the menu, live controls come first, then commands, keeping 5 rows of details.
        let live = if focus {
            live.min(body.saturating_sub(8))
        } else {
            0
        };
        let commands = if focus {
            (commands + 2)
                .min(body / 3)
                .min(body.saturating_sub(live + 5))
                .max(3)
        } else {
            3
        };
        [
            (width, commands),
            (width, body.saturating_sub(commands + live)),
            (width, live),
        ]
    }
}

/// Thin-bordered pane. The focused pane is marked by `▸` as well as colour; rows starting
/// with `> ` are the selection and `── ` a heading. Scrolls so row `anchor` stays visible.
fn pane(
    title: &str,
    lines: &[String],
    anchor: usize,
    (width, height): (usize, usize),
    focused: bool,
    color: bool,
) -> Vec<String> {
    if height < 3 || width < 8 {
        return vec![" ".repeat(width); height];
    }
    let inner = width - 4;
    let border = if focused { FOCUS } else { BORDER };
    let title = format!("{}{title} ", if focused { "▸ " } else { " " });
    let title = fit(&title, inner.min(width_of(&title)));
    let mut rows = vec![paint(
        &format!("┌─{title}{}┐", "─".repeat(width - 3 - width_of(&title))),
        border,
        color,
    )];
    let first = anchor.saturating_sub(height - 3);
    for index in first..first + height - 2 {
        let line = lines.get(index).map(String::as_str).unwrap_or("");
        let text = fit(line, inner);
        let text = if line.starts_with("> ") && focused {
            paint(&text, SELECTED, color)
        } else if line.starts_with("── ") {
            paint(&text, HEADING, color)
        } else if line.starts_with("Error:") {
            paint(&text, ERROR, color)
        } else {
            paint_bulbs(&text, color)
        };
        let side = paint("│", border, color);
        rows.push(format!("{side} {text} {side}"));
    }
    rows.push(paint(
        &format!("└{}┘", "─".repeat(width - 2)),
        border,
        color,
    ));
    rows
}

/// One tiled screen: header, commands | details | live panes, and a key-hint status line.
pub struct Screen {
    pub header: String,
    pub commands: Vec<String>,
    pub anchor: usize,
    pub focus_commands: bool,
    pub details: String,
    pub notice: String,
    pub live: Vec<String>,
    pub hints: String,
}

impl Screen {
    fn sizes(
        &self,
        columns: usize,
        height: usize,
    ) -> (Vec<String>, Vec<String>, [(usize, usize); 3]) {
        let width = columns.saturating_sub(1).max(1);
        let header = pack(&self.header, width);
        let hints = pack(&self.hints, width);
        let body = height.saturating_sub(header.len() + hints.len());
        let sizes = panes(
            columns,
            body,
            self.commands.len(),
            self.live.len(),
            self.focus_commands,
        );
        (header, hints, sizes)
    }

    fn notice_rows(&self, width: usize) -> Vec<String> {
        if self.notice.is_empty() {
            return Vec::new();
        }
        let mut rows = vec![String::new()];
        rows.extend(wrap(&self.notice, width));
        rows
    }

    /// Inner details area left for screen text after the pinned notice.
    pub fn details_size(&self, columns: usize, height: usize) -> (usize, usize) {
        let (_, _, [_, (width, height), _]) = self.sizes(columns, height);
        let width = width.saturating_sub(4);
        let notice = self.notice_rows(width).len();
        (width, height.saturating_sub(2 + notice))
    }

    /// Inner (width, height) available to the live pane at most.
    pub fn live_size(&self, columns: usize, height: usize) -> (usize, usize) {
        let (header, hints, _) = self.sizes(columns, height);
        let body = height.saturating_sub(header.len() + hints.len());
        let [_, _, (width, height)] = panes(
            columns,
            body,
            self.commands.len(),
            body,
            self.focus_commands,
        );
        (width.saturating_sub(4), height.saturating_sub(2))
    }

    pub fn render(&self, columns: usize, height: usize, color: bool) -> String {
        let width = columns.saturating_sub(1).max(1);
        if columns < 40 || height < 16 {
            let rows = wrap(
                "Resize terminal to at least 40 × 16 · Esc back · Ctrl+C quit",
                width,
            );
            return (0..height)
                .map(|index| fit(rows.get(index).map(String::as_str).unwrap_or(""), width))
                .collect::<Vec<_>>()
                .join("\r\n");
        }
        let (header, hints, [commands, details, live]) = self.sizes(columns, height);
        let mut rows = header
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let style = if index == 0 {
                    TITLE
                } else if row.contains("RUNNING") {
                    RUNNING
                } else {
                    PAUSED
                };
                paint_bulbs(&paint(&fit(row, width), style, color), color)
            })
            .collect::<Vec<_>>();
        let (text_width, text_height) = self.details_size(columns, height);
        let mut text = wrap(&self.details, text_width);
        text.resize(text_height, String::new());
        text.extend(self.notice_rows(text_width));
        let focus = self.focus_commands;
        let commands = pane(
            "Commands",
            &self.commands,
            self.anchor,
            commands,
            focus,
            color,
        );
        let details = pane("Details", &text, 0, details, !focus, color);
        let live = pane("Live", &self.live, 0, live, false, color);
        if columns >= 160 {
            for ((a, b), c) in commands.iter().zip(&details).zip(&live) {
                rows.push(format!("{a}{b}{c}"));
            }
        } else if columns >= 100 {
            let right = details.iter().chain(&live);
            for (a, b) in commands.iter().zip(right) {
                rows.push(format!("{a}{b}"));
            }
        } else {
            rows.extend(commands.into_iter().chain(details).chain(live));
        }
        rows.extend(hints.iter().map(|row| paint(&fit(row, width), DIM, color)));
        rows.truncate(height);
        rows.join("\r\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_unicode_layout_and_plain_color_fallback() {
        assert_eq!(fit("界e\u{301}x", 4), "界e\u{301}x");
        assert_eq!(fit("界界界", 4), "界… ");
        assert_eq!(fit("a\x1bb", 4), "a b ");
        assert_eq!(fit(bulb(true), 5), "💡 ON");
        assert_eq!(
            toggle_line(1, "Bank A Pad 1", "Shift", true),
            "[●] Toggle 1: Bank A Pad 1 (Shift)"
        );
        assert!(toggle_line(2, "Knob 3", "Ctrl+K", false).starts_with("[○] Toggle 2"));
        let screen = Screen {
            header: "KeyAI | MIDI 💡 ON connected hw:1,0,0 | Input 💡 OFF\nProgram 2 — VSCode (hardware/RAM verified) | RUNNING".into(),
            commands: vec!["── Run".into(), "> /run".into(), "  /pause".into()],
            anchor: 1,
            focus_commands: true,
            details: "/run\n\nEnable mappings.\n界界界".into(),
            notice: "Error: MIDI disconnected".into(),
            live: vec!["Active holds/toggles: none".into()],
            hints: "Up/Down: move | Enter: confirm | Space: select | Esc: back | Ctrl+C: quit".into(),
        };
        let row_of = |frame: &str, text: &str| {
            frame
                .lines()
                .enumerate()
                .find_map(|(row, line)| line.find(text).map(|column| (row, column)))
                .unwrap()
        };
        for (width, height) in [(170, 24), (120, 24), (80, 30), (50, 18)] {
            let frame = screen.render(width, height, false);
            assert_eq!(frame.lines().count(), height);
            assert!(!frame.contains('\x1b'));
            assert!(frame.lines().all(|line| width_of(line) < width));
            for text in [
                "RUNNING",
                "── Run",
                "> /run",
                "Error: MIDI disconnected",
                "Ctrl+C",
            ] {
                row_of(&frame, text); // nothing essential is cut off at any width
            }
            let (commands, details, live) = (
                row_of(&frame, "▸ Commands"),
                row_of(&frame, " Details "),
                row_of(&frame, " Live "),
            );
            if width >= 160 {
                assert!(commands.0 == details.0 && details.0 == live.0);
                assert!(commands.1 < details.1 && details.1 < live.1);
            } else if width >= 100 {
                assert!(commands.0 == details.0 && commands.1 < details.1 && details.0 < live.0);
            } else {
                assert!(commands.0 < details.0 && details.0 < live.0);
            }
        }
        let colored = screen.render(120, 24, true);
        assert!(colored.contains('\x1b') && colored.contains("▸ Commands"));
        assert!(screen.render(39, 24, false).contains("Resize terminal"));
    }
}
