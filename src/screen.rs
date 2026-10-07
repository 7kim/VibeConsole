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

pub fn light_columns(width: usize) -> usize {
    (width / 20).clamp(1, 4)
}

pub fn lights(items: &[(String, bool)], width: usize) -> Vec<String> {
    let columns = light_columns(width);
    let cell = width / columns;
    let mut rows = Vec::new();
    for group in items.chunks(columns) {
        let labels = group
            .iter()
            .map(|(label, _)| label.lines().count())
            .max()
            .unwrap_or(0);
        for row in 0..=labels {
            let mut line = String::new();
            for (label, on) in group {
                let text = if row == 0 {
                    bulb(*on)
                } else {
                    label.lines().nth(row - 1).unwrap_or("")
                };
                let used = cells(text).iter().map(|(_, width)| width).sum::<usize>();
                let padding = cell.saturating_sub(used) / 2;
                line.push_str(&fit(&format!("{}{text}", " ".repeat(padding)), cell));
            }
            rows.push(line);
        }
    }
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

fn paint(text: &str, style: &str, color: bool) -> String {
    if color {
        format!("\x1b[{style}m{text}\x1b[0m")
    } else {
        text.into()
    }
}

fn paint_bulbs(text: &str, color: bool) -> String {
    text.replace(bulb(true), &paint(bulb(true), "1;33", color))
        .replace(bulb(false), &paint(bulb(false), "2", color))
}

pub fn frame(
    width: usize,
    height: usize,
    header: &str,
    body: &str,
    footer: &str,
    color: bool,
) -> String {
    let width = width.saturating_sub(1).max(1); // Leave the last column to avoid autowrap.
    if width < 8 || height < 8 {
        let rows = wrap("Resize terminal · Esc back · Ctrl+C quit", width);
        return (0..height)
            .map(|index| fit(rows.get(index).map(String::as_str).unwrap_or(""), width))
            .collect::<Vec<_>>()
            .join("\r\n");
    }
    let mut rows = Vec::new();
    for (index, row) in header.lines().take(3).enumerate() {
        let style = match index {
            0 => "1;36",
            1 => "2",
            _ if row.starts_with("RUNNING") => "32",
            _ => "33",
        };
        rows.push(paint_bulbs(&paint(&fit(row, width), style, color), color));
    }
    let border = "─".repeat(width.saturating_sub(2));
    rows.push(paint(&format!("╭{border}╮"), "2", color));
    let content_height = height.saturating_sub(8);
    let content = wrap(body, width.saturating_sub(4));
    for index in 0..content_height {
        let line = content.get(index).map(String::as_str).unwrap_or("");
        let line = fit(line, width.saturating_sub(4));
        let line = if let Some(start) = line.find("> ") {
            let end = line[start..]
                .find('│')
                .map(|end| start + end)
                .unwrap_or(line.len());
            format!(
                "{}{}{}",
                &line[..start],
                paint(&line[start..end], "1;30;46", color),
                &line[end..]
            )
        } else {
            line
        };
        rows.push(format!("│ {} │", paint_bulbs(&line, color)));
    }
    rows.push(paint(&format!("╰{border}╯"), "2", color));
    for (index, row) in footer.lines().take(3).enumerate() {
        rows.push(paint(
            &fit(row, width),
            if index == 1 && row.starts_with("Error:") {
                "31"
            } else if index == 1 && !row.is_empty() {
                "33"
            } else {
                "2"
            },
            color,
        ));
    }
    rows.truncate(height);
    rows.join("\r\n")
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
        let cards = lights(
            &[
                ("Pad 1\nShift".into(), true),
                ("Pad 2\nCtrl+K".into(), false),
            ],
            40,
        );
        assert!(cards[0].contains(bulb(true)) && cards[0].contains(bulb(false)));
        assert!(cards[1].contains("Pad 1") && cards[2].contains("Shift"));
        for (width, height) in [(100, 24), (50, 18)] {
            let frame = frame(
                width,
                height,
                "KeyAI\nProgram 1\nRUNNING",
                "> Learn a pad\n界界界",
                "Active: none\n\nEnter: confirm",
                false,
            );
            assert_eq!(frame.lines().count(), height);
            assert!(!frame.contains('\x1b'));
            assert!(
                frame
                    .lines()
                    .all(|line| cells(line).iter().map(|(_, w)| w).sum::<usize>() < width)
            );
        }
    }
}
