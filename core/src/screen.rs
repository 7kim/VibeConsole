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
/// The default theme (v7.7 spec, Themes).
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
const fn rgb(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}
/// A palette from the spec's table order: background, text, muted, accent, heading, title,
/// green, yellow, red, selection, border.
const fn theme(c: [u32; 11]) -> Option<Palette> {
    Some(Palette {
        background: rgb(c[0]),
        text: rgb(c[1]),
        muted: rgb(c[2]),
        accent: rgb(c[3]),
        heading: rgb(c[4]),
        title: rgb(c[5]),
        green: rgb(c[6]),
        yellow: rgb(c[7]),
        red: rgb(c[8]),
        selection: rgb(c[9]),
        border: rgb(c[10]),
    })
}
/// v7.7 ticket 154: the 14 themes in [`crate::mappings::THEMES`] order; `None` is Terminal
/// colours (the terminal's own 16 ANSI colours, no RGB codes).
#[rustfmt::skip]
pub const PALETTES: [Option<Palette>; 14] = [
    Some(TOKYO_NIGHT),
    None,
    theme([0x24283b, 0xc0caf5, 0x9aa2c8, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0x9ece6a, 0xe0af68, 0xf7768e, 0x2e3c64, 0x414868]),
    theme([0x1e1e2e, 0xcdd6f4, 0xa6adc8, 0x89b4fa, 0xcba6f7, 0x89dceb, 0xa6e3a1, 0xf9e2af, 0xf38ba8, 0x313244, 0x45475a]),
    theme([0x24273a, 0xcad3f5, 0xa5adcb, 0x8aadf4, 0xc6a0f6, 0x91d7e3, 0xa6da95, 0xeed49f, 0xed8796, 0x363a4f, 0x494d64]),
    theme([0xeff1f5, 0x4c4f69, 0x5c5f77, 0x1e66f5, 0x8839ef, 0x1a6f7d, 0x2d7a1f, 0xa24f00, 0xd20f39, 0xccd0da, 0xbcc0cc]),
    theme([0x282828, 0xebdbb2, 0xa89984, 0x83a598, 0xd3869b, 0x8ec07c, 0xb8bb26, 0xfabd2f, 0xfb4934, 0x3c3836, 0x504945]),
    theme([0xfbf1c7, 0x3c3836, 0x665c54, 0x076678, 0x8f3f71, 0x427b58, 0x79740e, 0x8f5a0a, 0x9d0006, 0xebdbb2, 0xd5c4a1]),
    theme([0x2e3440, 0xeceff4, 0xa7b1c2, 0x88c0d0, 0xb48ead, 0x8fbcbb, 0xa3be8c, 0xebcb8b, 0xe48890, 0x3b4252, 0x4c566a]),
    theme([0x282a36, 0xf8f8f2, 0xa4a9c8, 0xbd93f9, 0xff79c6, 0x8be9fd, 0x50fa7b, 0xffb86c, 0xff5555, 0x44475a, 0x44475a]),
    theme([0x282c34, 0xabb2bf, 0x959dab, 0x61afef, 0xc678dd, 0x56b6c2, 0x98c379, 0xe5c07b, 0xe06c75, 0x3e4451, 0x3e4451]),
    theme([0x1f1f28, 0xdcd7ba, 0xa6a69c, 0x7e9cd8, 0x957fb8, 0x7fb4ca, 0x98bb6c, 0xe6c384, 0xe46876, 0x2d4f67, 0x54546d]),
    theme([0x2d353b, 0xd3c6aa, 0x9da9a0, 0x7fbbb3, 0xd699b6, 0x83c092, 0xa7c080, 0xdbbc7f, 0xe67e80, 0x3d484d, 0x475258]),
    theme([0x1f2430, 0xcccac2, 0x8a9199, 0x73d0ff, 0xdfbfff, 0x95e6cb, 0xd5ff80, 0xffcc66, 0xf28779, 0x2d3445, 0x3a4150]),
];
/// Each theme's note (v7.7 spec, Themes), in [`PALETTES`] order.
pub const THEME_NOTES: [&str; 14] = [
    "today's look",
    "the terminal's own 16 colours; how they look depends on your terminal",
    "lighter ground",
    "soft pastels, darkest",
    "medium",
    "light; accents darkened",
    "warm",
    "light",
    "red lightened",
    "",
    "",
    "",
    "",
    "",
];
std::thread_local! {
    /// The theme this thread draws with (v7.7 ticket 154), an index into [`PALETTES`]. Per
    /// thread: the session draws on one, and tests drawing in parallel don't share it.
    static THEME: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// Draw with the theme named `name` from now on; an unknown name draws Tokyo Night.
pub fn set_theme(name: &str) {
    let index = crate::mappings::THEMES.iter().position(|t| *t == name);
    THEME.with(|t| t.set(index.unwrap_or(0)));
}
fn palette() -> Option<Palette> {
    PALETTES[THEME.with(std::cell::Cell::get)]
}
/// What every painted text goes back to: the theme's background and text, or (Terminal
/// colours) the terminal's own.
fn base() -> String {
    match palette() {
        Some(p) => {
            let [r, g, b] = p.background;
            let [tr, tg, tb] = p.text;
            format!("0;48;2;{r};{g};{b};38;2;{tr};{tg};{tb}")
        }
        None => "0".into(),
    }
}

/// What a piece of text is; [`style`] turns it into the palette's colours.
#[derive(Clone, Copy)]
enum Role {
    Border,
    Focus,
    /// The cursor row, filled with the accent.
    Selected,
    Pressed,
    Favourite,
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
    let Some(p) = palette() else {
        // Terminal colours: the same roles in the 16 ANSI colours (bright black for muted,
        // reverse video for the selection).
        return match role {
            Role::Border | Role::Muted => "90",
            Role::Focus => "1;34",
            Role::Selected => "1;30;44",
            Role::Pressed => "1;7",
            Role::Favourite => "1;33",
            Role::Heading => "1;35",
            Role::Title => "1;36",
            Role::Running => "32",
            Role::Paused => "33",
            Role::Error => "31",
            Role::BankA => "1;31",
            Role::BankB => "1;32",
            Role::ActiveTab => "1;4;30;44",
        }
        .into();
    };
    let fg = |[r, g, b]: [u8; 3]| format!("38;2;{r};{g};{b}");
    let bg = |[r, g, b]: [u8; 3]| format!("48;2;{r};{g};{b}");
    match role {
        Role::Border => fg(p.border),
        Role::Focus => format!("1;{}", fg(p.accent)),
        Role::Selected => format!("1;{};{}", fg(p.background), bg(p.accent)),
        Role::Pressed => format!("1;{};{}", fg(p.text), bg(p.selection)),
        Role::Favourite => format!("1;{}", fg(p.yellow)),
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
        let base = base();
        format!("\x1b[{base};{}m{text}\x1b[{base}m", style(role))
    } else {
        text.into()
    }
}

fn paint_program_row(text: &str, role: Role, color: bool) -> String {
    for (device, device_role) in [("Xbox", Role::Running), ("Akai MPK mini", Role::Title)] {
        if let Some(rest) = text
            .strip_prefix(device)
            .filter(|rest| rest.starts_with(" · "))
        {
            return format!(
                "{}{}",
                paint(device, device_role, color),
                paint(rest, role, color)
            );
        }
    }
    paint(text, role, color)
}

/// The frame's rows, each starting from the theme's background and text (v7.7 ticket 154),
/// so unpainted text and padding are on the theme's ground, not the terminal's.
fn finish(mut rows: Vec<String>, height: usize, color: bool) -> String {
    rows.truncate(height);
    if color {
        let base = format!("\x1b[{}m", base());
        for row in &mut rows {
            row.insert_str(0, &base);
        }
    }
    rows.join("\r\n")
}

/// A theme row's `■■■■■■` in that theme's accent, heading, title, green, yellow and red.
fn paint_swatch(text: &str, color: bool) -> String {
    const SWATCH: &str = "■■■■■■";
    let theme = (crate::mappings::THEMES.iter().enumerate())
        .filter(|(_, name)| text.contains(*name))
        .max_by_key(|(_, name)| name.len());
    let (Some((index, _)), true) = (theme, color && text.contains(SWATCH)) else {
        return text.into();
    };
    // Terminal colours sends no RGB code, not even another theme's swatch.
    if palette().is_none() && PALETTES[index].is_some() {
        return text.into();
    }
    let base = base();
    let codes: Vec<String> = match PALETTES[index] {
        Some(p) => [p.accent, p.heading, p.title, p.green, p.yellow, p.red]
            .iter()
            .map(|[r, g, b]| format!("38;2;{r};{g};{b}"))
            .collect(),
        None => ["34", "35", "36", "32", "33", "31"]
            .map(String::from)
            .to_vec(),
    };
    let squares = codes
        .iter()
        .map(|code| format!("\x1b[{base};{code}m■"))
        .collect::<String>();
    text.replace(SWATCH, &format!("{squares}\x1b[{base}m"))
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
        let text = if line.starts_with("> ") && focused {
            paint(&text, Role::Selected, color)
        } else if line.starts_with("◆ ") {
            paint(&text, Role::Pressed, color)
        } else if line.trim_start().starts_with("★ ") {
            paint(&text, Role::Favourite, color)
        } else if line.starts_with("━━ ") || line.trim_start().starts_with("⚠ ") {
            // Danger: a red rule and red rows (skill `destructive-emphasis`).
            paint(&text, Role::Error, color)
        } else if line.starts_with("── ") {
            paint(&text, Role::Heading, color)
        } else if line.starts_with("Error:") {
            paint(&text, Role::Error, color)
        } else {
            paint_swatch(&paint_bulbs(&text, color), color)
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
                    // v7.7 ticket 156: a badge (` ●N` red, ` !` yellow) keeps its mark.
                    match slot.find(" ●").or_else(|| slot.find(" !")) {
                        Some(at) => {
                            let (name, badge) = slot.split_at(at);
                            let role = if badge.contains('●') {
                                Role::Error
                            } else {
                                Role::Paused
                            };
                            format!("{name}{}", paint(badge, role, color))
                        }
                        None => slot,
                    }
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

    /// Inner Details row under a one-based terminal mouse coordinate.
    pub fn details_row_at(
        &self,
        (columns, height): (usize, usize),
        (column, row): (usize, usize),
    ) -> Option<usize> {
        if columns < 40 || height < 16 || !self.panes.is_empty() || self.focus_commands {
            return None;
        }
        let (header, _, [(commands_width, commands_height), (width, pane_height), _]) =
            self.sizes(columns, height);
        let (left, above) = if columns >= 100 {
            (commands_width, 0)
        } else {
            (0, commands_height)
        };
        let top = usize::from(!self.tabs.is_empty()) + header.len() + above;
        let row = row.checked_sub(top + 2)?;
        let text_height =
            pane_height.saturating_sub(2 + self.notice_rows(width.saturating_sub(4)).len());
        (column >= left + 3 && column < left + width.saturating_sub(1) && row < text_height)
            .then_some(row)
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
            paint_bulbs(&paint_program_row(&fit(row, width), role, color), color)
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
                paint_program_row(&fit(row, width), role, color)
            }));
            rows.extend(
                hints
                    .iter()
                    .map(|row| paint(&fit(row, width), Role::Muted, color)),
            );
            return finish(rows, height, color);
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
        finish(rows, height, color)
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

    /// v7.7 ticket 154: every RGB theme's text and muted text reach 4.5:1 on its background
    /// (WCAG relative luminance); Terminal colours draws with no RGB code at all.
    #[test]
    fn themes_are_readable_and_terminal_colours_uses_no_rgb() {
        fn luminance(c: [u8; 3]) -> f64 {
            let channel = |v: u8| {
                let v = f64::from(v) / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
        }
        let ratio = |a, b| {
            let (a, b) = (luminance(a), luminance(b));
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        };
        assert_eq!(PALETTES.len(), crate::mappings::THEMES.len());
        for (name, palette) in crate::mappings::THEMES.iter().zip(PALETTES) {
            let Some(p) = palette else {
                assert_eq!(*name, "Terminal colours");
                continue;
            };
            for (role, colour) in [("text", p.text), ("muted", p.muted)] {
                let r = ratio(colour, p.background);
                assert!(r >= 4.5, "{name} {role}: {r:.2}:1");
            }
        }
        let screen = Screen {
            tabs: vec!["1 Live".into(), "8 System".into()],
            groups: vec![1],
            active_tab: 1,
            header: "VibeConsole\nProgram 1 | RUNNING".into(),
            commands: Vec::new(),
            anchor: 0,
            focus_commands: false,
            details: String::new(),
            notice: String::new(),
            live: Vec::new(),
            hints: "Ctrl+C: quit".into(),
            panes: vec![Pane {
                title: "Theme".into(),
                rows: vec![
                    "> ● Tokyo Night           ■■■■■■".into(),
                    "    Nord ■■■■■■".into(),
                ],
                anchor: 0,
                focused: true,
            }],
        };
        set_theme("Terminal colours");
        let frame = screen.render(100, 24, true);
        set_theme("Tokyo Night");
        assert!(frame.contains('\x1b'), "still coloured");
        assert!(
            !frame.contains("38;2;") && !frame.contains("48;2;"),
            "{frame:?}"
        );
        // RGB themes paint their own ground on every row, and the swatch in its colours.
        set_theme("Catppuccin Latte");
        let latte = screen.render(100, 24, true);
        set_theme("Tokyo Night");
        assert!(
            latte
                .lines()
                .all(|l| l.starts_with("\x1b[0;48;2;239;241;245;38;2;76;79;105m"))
        );
        assert!(
            latte.contains(";38;2;136;192;208m■"),
            "Nord's accent in Nord's swatch"
        );
        set_theme("no such theme");
        assert_eq!(
            palette(),
            Some(TOKYO_NIGHT),
            "an unknown name draws Tokyo Night"
        );
    }

    /// v7.7 ticket 151: a tab's panes stack full width, the selected row stays visible as
    /// panes shrink, the pressed line has its own highlight, and the notice keeps its
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
        let lit = format!("{}m◆ Pad 5", style(Role::Pressed));
        assert!(
            colored.contains(&lit),
            "the pressed line has its own highlight"
        );
        assert!(colored.contains(&format!("{}mIDLE", style(Role::Muted))));
    }

    #[test]
    fn device_names_and_favourites_keep_distinct_styles() {
        set_theme("Tokyo Night");
        let xbox = "Xbox · Apps";
        let akai = "Akai MPK mini · Prompts";
        assert!(paint_program_row(xbox, Role::Paused, true).contains(";38;2;158;206;106mXbox"));
        assert!(
            paint_program_row(akai, Role::Paused, true).contains(";38;2;125;207;255mAkai MPK mini")
        );
        assert_eq!(paint_program_row(akai, Role::Paused, false), akai);

        let rows = vec![
            "> Output mute".into(),
            "  ★ Next track".into(),
            "  Previous track".into(),
        ];
        let frame = pane("Actions", &rows, 0, (50, 5), true, true);
        assert!(frame[1].contains(";48;2;122;162;247m> Output mute"));
        assert!(frame[2].contains(";38;2;224;175;104m  ★ Next track"));
        assert!(!frame[3].contains(";38;2;224;175;104m"));
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
        assert!(colored.contains("1;4;38;2;26;27;38;48;2;122;162;247m[3 Agents]"));
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
