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
                // Break after the last space so words stay whole; a longer word still splits.
                let tail = match row.rfind(' ') {
                    Some(at) if c != ' ' && !row[..at].trim_end().is_empty() => {
                        row.split_off(at + 1)
                    }
                    _ => String::new(),
                };
                rows.push(row.trim_end().to_owned());
                used = cells(&tail).iter().map(|(_, size)| size).sum();
                row = tail;
                if c == ' ' {
                    continue;
                }
            }
            row.push(c);
            used += size;
        }
        rows.push(row);
    }
    rows
}

/// The colours of one theme by role (v7.7 spec, Palette instead of constants). Drawing code
/// names a [`Role`], never a colour.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Palette {
    pub background: [u8; 3],
    pub border: [u8; 3],
    pub text: [u8; 3],
    pub muted: [u8; 3],
    pub accent: [u8; 3],
    pub heading: [u8; 3],
    pub title: [u8; 3],
    pub green: [u8; 3],
    pub yellow: [u8; 3],
    pub red: [u8; 3],
    pub selection: [u8; 3],
}
/// The default theme (v7.7 spec, Themes). The other 13 arrive with ticket 154.
pub const TOKYO_NIGHT: Palette = Palette {
    background: [0x1a, 0x1b, 0x26],
    border: [0x41, 0x48, 0x68],
    text: [0xc0, 0xca, 0xf5],
    muted: [0x81, 0x89, 0xb0],
    accent: [0x7a, 0xa2, 0xf7],
    heading: [0xbb, 0x9a, 0xf7],
    title: [0x7d, 0xcf, 0xff],
    green: [0x9e, 0xce, 0x6a],
    yellow: [0xe0, 0xaf, 0x68],
    red: [0xf7, 0x76, 0x8e],
    selection: [0x28, 0x34, 0x57],
};
/// What a piece of text is; [`style`] turns it into the palette's colours.
#[derive(Clone, Copy)]
enum Role {
    Border,
    Focus,
    /// The selected row: `>` plus the selection background and bold, never the primary look.
    Selected,
    Heading,
    Title,
    Running,
    Paused,
    Error,
    Muted,
    BankA,
    BankB,
    /// The active tab: filled with the accent and underlined (and bracketed, so not colour only).
    ActiveTab,
}
fn style(role: Role) -> String {
    let p = TOKYO_NIGHT;
    let fg = |[r, g, b]: [u8; 3]| format!("38;2;{r};{g};{b}");
    let bg = |[r, g, b]: [u8; 3]| format!("48;2;{r};{g};{b}");
    match role {
        Role::Border => fg(p.border),
        Role::Focus => format!("1;{}", fg(p.accent)),
        Role::Selected => format!("1;{};{}", fg(p.text), bg(p.selection)),
        Role::Heading => format!("1;{}", fg(p.heading)),
        Role::Title => format!("1;{}", fg(p.title)),
        Role::Running => fg(p.green),
        Role::Paused => fg(p.yellow),
        Role::Error => fg(p.red),
        Role::Muted => fg(p.muted),
        Role::BankA => format!("1;{}", fg(p.red)),
        Role::BankB => format!("1;{}", fg(p.green)),
        Role::ActiveTab => format!("1;4;{};{}", fg(p.background), bg(p.accent)),
    }
}

fn paint(text: &str, role: Role, color: bool) -> String {
    if color {
        format!("\x1b[{}m{text}\x1b[0m", style(role))
    } else {
        text.into()
    }
}

fn paint_bulbs(text: &str, color: bool) -> String {
    text.replace(bulb(true), &paint(bulb(true), Role::Paused, color))
        .replace(bulb(false), &paint(bulb(false), Role::Muted, color))
        .replace("[●]", &paint("[●]", Role::Paused, color))
        .replace("Bank A", &paint("Bank A", Role::BankA, color))
        .replace("Bank B", &paint("Bank B", Role::BankB, color))
        // v7.7 ticket 151: agent states keep their word; the colour only adds to it.
        .replace("NEEDS INPUT", &paint("NEEDS INPUT", Role::Error, color))
        .replace("DECISION", &paint("DECISION", Role::Paused, color))
        .replace("WORKING", &paint("WORKING", Role::Focus, color))
        .replace("FINISHED", &paint("FINISHED", Role::Running, color))
        .replace("IDLE", &paint("IDLE", Role::Muted, color))
}

/// One agent card (v7.7 ticket 151, Agents pane Gallery view): a box `width` wide with
/// `title` on its top border; the selected card has a double border and `>`.
pub fn card(title: &str, lines: &[String], width: usize, selected: bool) -> Vec<String> {
    let [h, v, tl, tr, bl, br] = if selected {
        ["═", "║", "╔", "╗", "╚", "╝"]
    } else {
        ["─", "│", "┌", "┐", "└", "┘"]
    };
    let inner = width.saturating_sub(4);
    let title = format!(" {}{title} ", if selected { "> " } else { "" });
    let title = fit(&title, inner.min(width_of(&title)));
    let mut rows = vec![format!(
        "{tl}{h}{title}{}{tr}",
        h.repeat(width.saturating_sub(3 + width_of(&title)))
    )];
    rows.extend(
        lines
            .iter()
            .map(|line| format!("{v} {} {v}", fit(line, inner))),
    );
    rows.push(format!("{bl}{}{br}", h.repeat(width.saturating_sub(2))));
    rows
}

/// The inner width of a full-width pane in a `columns`-wide terminal.
pub fn pane_width(columns: usize) -> usize {
    columns.saturating_sub(5)
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
    let border = if focused { Role::Focus } else { Role::Border };
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
        let text = if (line.starts_with("> ") && focused) || line.starts_with("◆ ") {
            paint(&text, Role::Selected, color)
        } else if line.starts_with("── ") {
            paint(&text, Role::Heading, color)
        } else if line.starts_with("Error:") {
            paint(&text, Role::Error, color)
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

/// The tab bar (v7.7 ticket 150): `VibeConsole  1 Live │ 2 Mappings  3 Agents …`, a `│`
/// before each tab that starts a group, the active tab in brackets (and, in colour, filled
/// and underlined). When it doesn't fit, the tabs around the active one between `‹ ›`; below
/// 60 columns only the active tab and its place (`‹ Agents 3/10 ›`).
fn tab_bar(tabs: &[String], groups: &[usize], active: usize, width: usize, color: bool) -> String {
    let slot = |index: usize| {
        let label = &tabs[index];
        if index == active {
            format!("[{label}]")
        } else {
            format!(" {label} ")
        }
    };
    let gap = |index: usize| if groups.contains(&index) { "│" } else { "" };
    let draw = |range: std::ops::Range<usize>| {
        range
            .map(|index| {
                let slot = slot(index);
                let slot = if index == active {
                    paint(&slot, Role::ActiveTab, color)
                } else {
                    slot
                };
                format!("{}{slot}", paint(gap(index), Role::Border, color))
            })
            .collect::<String>()
    };
    let span = |range: std::ops::Range<usize>| -> usize {
        range
            .map(|index| width_of(gap(index)) + width_of(&slot(index)))
            .sum()
    };
    let title = "VibeConsole ";
    if width_of(title) + span(0..tabs.len()) <= width {
        return format!(
            "{}{}",
            paint(title, Role::Title, color),
            draw(0..tabs.len())
        );
    }
    let edges = |start: usize, end: usize| 2 + span(start..end); // ‹ and ›
    let mut start = active;
    while start > 0 && edges(start - 1, active + 1) <= width {
        start -= 1;
    }
    let mut end = active + 1;
    while end < tabs.len() && edges(start, end + 1) <= width {
        end += 1;
    }
    if width < 60 || edges(start, end) > width {
        let name = tabs[active]
            .split_once(' ')
            .map_or(tabs[active].as_str(), |p| p.1);
        let place = format!("‹ {name} {}/{} ›", active + 1, tabs.len());
        return paint(&place, Role::ActiveTab, color);
    }
    let more = |shown: bool| if shown { "‹" } else { " " };
    format!(
        "{}{}{}",
        paint(more(start > 0), Role::Muted, color),
        draw(start..end),
        paint(if end < tabs.len() { "›" } else { " " }, Role::Muted, color)
    )
}

/// A titled box of a tab's body (v7.7 ticket 151). Rows starting `> ` are the selection,
/// `── ` a heading and `◆ ` the control pressed in the last second; `anchor` stays visible.
pub struct Pane {
    pub title: String,
    pub rows: Vec<String>,
    pub anchor: usize,
    pub focused: bool,
}

/// One tiled screen: the tab bar, a header, the active tab's rows | details | live panes
/// (or, when `panes` is set, those panes stacked), and a key-hint status line.
pub struct Screen {
    /// Tab labels (`1 Live`); empty draws no tab bar.
    pub tabs: Vec<String>,
    /// Indexes of the tabs that start a group (a `│` before them).
    pub groups: Vec<usize>,
    pub active_tab: usize,
    pub header: String,
    pub commands: Vec<String>,
    pub anchor: usize,
    pub focus_commands: bool,
    pub details: String,
    pub notice: String,
    pub live: Vec<String>,
    pub hints: String,
    /// The tab's own panes, drawn full width one under the other; empty keeps the
    /// rows | details | live layout (the screens a command opens).
    pub panes: Vec<Pane>,
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
        let bar = usize::from(!self.tabs.is_empty());
        let body = height.saturating_sub(bar + header.len() + hints.len());
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
        let bar = usize::from(!self.tabs.is_empty());
        let body = height.saturating_sub(bar + header.len() + hints.len());
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
        let mut rows = Vec::new();
        if !self.tabs.is_empty() {
            let bar = |color| tab_bar(&self.tabs, &self.groups, self.active_tab, width, color);
            // The bar is built to fit; padding keeps every row the same width.
            let pad = width.saturating_sub(width_of(&bar(false)));
            rows.push(format!("{}{}", bar(color), " ".repeat(pad)));
        }
        rows.extend(header.iter().enumerate().map(|(index, row)| {
            let role = if index == 0 {
                Role::Title
            } else if row.contains("RUNNING") {
                Role::Running
            } else {
                Role::Paused
            };
            paint_bulbs(&paint(&fit(row, width), role, color), color)
        }));
        if !self.panes.is_empty() {
            let notice = wrap(&self.notice, width.saturating_sub(1));
            let notice = &notice[..notice.len().min(3) * usize::from(!self.notice.is_empty())];
            let mut body = height.saturating_sub(rows.len() + hints.len() + notice.len());
            for (index, p) in self.panes.iter().enumerate() {
                // Each pane after this one keeps at least its borders and one row.
                let reserve = 3 * (self.panes.len() - index - 1);
                let size = (p.rows.len() + 2).min(body.saturating_sub(reserve));
                if size >= 3 {
                    rows.extend(pane(
                        &p.title,
                        &p.rows,
                        p.anchor,
                        (width, size),
                        p.focused,
                        color,
                    ));
                    body -= size;
                }
            }
            rows.extend((0..body).map(|_| " ".repeat(width)));
            rows.extend(notice.iter().map(|row| {
                let role = if row.starts_with("Error:") {
                    Role::Error
                } else {
                    Role::Paused
                };
                paint(&fit(row, width), role, color)
            }));
            rows.extend(
                hints
                    .iter()
                    .map(|row| paint(&fit(row, width), Role::Muted, color)),
            );
            rows.truncate(height);
            return rows.join("\r\n");
        }
        let (text_width, text_height) = self.details_size(columns, height);
        let mut text = wrap(&self.details, text_width);
        text.resize(text_height, String::new());
        text.extend(self.notice_rows(text_width));
        let focus = self.focus_commands;
        let title = self.tabs.get(self.active_tab).map_or("Commands", |label| {
            label.split_once(' ').map_or(label.as_str(), |p| p.1)
        });
        let commands = pane(title, &self.commands, self.anchor, commands, focus, color);
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
        rows.extend(
            hints
                .iter()
                .map(|row| paint(&fit(row, width), Role::Muted, color)),
        );
        rows.truncate(height);
        rows.join("\r\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v6/01: dialogs split words at the column ("P│rogram"). Rows now break at spaces.
    #[test]
    fn wrap_keeps_words_whole_and_still_splits_overlong_words() {
        let text = "Not saved: validation: piano keys cannot be mapped in Program 1: octave/tempo/arpeggiator feedback comes from Bank A Pad 1, Bank A Pad 2.";
        let rows = wrap(text, 56);
        assert!(rows.iter().all(|row| width_of(row) <= 56), "{rows:?}");
        assert_eq!(rows.join(" "), text); // only spaces were consumed at breaks
        assert_eq!(
            rows,
            [
                "Not saved: validation: piano keys cannot be mapped in",
                "Program 1: octave/tempo/arpeggiator feedback comes from",
                "Bank A Pad 1, Bank A Pad 2.",
            ]
        );
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap("ab  cd", 3), ["ab", "cd"]);
    }

    /// v7.7 ticket 151: a tab's panes stack full width, the selected row stays visible as
    /// panes shrink, the pressed line is drawn like the selection, and the notice keeps its
    /// place above the hints at every size.
    #[test]
    fn panes_stack_keep_the_selection_and_the_notice() {
        let pane = |title: &str, rows: Vec<String>, anchor, focused| Pane {
            title: title.into(),
            rows,
            anchor,
            focused,
        };
        let agents = (0..30)
            .map(|n| format!("{}● agent-{n} IDLE", if n == 25 { "> " } else { "  " }))
            .collect();
        let screen = Screen {
            tabs: vec!["1 Live".into(), "2 Mappings".into()],
            groups: vec![1],
            active_tab: 0,
            header: "VibeConsole\nProgram 1 | RUNNING".into(),
            commands: Vec::new(),
            anchor: 0,
            focus_commands: false,
            details: String::new(),
            notice: "Error: go to agent: tmux failed".into(),
            live: Vec::new(),
            hints: "Tab ←→ 1–0: tabs | Ctrl+C: quit".into(),
            panes: vec![
                pane("Live", vec!["  ❚❚ Pause".into(), "  Quit".into()], 0, false),
                pane("Agents", agents, 25, true),
                pane(
                    "Pads and buttons",
                    vec!["◆ Pad 5 · Orca · sent  ← pressed".into()],
                    0,
                    false,
                ),
            ],
        };
        for (width, height) in [(120, 30), (80, 24), (40, 16)] {
            let frame = screen.render(width, height, false);
            assert_eq!(frame.lines().count(), height);
            assert!(frame.lines().all(|l| width_of(l) < width), "{frame}");
            for text in ["> ● agent-25", "Error: go to agent", "Ctrl+C", "─ Live "] {
                assert!(frame.contains(text), "{width}×{height} {text}:\n{frame}");
            }
        }
        let colored = screen.render(120, 30, true);
        let lit = format!("\x1b[{}m◆ Pad 5", style(Role::Selected));
        assert!(colored.contains(&lit), "the pressed line looks selected");
        assert!(colored.contains(&format!("\x1b[{}mIDLE", style(Role::Muted))));
    }

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
        let tabs = [
            "1 Live",
            "2 Mappings",
            "3 Agents",
            "4 Prompts",
            "5 Devices",
            "6 Apps & profiles",
            "7 Xbox",
            "8 System",
            "9 Insights",
            "0 AI",
        ]
        .map(String::from)
        .to_vec();
        let mut screen = Screen {
            tabs: tabs.clone(),
            groups: vec![1, 4, 7],
            active_tab: 1,
            header: "VibeConsole | MIDI 💡 ON connected hw:1,0,0 | Input 💡 OFF\nProgram 2 — VSCode (hardware/RAM verified) | RUNNING".into(),
            commands: vec!["── Run".into(), "> /run".into(), "  /pause".into()],
            anchor: 1,
            focus_commands: true,
            details: "/run\n\nEnable mappings.\n界界界".into(),
            notice: "Error: MIDI disconnected".into(),
            live: vec!["Active holds/toggles: none".into()],
            hints: "Up/Down: move | Enter: confirm | Space: select | Esc: back | Ctrl+C: quit".into(),
            panes: Vec::new(),
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
            // Narrow and short, the rows pane scrolls with the selection (headings may go).
            if width >= 80 {
                row_of(&frame, "── Run");
            }
            for text in ["RUNNING", "> /run", "Error: MIDI disconnected", "Ctrl+C"] {
                row_of(&frame, text); // nothing essential is cut off at any width
            }
            let (commands, details, live) = (
                row_of(&frame, "▸ Mappings"),
                row_of(&frame, "─ Details "),
                row_of(&frame, "─ Live "),
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
        assert!(colored.contains('\x1b') && colored.contains("▸ Mappings"));
        assert!(screen.render(39, 24, false).contains("Resize terminal"));
        assert!(screen.render(80, 15, false).contains("Resize terminal"));

        // v7.7 ticket 150: the tab bar. Wide: every tab in four groups, the active one in
        // brackets (and filled and underlined in colour).
        screen.active_tab = 2;
        let bar = |frame: String| frame.lines().next().unwrap().to_owned();
        let wide = bar(screen.render(170, 24, false));
        assert!(
            wide.starts_with(
                "VibeConsole  1 Live │ 2 Mappings [3 Agents] 4 Prompts │ 5 Devices  6 Apps & profiles  7 Xbox │ 8 System  9 Insights  0 AI "
            ),
            "{wide}"
        );
        let colored = bar(screen.render(170, 24, true));
        assert!(colored.contains("\x1b[1;4;38;2;26;27;38;48;2;122;162;247m[3 Agents]"));
        // 80 columns: the tabs around the active one, scrolling behind ‹ ›.
        screen.active_tab = 8;
        let narrow = bar(screen.render(80, 24, false));
        assert!(
            narrow.starts_with('‹') && narrow.contains("[9 Insights]"),
            "{narrow}"
        );
        assert!(!narrow.contains("1 Live"), "{narrow}");
        screen.active_tab = 0;
        let start = bar(screen.render(80, 24, false));
        assert!(
            start.starts_with(" [1 Live]") && start.contains('›'),
            "{start}"
        );
        // 40 columns: only the active tab and its place.
        screen.active_tab = 5;
        assert_eq!(
            bar(screen.render(40, 16, false)).trim_end(),
            "‹ Apps & profiles 6/10 ›"
        );
        for (width, height) in [(80, 24), (40, 16)] {
            for active in 0..tabs.len() {
                screen.active_tab = active;
                let frame = screen.render(width, height, false);
                assert_eq!(frame.lines().count(), height);
                assert!(
                    frame.lines().all(|line| width_of(line) < width),
                    "{width}×{height}:\n{frame}"
                );
                for text in ["RUNNING", "> /run", "Ctrl+C"] {
                    row_of(&frame, text);
                }
            }
        }
    }
}
