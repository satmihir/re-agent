use crate::cli::{number, text, yes};
use regex::Regex;
use serde_json::Value;
use std::{sync::LazyLock, time::Duration};
#[path = "cli/activity.rs"]
mod activity;
pub use activity::{
    begin_turn, progress_text, progress_text_in_region, tool_finished, tool_finished_in_region,
};

pub(crate) mod palette;
use palette::{
    ACCENT, BOLD, DIM, ERROR, EXEC, ITALIC, RESET, SUCCESS, UNDERLINE, UNDERLINE_OFF, WARNING,
};

const LINK_CLOSE: &str = "\x1b]8;;\x1b\\";

/// Sanitize makes terminal controls visible without altering newlines or tabs.
pub fn sanitize(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let n = c as u32;
        if c != '\n' && c != '\t' && (n < 32 || n == 127 || (128..=159).contains(&n)) {
            out.push_str(&format!("\\x{n:02x}"))
        } else {
            out.push(c)
        }
    }
    out
}
/// Styled reports whether a file descriptor is a terminal requesting color.
pub fn styled(fd: i32) -> bool {
    std::env::var("NO_COLOR").unwrap_or_default().is_empty() && unsafe { libc::isatty(fd) == 1 }
}
/// Columns returns terminal width, or zero for a non-terminal descriptor.
pub fn columns(fd: i32) -> usize {
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) == 0 {
            size.ws_col as usize
        } else {
            0
        }
    }
}
/// Display sanitizes model text and optionally renders Markdown.
pub fn display(text: &str, styled: bool, columns: usize) -> String {
    let safe = sanitize(text);
    if styled {
        markdown(&safe, columns)
    } else {
        safe
    }
}
fn spans(text: &str) -> String {
    static LINKS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\(([^)\s\x00-\x1f\x7f-\x9f]+)\)").unwrap());
    let parts: Vec<_> = text.split('`').collect();
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        if i % 2 == 1 && i != parts.len() - 1 {
            out.push_str(&format!("{ACCENT}{p}{RESET}"));
            continue;
        }
        // Keep opaque destinations out of emphasis matching, including bold around a link.
        let mut links = Vec::new();
        let marked = LINKS.replace_all(p, |link: &regex::Captures<'_>| {
            links.push(format!(
                "\x1b]8;;{}\x1b\\{UNDERLINE}{}{UNDERLINE_OFF}{LINK_CLOSE}",
                &link[2],
                emphasis(&link[1]).replace(RESET, &format!("{RESET}{UNDERLINE}"))
            ));
            format!("\x1b]8;;{}\x1b\\", links.len() - 1)
        });
        let mut styled = emphasis(&marked);
        for (i, link) in links.iter().enumerate() {
            styled = styled.replace(&format!("\x1b]8;;{i}\x1b\\"), link);
        }
        out.push_str(&styled);
    }
    out
}
fn emphasis(text: &str) -> String {
    static BOLD_SPAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*\*([^*]+)\*\*").unwrap());
    static ITALIC_SPAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*([^*]+)\*").unwrap());
    let bold = BOLD_SPAN.replace_all(text, format!("{BOLD}${{1}}{RESET}"));
    ITALIC_SPAN
        .replace_all(&bold, format!("{ITALIC}${{1}}{RESET}"))
        .into_owned()
}

fn line(text: &str) -> (String, String, String) {
    static RULE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(-{3,}|\*{3,}|_{3,})$").unwrap());
    static BULLET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-*+][ \t]+").unwrap());
    static NUMBERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+[.)][ \t]+").unwrap());
    let content = text.trim_start_matches([' ', '\t']);
    let indent = &text[..text.len() - content.len()];
    let level = content.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&level) && content.as_bytes().get(level) == Some(&b' ') {
        return (
            indent.into(),
            indent.into(),
            format!(
                "{BOLD}{ACCENT}{}{RESET}",
                spans(content[level..].trim()).replace(RESET, &format!("{RESET}{BOLD}{ACCENT}"))
            ),
        );
    }
    if RULE.is_match(content) {
        return (
            format!("{indent}{DIM}{}{RESET}", "─".repeat(32)),
            String::new(),
            String::new(),
        );
    }
    if let Some(m) = BULLET.find(content) {
        return (
            format!("{indent}{DIM}•{RESET} "),
            format!("{indent}  "),
            spans(&content[m.end()..]),
        );
    }
    if let Some(m) = NUMBERED.find(content) {
        let n = m.as_str().trim();
        return (
            format!("{indent}{DIM}{n}{RESET} "),
            format!("{indent}{}", " ".repeat(width(n) + 1)),
            spans(&content[m.end()..]),
        );
    }
    if let Some(quote) = content.strip_prefix('>') {
        let bar = format!("{indent}{DIM}│ {RESET}");
        return (bar.clone(), bar, spans(quote.trim()));
    }
    (indent.into(), indent.into(), spans(content))
}
/// Wrap preserves styled words while fitting subsequent rows to a hanging prefix.
pub(crate) fn wrap(first: &str, hang: &str, body: &str, columns: usize) -> Vec<String> {
    if columns == 0 || width(&format!("{first}{body}")) <= columns {
        return vec![format!("{first}{body}")];
    }
    let mut rows = Vec::new();
    let mut row = first.to_string();
    let mut cells = width(first);
    let mut words = 0;
    let mut open = String::new();
    let mut link = String::new();
    for word in body.split(' ') {
        if word.is_empty() && words == 0 && !rows.is_empty() {
            continue;
        }
        let w = width(word);
        if words > 0 && cells + 1 + w > columns {
            if !link.is_empty() {
                row.push_str(LINK_CLOSE);
            }
            if !open.is_empty() {
                row.push_str(RESET)
            }
            rows.push(row);
            row = format!("{hang}{open}{link}");
            cells = width(hang);
            words = 0
        }
        if words > 0 {
            row.push(' ');
            cells += 1
        }
        row.push_str(word);
        cells += w;
        words += 1;
        let mut rest = word;
        while !rest.is_empty() {
            if let Some(seq) = terminal_sequence(rest) {
                if seq.starts_with("\x1b]8;;") {
                    link = if seq == LINK_CLOSE {
                        String::new()
                    } else {
                        seq.into()
                    };
                } else if seq == RESET {
                    open.clear();
                } else if seq.ends_with('m') {
                    open.push_str(seq);
                }
                rest = &rest[seq.len()..];
                continue;
            }
            rest = &rest[rest.chars().next().unwrap().len_utf8()..];
        }
    }
    rows.push(row);
    rows
}
fn table_cells(row: &str) -> Vec<String> {
    let row = row.trim();
    let row = row.strip_prefix('|').unwrap_or(row);
    let row = if row.ends_with('|') && !row.ends_with("\\|") {
        &row[..row.len() - 1]
    } else {
        row
    };
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = row.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            chars.next();
            cell.push('|')
        } else if c == '|' {
            cells.push(cell.trim().into());
            cell.clear()
        } else {
            cell.push(c)
        }
    }
    cells.push(cell.trim().into());
    cells
}
fn table(lines: &[&str], columns: usize) -> Vec<String> {
    let indent = &lines[0][..lines[0].len() - lines[0].trim_start_matches([' ', '\t']).len()];
    let align = table_cells(lines[1]);
    let count = align.len();
    let mut widths = vec![0; count];
    let mut rows = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i == 1 {
            continue;
        }
        let cells = table_cells(line);
        let mut row = vec![String::new(); count];
        for c in 0..count {
            if let Some(cell) = cells.get(c) {
                row[c] = spans(cell)
            }
            widths[c] = widths[c].max(width(&row[c]));
        }
        rows.push(row)
    }
    let total = width(indent) + 3 * count.saturating_sub(1) + widths.iter().sum::<usize>();
    if columns > 0 && total > columns {
        return lines.iter().map(|l| spans(l)).collect();
    }
    let mut out = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let mut cells = Vec::new();
        for c in 0..count {
            let mut cell = row[c].clone();
            if r == 0 {
                cell = format!(
                    "{BOLD}{}{RESET}",
                    cell.replace(RESET, &format!("{RESET}{BOLD}"))
                )
            }
            let pad = widths[c] - width(&cell);
            let (left, right) = if align[c].starts_with(':') && align[c].ends_with(':') {
                (pad / 2, pad - pad / 2)
            } else if align[c].ends_with(':') {
                (pad, 0)
            } else {
                (0, pad)
            };
            cells.push(format!("{}{cell}{}", " ".repeat(left), " ".repeat(right)))
        }
        out.push(
            format!("{indent}{}", cells.join(&format!(" {DIM}│{RESET} ")))
                .trim_end_matches(' ')
                .into(),
        );
        if r == 0 {
            out.push(format!(
                "{indent}{DIM}{}{RESET}",
                widths
                    .iter()
                    .map(|n| "─".repeat(*n))
                    .collect::<Vec<_>>()
                    .join("─┼─")
            ))
        }
    }
    out
}
fn code_rows(text: &str, indent: &str, color: &str, columns: usize) -> Vec<String> {
    let prefix = format!("{indent}{DIM}│{RESET} ");
    let room = if columns == 0 {
        usize::MAX
    } else {
        columns.saturating_sub(width(&prefix)).max(1)
    };
    let mut rows = Vec::new();
    let mut body = String::new();
    let mut cells = 0;
    for c in text.replace('\t', "    ").chars() {
        let w = rune_width(c);
        if cells > 0 && cells + w > room {
            rows.push(format!("{prefix}{color}{body}{RESET}"));
            body.clear();
            cells = 0;
        }
        body.push(c);
        cells += w;
    }
    rows.push(format!("{prefix}{color}{body}{RESET}"));
    rows
}

/// Markdown renders terminal headings, framed code blocks, hyperlinks and tables.
pub fn markdown(text: &str, columns: usize) -> String {
    static TABLE_SEPARATOR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$").unwrap());
    let lines: Vec<_> = text.split('\n').collect();
    let mut rendered = Vec::new();
    let mut fence = None;
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let trimmed = l.trim();
        let ticks = trimmed.bytes().take_while(|b| *b == b'`').count();
        if let Some((length, language, indent)) = fence {
            if ticks >= length && trimmed[ticks..].is_empty() {
                fence = None;
            } else {
                let color = match language {
                    "diff" if l.starts_with('+') => SUCCESS,
                    "diff" if l.starts_with('-') => ERROR,
                    "diff" => "",
                    _ => ACCENT,
                };
                rendered.extend(code_rows(l, indent, color, columns));
            }
            i += 1;
            continue;
        }
        if ticks >= 3 {
            let language = trimmed[ticks..].trim();
            let indent = &l[..l.len() - l.trim_start().len()];
            fence = Some((ticks, language, indent));
            if !language.is_empty() {
                let room = columns.saturating_sub(width(indent) + 2);
                let label = if columns > 0 && room == 0 {
                    String::new()
                } else {
                    truncate(language, room)
                };
                rendered.push(format!("{indent}{DIM}│ {label}{RESET}"));
            }
            i += 1;
            continue;
        }
        let h = l.trim();
        if h.starts_with('|')
            && h.ends_with('|')
            && h.len() >= 2
            && i + 1 < lines.len()
            && TABLE_SEPARATOR.is_match(lines[i + 1])
            && table_cells(l).len() == table_cells(lines[i + 1]).len()
        {
            let mut end = i + 2;
            while end < lines.len() && lines[end].trim().starts_with('|') {
                end += 1
            }
            rendered.extend(table(&lines[i..end], columns));
            i = end;
            continue;
        }
        let (first, hang, body) = line(l);
        rendered.extend(wrap(&first, &hang, &body, columns));
        i += 1;
    }
    rendered.join("\n")
}
/// Rune width approximates East Asian terminal cell widths.
pub(crate) fn rune_width(c: char) -> usize {
    let n = c as u32;
    static ZERO_WIDTH: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[\p{Mn}\p{Me}\p{Cf}]$").unwrap());
    if ZERO_WIDTH.is_match(&c.to_string()) {
        return 0;
    }
    if n >= 0x1100
        && (n <= 0x115f
            || n == 0x2329
            || n == 0x232a
            || (0x2e80..=0xa4cf).contains(&n)
            || (0xac00..=0xd7a3).contains(&n)
            || (0xf900..=0xfaff).contains(&n)
            || (0xfe10..=0xfe19).contains(&n)
            || (0xfe30..=0xfe6f).contains(&n)
            || (0xff00..=0xff60).contains(&n)
            || (0xffe0..=0xffe6).contains(&n)
            || (0x1f300..=0x1f64f).contains(&n)
            || (0x1f900..=0x1f9ff).contains(&n)
            || (0x20000..=0x3fffd).contains(&n))
    {
        2
    } else {
        1
    }
}
fn terminal_sequence(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    if bytes.starts_with(b"\x1b[") {
        let end = bytes[2..].iter().position(|b| (b'@'..=b'~').contains(b))? + 3;
        return Some(&text[..end]);
    }
    if bytes.starts_with(b"\x1b]") {
        for i in 2..bytes.len() {
            if bytes[i] == 7 {
                return Some(&text[..i + 1]);
            }
            if bytes[i..].starts_with(b"\x1b\\") {
                return Some(&text[..i + 2]);
            }
        }
    }
    None
}

/// Width ignores terminal styles and hyperlinks while measuring terminal cells.
pub fn width(mut text: &str) -> usize {
    let mut n = 0;
    while !text.is_empty() {
        if let Some(seq) = terminal_sequence(text) {
            text = &text[seq.len()..];
            continue;
        }
        let c = text.chars().next().unwrap();
        text = &text[c.len_utf8()..];
        n += rune_width(c);
    }
    n
}
/// Truncate shortens text to cells and appends an ellipsis if necessary.
pub fn truncate(text: &str, cells: usize) -> String {
    if cells == 0 || width(text) <= cells {
        return text.into();
    }
    let mut out = String::new();
    let mut n = 0;
    let mut rest = text;
    let mut linked = false;
    let mut styled = false;
    while !rest.is_empty() {
        if let Some(seq) = terminal_sequence(rest) {
            linked = if seq.starts_with("\x1b]8;;") {
                seq != LINK_CLOSE
            } else {
                linked
            };
            if seq.starts_with("\x1b[") && seq.ends_with('m') {
                styled = seq != RESET;
            }
            out.push_str(seq);
            rest = &rest[seq.len()..];
            continue;
        }
        let c = rest.chars().next().unwrap();
        if n + rune_width(c) > cells - 1 {
            break;
        }
        rest = &rest[c.len_utf8()..];
        n += rune_width(c);
        out.push(c)
    }
    out.push('…');
    if linked {
        out.push_str(LINK_CLOSE);
    }
    if styled {
        out.push_str(RESET);
    }
    out
}
/// Count formats token totals as compact decimal counts.
pub fn count(n: i64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 999950 {
        format!("{:.1}k", n as f64 / 1000.)
    } else {
        format!("{:.1}M", n as f64 / 1e6)
    }
}
/// Router usage line keeps router accounting separate from generation tokens.
pub(crate) fn router_usage_line(usage: &Value) -> String {
    if !yes(usage, "known") {
        return "router     Jev usage unknown; separate from generation".into();
    }
    format!(
        "router     Jev {} tokens in · {} out; separate from generation",
        count(number(usage, "input_tokens")),
        count(number(usage, "output_tokens"))
    )
}

fn router_usage_note(usage: &Value, styled: bool) -> String {
    if usage.is_null() {
        return String::new();
    }
    let line = format!("  · {}", router_usage_line(usage));
    if styled {
        format!("{DIM}{line}{RESET}\n")
    } else {
        format!("{line}\n")
    }
}

/// Plural formats a count and its singular or plural label.
pub fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
/// Short path substitutes the current user's home directory prefix.
pub fn short_path(path: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if path == home || path.starts_with(&(home.clone() + "/")) {
            return format!("~{}", &path[home.len()..]);
        }
    }
    path.into()
}
/// Model presentation returns model, provider, and effort labels.
pub fn model_presentation(cfg: &Value) -> (String, String, String) {
    if text(cfg, "provider") == "scripted" {
        return ("scripted".into(), String::new(), String::new());
    }
    let mut provider = text(cfg, "provider").to_string();
    let auth = match text(cfg, "auth_mode") {
        "chatgpt" => "ChatGPT plan",
        "key" => "API key",
        "proxy" => "API proxy",
        _ => "",
    };
    if !auth.is_empty() {
        provider.push_str(" · ");
        provider.push_str(auth);
    }
    let effort = text(cfg, "reasoning_effort");
    let mut effort = if effort.is_empty() {
        "default effort".into()
    } else {
        format!("effort {effort}")
    };
    let window = number(cfg, "context_window_tokens");
    if window > 0 {
        let cap = crate::catalog::model_info(text(cfg, "model"))["context_window"]
            .as_i64()
            .unwrap_or(window)
            .min(window);
        effort.push_str(&format!(" · window {} override", window_label(cap)))
    }
    (text(cfg, "model").into(), provider, effort)
}
/// Window label formats a token capacity without redundant decimal zeroes.
pub(crate) fn window_label(tokens: i64) -> String {
    if tokens < 1_000_000 {
        return count(tokens).replacen(".0k", "k", 1);
    }
    format!("{:.2}", tokens as f64 / 1_000_000.0)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
        + "M"
}
/// Mode labels the configured authority.
pub fn mode(cfg: &Value) -> &'static str {
    if yes(cfg, "read_only") {
        "read only"
    } else {
        "read, write, and execute"
    }
}
fn header_line(line: &str) {
    if styled(2) {
        eprintln!("{DIM}{line}{RESET}")
    } else {
        eprintln!("{line}")
    }
}
fn welcome(cfg: &Value, columns: usize, styled: bool) -> String {
    let illustrated = styled && columns >= 72;
    let room = columns.saturating_sub(if illustrated { 17 } else { 1 });
    let safe = |s: &str| sanitize(s).replace('\n', "↵").replace('\t', "⇥");
    let (model, provider, effort) = model_presentation(cfg);
    let mut rows = vec![if provider.is_empty() {
        safe(&model)
    } else {
        format!("{} · {} · {}", safe(&model), safe(&provider), safe(&effort))
    }];
    if illustrated {
        rows.insert(0, "re:agent".into());
        rows.insert(0, String::new());
    } else {
        rows[0].insert_str(0, "re:agent · ");
    }
    let workspace = safe(&short_path(text(cfg, "workspace")));
    rows.push(truncate(&format!("workspace {workspace}"), room));
    rows.push(if yes(cfg, "plan") {
        format!("{} · plan mode: exec and file changes refused", mode(cfg))
    } else if yes(cfg, "read_only") {
        "read only".into()
    } else {
        "! exec: commands run as you; read, write, and network".into()
    });
    if yes(cfg, "fast") {
        rows.push("fast mode on (higher token rates)".into());
    }
    if yes(cfg, "proxied") {
        rows.push(format!(
            "requests go to {} (API_PROXY_URL)",
            safe(&crate::transport::redacted(
                &std::env::var("API_PROXY_URL").unwrap_or_default()
            ))
        ));
    }
    let id = safe(text(cfg, "session_id"));
    if yes(cfg, "checkpoint_enabled") && !id.is_empty() {
        let resume = format!("session ID: {id} · resume: reagent chat --resume {id}");
        rows.push(if room == 0 || width(&resume) <= room {
            resume
        } else {
            format!("session ID: {id} · resume: --resume {id}")
        });
    }
    rows.push("/help commands · Ctrl-D exit".into());
    let art = [
        "      ●      ",
        "  ▗▄▖   ▗▄▖  ",
        "   █  B  █   ",
        "   █ P   █   ",
        "  ▟▘     ▝▙  ",
        " ▟▘LLLLLLL▝▙ ",
        "▐▌LLLL>LLLL▐▌",
        "▝▙▄▄▄▄▄▄▄▄▄▟▘",
    ];
    let mut details = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        // Paths are shortened, not wrapped; disclosures retain their complete text.
        let lines = if index == if illustrated { 3 } else { 1 } {
            vec![row.clone()]
        } else {
            wrap("", "", row, room)
        };
        for line in lines {
            details.push(if illustrated && index == 1 {
                format!("{BOLD}re{EXEC}:{RESET}{BOLD}agent{RESET}")
            } else if illustrated && index == 2 {
                format!("{BOLD}{line}{RESET}")
            } else if styled {
                format!("{DIM}{line}{RESET}").replace(
                    "plan mode",
                    &format!("{RESET}{BOLD}{ACCENT}plan mode{RESET}{DIM}"),
                )
            } else {
                line
            });
        }
    }
    let mut out = String::new();
    for index in 0..details.len().max(if illustrated { art.len() } else { 0 }) {
        if illustrated {
            for (col, c) in art
                .get(index)
                .unwrap_or(&"             ")
                .chars()
                .enumerate()
            {
                match c {
                    '●' | 'P' => out.push_str(&format!(
                        "{EXEC}{}{RESET}",
                        if c == '●' { '●' } else { '•' }
                    )),
                    'B' => out.push_str(&format!("{EXEC}•{RESET}")),
                    'L' => out.push_str(&format!("{} {RESET}", palette::FLASK_LIQUID[col])),
                    '>' => out.push_str(&format!(
                        "{}{BOLD}{ACCENT}❯{RESET}",
                        palette::FLASK_LIQUID[col]
                    )),
                    _ => out.push(c),
                }
            }
            out.push_str("   ");
        }
        if let Some(detail) = details.get(index) {
            out.push_str(detail);
        }
        out.push('\n');
    }
    out
}

/// Header presents a run's model, workspace, and authority.
pub fn header(cfg: &Value, chat: bool) {
    if chat {
        eprint!("{}", welcome(cfg, columns(2), styled(2)));
        return;
    }
    let (model, provider, effort) = model_presentation(cfg);
    let details = if provider.is_empty() {
        String::new()
    } else {
        format!(" ({provider}, {effort})")
    };
    let workspace = short_path(text(cfg, "workspace"));
    let mut mode = mode(cfg).to_string();
    if yes(cfg, "plan") {
        mode.push_str(" · plan mode")
    }
    if yes(cfg, "fast") {
        mode.push_str(" · fast (higher token rates)");
    }
    let prefix = "re:agent · ";
    let suffix = format!("{details} · {mode} · {workspace}");
    if styled(2) {
        eprintln!("{DIM}{prefix}{RESET}{BOLD}{model}{RESET}{DIM}{suffix}{RESET}")
    } else {
        eprintln!("{prefix}{model}{suffix}")
    }
    if yes(cfg, "proxied") {
        header_line(&format!(
            "requests go to {} (API_PROXY_URL)",
            crate::transport::redacted(&std::env::var("API_PROXY_URL").unwrap_or_default())
        ))
    }
    if yes(cfg, "plan") {
        header_line("! plan mode: exec and file changes are refused while plan mode is on")
    } else if !yes(cfg, "read_only") {
        header_line(&format!("! exec mode: commands run as you, in {workspace}, and can read, write, and use the network"))
    }
}
/// Model started shows the active request without committing a transcript line.
pub fn model_started(
    region: Option<&crate::terminal::RegionHandle>,
    model: &str,
    step: i64,
    max_steps: i64,
) {
    let mut label = format!("thinking · {} · step {step}", sanitize(model));
    if max_steps > 0 {
        label.push_str(&format!(" of {max_steps}"));
    }
    if let Some(region) = region {
        let _ = region.lock().unwrap().set_progress(Some(&label));
    }
}

/// Model finished removes the active operation from the input pane.
pub fn model_finished(region: Option<&crate::terminal::RegionHandle>) {
    if let Some(region) = region {
        let _ = region.lock().unwrap().set_progress(None);
    }
}

/// Agents refreshed updates working counts and wait progress, then prints drained notices once.
pub fn agents_refreshed(
    region: Option<&crate::terminal::RegionHandle>,
    count: usize,
    wait_label: Option<&str>,
    notices: &[String],
    rows: &[crate::agents::AgentRow],
) {
    if let Some(region) = region {
        let label = wait_label.map(|label| sanitize(label).replace('\n', "↵").replace('\t', "⇥"));
        let _ = region
            .lock()
            .unwrap()
            .refresh_agents(count, label.as_deref(), rows);
    }
    for notice in notices {
        let notice = sanitize(notice).replace('\n', "↵").replace('\t', "⇥");
        let notice = if region.is_some() {
            truncate(&notice, columns(2).saturating_sub(5))
        } else {
            notice
        };
        crate::terminal::output(region, 2, &format!("  ↳ {notice}\n"));
    }
}

/// Tool started shows transient progress without adding browsing transcript lines.
pub fn tool_started(region: Option<&crate::terminal::RegionHandle>, call: &Value) {
    if let (Some(region), Some(label)) = (region, activity::progress_label(call)) {
        let _ = region.lock().unwrap().set_progress(Some(&label));
    }
}

fn retry_delay(milliseconds: i64) -> String {
    if milliseconds == 0 {
        return "0s".into();
    }
    let magnitude = i128::from(milliseconds).abs();
    if magnitude < 1000 {
        return format!("{milliseconds}ms");
    }
    let seconds = format!("{}.{:03}", magnitude / 1000 % 60, magnitude % 1000)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned();
    let hours = magnitude / 3_600_000;
    let minutes = magnitude / 60_000 % 60;
    let prefix = if hours > 0 {
        format!("{hours}h{minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        String::new()
    };
    format!(
        "{}{prefix}{seconds}s",
        if milliseconds < 0 { "-" } else { "" }
    )
}
/// Retry scheduled records the failed attempt and shows its backoff status.
pub fn retry_scheduled(
    region: Option<&crate::terminal::RegionHandle>,
    attempt: i64,
    delay_ms: i64,
    reason: &str,
) {
    let delay = retry_delay(delay_ms);
    let reason = sanitize(reason).replace('\n', "\n    ");
    let note = format!("  · model attempt {attempt} failed ({reason}); retrying in {delay}");
    let note = if styled(2) {
        format!("{DIM}{note}{RESET}\n")
    } else {
        format!("{note}\n")
    };
    crate::terminal::output(region, 2, &note);
    if let Some(region) = region {
        let label = format!(
            "model unavailable · retry {} in {}s",
            attempt + 1,
            (delay_ms + 500) / 1000
        );
        let _ = region.lock().unwrap().set_progress(Some(&label));
    }
}

/// Retry started shows the request following a backoff wait.
pub fn retry_started(region: Option<&crate::terminal::RegionHandle>, attempt: i64) {
    if let Some(region) = region {
        let label = format!("thinking · model · attempt {attempt}");
        let _ = region.lock().unwrap().set_progress(Some(&label));
    }
}

/// Usage renders a usage record for summaries and status commands.
pub fn usage(u: &Value) -> String {
    let mut out = if yes(u, "known") {
        format!(
            "{} in ({} cached) · {} out",
            count(number(u, "input_tokens")),
            count(number(u, "cached_input_tokens")),
            count(number(u, "output_tokens"))
        )
    } else {
        "tokens unknown".into()
    };
    let unreported = number(u, "unreported_attempts");
    if unreported > 0 {
        out.push_str(&format!(
            " · {}",
            plural(unreported, "unreported retry", "unreported retries")
        ))
    }
    out
}
/// Plan block finds the last completed exact-line block unless a later opening is unclosed.
pub(crate) fn plan_block(reply: &str) -> Option<(usize, usize)> {
    let mut open = None;
    let mut found = None;
    let mut offset = 0;
    for line in reply.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        match line {
            "<plan>" => {
                open = Some(offset);
                found = None;
            }
            "</plan>" => {
                if let Some(start) = open.take() {
                    found = Some((start, offset + line.len()));
                }
            }
            _ => {}
        }
        offset += line.len() + 1;
    }
    if open.is_some() {
        None
    } else {
        found
    }
}

fn reply_text(reply: &str, show_plan: bool, styled: bool, columns: usize) -> String {
    if show_plan {
        if let Some((start, end)) = plan_block(reply) {
            let content = &reply[start + "<plan>\n".len()..end - "</plan>".len()];
            let content = content.strip_suffix('\n').unwrap_or(content);
            let label = if styled {
                format!("{DIM}plan{RESET}")
            } else {
                "plan".into()
            };
            return format!(
                "{}{label}\n{}{}",
                display(&reply[..start], styled, columns),
                display(content, styled, columns),
                display(&reply[end..], styled, columns)
            );
        }
    }
    display(reply, styled, columns)
}

/// Blocked advice identifies the local command needed before another turn.
pub fn blocked_advice(status: &str) -> &'static str {
    if status == "limit_exceeded" {
        "/reset to continue; /compact works before the window fills (watch the context percentage)"
    } else {
        "/compact or /reset to continue"
    }
}

/// Run_details retains detailed last-Run accounting for the chat status command.
pub(crate) fn run_details(result: &Value) -> String {
    let mut out = format!(
        "last run   {} · {} · {} · {}",
        text(result, "status"),
        plural(number(result, "steps"), "step", "steps"),
        plural(number(result, "tool_calls"), "tool call", "tool calls"),
        usage(&result["usage"])
    );
    if !result["tree_cost"].is_null() {
        out.push_str(&format!("\ntree cost  {}", usage(&result["tree_cost"])));
    }
    if !result["router_usage"].is_null() {
        out.push_str(&format!("\n{}", router_usage_line(&result["router_usage"])));
    }
    out
}

fn token_total(usage: &Value) -> String {
    let total = number(usage, "input_tokens").checked_add(number(usage, "output_tokens"));
    let mut out = match (yes(usage, "known"), total) {
        (true, Some(total)) => format!("{} tokens", count(total)),
        _ => "tokens unknown".into(),
    };
    let unreported = number(usage, "unreported_attempts");
    if unreported > 0 {
        out.push_str(&format!(
            " (incomplete; {})",
            plural(unreported, "unreported retry", "unreported retries")
        ));
    }
    out
}

fn turn_summary(
    result: &Value,
    elapsed: Duration,
    state: &Value,
    chat: bool,
    styled: bool,
) -> String {
    let status = text(result, "status");
    let elapsed = if elapsed.as_secs() < 60 {
        format!("{:.1}s", elapsed.as_secs_f64())
    } else {
        format!("{}m{:02}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
    };
    if chat && status == "completed" {
        let mut line = format!(
            "✓ {elapsed} · {} · {}",
            plural(number(result, "tool_calls"), "tool call", "tool calls"),
            token_total(&result["usage"])
        );
        let tree = &result["tree_cost"];
        // Enabling delegation alone creates tree_cost, even without any workers.
        let agents_ran = yes(&state["_display"], "agents_ran")
            || [
                "input_tokens",
                "cached_input_tokens",
                "output_tokens",
                "reasoning_tokens",
                "unreported_attempts",
            ]
            .iter()
            .any(|key| tree[*key] != result["usage"][*key]);
        if !tree.is_null() && agents_ran {
            line.push_str(&format!(" · tree {}", token_total(tree)));
        }
        return if styled {
            format!("\n{DIM}{line}{RESET}\n")
        } else {
            format!("{line}\n")
        };
    }
    let (mark, color) = match status {
        "completed" => ("✓", SUCCESS),
        "cancelled" => ("–", DIM),
        "effect_unknown" => ("!", WARNING),
        _ => ("✗", ERROR),
    };
    let suffix = format!(
        " {status} · {} · {} · {} · {elapsed}",
        plural(number(result, "steps"), "step", "steps"),
        plural(number(result, "tool_calls"), "tool call", "tool calls"),
        usage(&result["usage"])
    );
    let mut out = if styled {
        format!("\n{color}{mark}{RESET}{DIM}{suffix}{RESET}\n")
    } else {
        format!("{mark}{suffix}\n")
    };
    if !result["tree_cost"].is_null() {
        let cost = &result["tree_cost"];
        out.push_str(&format!(
            "  tree cost: {} input · {} output tokens{}\n",
            count(number(cost, "input_tokens")),
            count(number(cost, "output_tokens")),
            if !yes(cost, "known") || number(cost, "unreported_attempts") > 0 {
                " (incomplete)"
            } else {
                ""
            }
        ));
    }
    out
}

fn agent_effect_summary(result: &Value, state: &Value, recap: bool) -> String {
    use std::collections::{BTreeMap, BTreeSet};
    let mut agents = BTreeMap::<&str, (BTreeSet<(&str, &str)>, i64, i64)>::new();
    let mut out = String::new();
    for effect in result["effects"].as_array().into_iter().flatten() {
        let id = text(effect, "agent_id");
        if id.is_empty() {
            continue;
        }
        let label = activity::agent_label(id, state);
        if text(effect, "effect") == "unknown" {
            let description = if text(effect, "path").is_empty() {
                text(effect, "summary").to_string()
            } else {
                format!(
                    "{} {}",
                    text(effect, "operation"),
                    short_path(text(effect, "path"))
                )
            };
            out.push_str(&format!(
                "  ! {label}: {} (effect unknown) [{}]\n",
                sanitize(&description).replace('\n', "↵").replace('\t', "⇥"),
                sanitize(&short_path(text(effect, "workspace")))
                    .replace('\n', "↵")
                    .replace('\t', "⇥")
            ));
            continue;
        }
        if !recap {
            continue;
        }
        let (files, commands, omitted) = agents.entry(id).or_default();
        if !text(effect, "path").is_empty() {
            files.insert((text(effect, "workspace"), text(effect, "path")));
        } else if number(effect, "omitted_paths") > 0 {
            *omitted += number(effect, "omitted_paths");
        } else {
            *commands += 1;
        }
    }
    for (id, (files, commands, omitted)) in agents {
        let mut parts = Vec::new();
        if !files.is_empty() {
            parts.push(format!(
                "{} changed",
                plural(files.len() as i64, "file", "files")
            ));
        }
        if commands > 0 {
            parts.push(plural(commands, "operation", "operations"));
        }
        if omitted > 0 {
            parts.push(format!("{omitted} additional paths omitted"));
        }
        out.push_str(&format!(
            "  {}: {}\n",
            activity::agent_label(id, state),
            parts.join(" · ")
        ));
    }
    out
}

/// Result prints a reply to stdout and its factual run summary to stderr.
pub fn result(result: &Value, elapsed: Duration, cfg: &Value, state: &Value, chat: bool) {
    result_in_region(result, elapsed, cfg, state, chat, None);
}

/// Result in region inserts presentation above a live input pane.
pub fn result_in_region(
    result: &Value,
    elapsed: Duration,
    cfg: &Value,
    state: &Value,
    chat: bool,
    region: Option<&crate::terminal::RegionHandle>,
) {
    macro_rules! eprintln {
        () => { crate::terminal::output(region, 2, "\n") };
        ($($args:tt)*) => { crate::terminal::output(region, 2, &format!("{}\n", format_args!($($args)*))) };
    }
    macro_rules! eprint {
        ($($args:tt)*) => { crate::terminal::output(region, 2, &format!($($args)*)) };
    }
    macro_rules! println {
        ($($args:tt)*) => { crate::terminal::output(region, 1, &format!("{}\n", format_args!($($args)*))) };
    }
    let reply = text(result, "reply");
    // Only a published reply needs its terminal summary ready before stdout is released.
    let output_update = region.filter(|_| !reply.is_empty());
    if let Some(region) = output_update {
        let _ = region.lock().unwrap().begin_output();
    }
    if !reply.is_empty() {
        if styled(2)
            && (yes(&state["_display"], "printed")
                || region.is_some_and(|region| region.lock().unwrap().printed))
        {
            eprintln!();
        }
        println!(
            "{}",
            reply_text(reply, chat, styled(1), columns(1).min(100))
        )
    }
    let status = text(result, "status");
    eprint!("{}", turn_summary(result, elapsed, state, chat, styled(2)));
    eprint!("{}", crate::file_diff::totals(state, styled(2)));
    if !chat || status != "completed" || yes(cfg, "recap") {
        eprint!("{}", activity::browsing_summary(state, styled(2)));
    }
    if !text(result, "reason").is_empty() {
        let reason = if text(result, "reason") == "no_followup_step" {
            "the step budget ran out"
        } else {
            text(result, "reason")
        };
        eprintln!("  {}", sanitize(reason))
    }
    eprint!(
        "{}",
        agent_effect_summary(result, state, yes(cfg, "recap") || status != "completed")
    );
    for effect in result["effects"].as_array().into_iter().flatten() {
        if !text(effect, "agent_id").is_empty() {
            continue;
        }
        let unknown = text(effect, "effect") == "unknown";
        if !yes(cfg, "recap") && status == "completed" && !unknown {
            continue;
        }
        let description = if text(effect, "path").is_empty() {
            text(effect, "summary").to_string()
        } else {
            format!("{} {}", text(effect, "operation"), text(effect, "path"))
        };
        if unknown && status == "completed" && !yes(cfg, "recap") {
            eprintln!("  ! {} (effect unknown)", sanitize(&description));
        }
    }
    if yes(cfg, "recap") || status != "completed" {
        for line in state["_display"]["recap"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if let Some(command) = line.strip_prefix("ran ") {
                eprintln!("  ran     {command}");
            } else {
                eprintln!("  {line}");
            }
        }
    }
    if chat && !text(state, "blocked").is_empty() {
        let line = format!(
            "  session blocked: {}",
            blocked_advice(text(state, "blocked"))
        );
        if styled(2) {
            eprintln!("{DIM}{line}{RESET}");
        } else {
            eprintln!("{line}");
        }
    }
    if !chat || status != "completed" {
        let path = text(result, "trace_path");
        eprintln!(
            "trace: {}",
            if path.is_empty() {
                "not recorded".into()
            } else if styled(2) {
                short_path(path)
            } else {
                sanitize(path)
            }
        )
    }
    let router = &result["router_usage"];
    if !chat
        || status != "completed"
        || !yes(router, "known")
        || number(router, "unreported_attempts") > 0
    {
        eprint!("{}", router_usage_note(router, styled(2)));
    }
    if chat && text(state, "blocked").is_empty() && yes(result, "resumable") {
        let line = if status == "cancelled" {
            "  interrupted; inspect unknown effects before continuing"
        } else {
            "  nothing was lost; your next message continues from the last completed step"
        };
        if styled(2) {
            eprintln!("{DIM}{line}{RESET}");
        } else {
            eprintln!("{line}");
        }
    }
    if chat && styled(2) {
        eprintln!()
    }
    if let Some(region) = output_update {
        let _ = region.lock().unwrap().end_output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn large_markdown_reply() -> String {
        let section = concat!(
            "## Rendering performance report\n\n",
            "This **Markdown reply** checks wrapping and *emphasis* while preserving `inline code` and [links](https://example.invalid/docs) across repeated sections with enough ordinary words to cross eighty terminal columns reliably.\n\n",
            "- Bullet items include **bold text** and *italic text* with hanging indentation.\n",
            "1. Numbered items preserve their prefix and show `code` beside normal words.\n",
            "> Quoted paragraphs keep their visible marker when the text wraps onto another row.\n\n",
            "| Feature | Status |\n| :--- | ---: |\n| cache | **ready** |\n\n",
            "---\n```rust\nlet cached = true;\n```\n\n",
        );
        let reply = section.repeat(64);
        assert_eq!(reply.split_whitespace().count(), 5_760);
        reply
    }

    #[test]
    fn large_markdown_reply_preserves_rendered_bytes() {
        use sha2::{Digest, Sha256};

        let rendered = display(&large_markdown_reply(), true, 80);
        assert_eq!(rendered.len(), 59_840);
        assert_eq!(
            format!("{:x}", Sha256::digest(rendered.as_bytes())),
            "825357f99c418419a3bc03c7b0af0cf51d2fc985ac97aa2e6cb775fd88563cb7"
        );
    }

    #[test]
    #[ignore = "manual rendering benchmark; run with --release --ignored --nocapture"]
    fn benchmark_large_markdown_reply() {
        use sha2::{Digest, Sha256};
        use std::{hint::black_box, time::Instant};

        let reply = large_markdown_reply();
        let start = Instant::now();
        let rendered = black_box(display(black_box(&reply), true, 80));
        let first_ms = start.elapsed().as_secs_f64() * 1_000.;
        println!(
            "5760 words, {} input bytes, {} output bytes, width 80; sha256 {:x}",
            reply.len(),
            rendered.len(),
            Sha256::digest(rendered.as_bytes())
        );
        println!("first render: {first_ms:.3} ms");
        let mut samples = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            for _ in 0..3 {
                black_box(display(black_box(&reply), true, 80));
            }
            samples.push(start.elapsed().as_secs_f64() * 1_000. / 3.);
        }
        println!("7 samples, 3 renders/sample, ms/render: {samples:.3?}");
        samples.sort_by(f64::total_cmp);
        println!("median: {:.3} ms/render", samples[3]);
    }
    #[test]
    fn router_notes_distinguish_missing_unknown_and_known_usage() {
        for (usage, expected) in [
            (serde_json::Value::Null, ""),
            (
                serde_json::json!({"known":false,"input_tokens":13,"output_tokens":3}),
                "  · router     Jev usage unknown; separate from generation\n",
            ),
            (
                serde_json::json!({"known":true,"input_tokens":13,"output_tokens":3}),
                "  · router     Jev 13 tokens in · 3 out; separate from generation\n",
            ),
            (
                serde_json::json!({"known":true,"input_tokens":0,"output_tokens":0}),
                "  · router     Jev 0 tokens in · 0 out; separate from generation\n",
            ),
        ] {
            assert_eq!(router_usage_note(&usage, false), expected);
            assert_eq!(
                router_usage_note(&usage, true),
                if expected.is_empty() {
                    String::new()
                } else {
                    format!("{DIM}{}{RESET}\n", expected.trim_end_matches('\n'))
                }
            );
        }
    }
    #[test]
    fn plan_rendering_removes_only_tag_lines_not_content_whitespace() {
        for content in [
            "\u{2028}\u{2029}",
            "  indented\t ",
            "\n\nbody\n\n",
            "",
            "\u{a0}body\u{3000}",
        ] {
            let reply = format!("before\n<plan>\n{content}\n</plan>\nafter");
            assert_eq!(
                reply_text(&reply, true, false, 0),
                format!("before\nplan\n{content}\nafter")
            );
            assert_eq!(reply_text(&reply, false, false, 0), reply);
        }
        for reply in [
            "inline <plan>\nbody\n</plan>",
            "<plan> \nbody\n</plan>",
            "<plan>\nbody\n</plan> ",
            "<plan>\none\n</plan>\n<plan>\nunfinished",
        ] {
            assert_eq!(reply_text(reply, true, false, 0), reply);
        }
        assert_eq!(
            reply_text("<plan>\none\n</plan>\n<plan>\ntwo\n</plan>", true, false, 0),
            "<plan>\none\n</plan>\nplan\ntwo"
        );
    }
    #[test]
    fn welcome_restores_the_flask_without_hiding_auth_authority_or_resume() {
        let mut cfg = serde_json::json!({"model":"gpt-6-luna", "provider":"openai", "auth_mode":"key", "reasoning_effort":"low", "workspace":"/work/a-very-long-workspace-name-that-must-not-wrap/repo", "session_id":"0123456789abcdef", "checkpoint_enabled":true});
        for styled in [false, true] {
            for columns in [0, 40, 71, 72, 80, 100] {
                let out = welcome(&cfg, columns, styled);
                let rows: Vec<_> = out.lines().collect();
                assert!(
                    columns == 0 || rows.iter().all(|row| width(row) < columns),
                    "{out}"
                );
                let illustrated = styled && columns >= 72;
                assert_eq!(out.contains("▝▙▄▄▄▄▄▄▄▄▄▟▘"), illustrated, "{out}");
                if illustrated {
                    assert!(rows.len() >= 8, "{out}");
                    assert!(rows[1].contains("\x1b[1mre\x1b[38;5;99m:"), "{out}");
                    assert!(rows[2].contains("gpt-6-luna"), "{out}");
                } else {
                    assert!(rows.len() <= 8, "{out}");
                }
                assert_eq!(
                    rows.iter().filter(|row| row.contains("workspace ")).count(),
                    1,
                    "{out}"
                );
                let plain = welcome(&cfg, columns, false)
                    .lines()
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(plain.contains("API key"), "{out}");
                assert!(out.contains("commands run as you"), "{out}");
                assert_eq!(out.matches("--resume").count(), 1, "{out}");
                assert!(out.contains("/help"), "{out}");
                assert!(!out.contains("AGENTS.md loaded"));
            }
        }
        cfg["checkpoint_enabled"] = serde_json::json!(false);
        cfg["read_only"] = serde_json::json!(true);
        cfg["plan"] = serde_json::json!(true);
        let out = welcome(&cfg, 72, false);
        assert!(out.contains("read only · plan mode: exec and file changes refused"));
        assert!(!out.contains("resume"));
        let scripted = welcome(&serde_json::json!({"provider":"scripted"}), 80, false);
        assert!(!scripted.contains("session ID:"));
        assert_eq!(scripted.lines().count(), 4);
    }

    #[test]
    fn welcome_discloses_fast_token_rates_only_when_enabled() {
        for fast in [false, true] {
            let cfg = serde_json::json!({"provider":"openai", "model":"gpt-6.1-sol", "fast":fast});
            for columns in [0, 40, 72, 100] {
                for styled in [false, true] {
                    let out = welcome(&cfg, columns, styled);
                    assert_eq!(out.contains("fast mode on (higher token rates)"), fast);
                }
            }
        }
    }

    #[test]
    fn completed_chat_summary_is_compact_but_run_errors_recap_and_unknown_usage_remain_visible() {
        const CHILD: &str = "REAGENT_TEST_TURN_SUMMARY";
        if let Ok(scenario) = std::env::var(CHILD) {
            let mut result = serde_json::json!({"status":"completed", "steps":3, "tool_calls":2, "reply":"reply", "usage":{"known":true,"input_tokens":1200,"cached_input_tokens":700,"output_tokens":300,"reasoning_tokens":100,"unreported_attempts":0}});
            let mut state =
                serde_json::json!({"_display":{"recap":["changed file"],"reads":["a"]}});
            let mut cfg = serde_json::json!({});
            match scenario.as_str() {
                "cancelled" => result["status"] = serde_json::json!("cancelled"),
                "incomplete" => {
                    result["usage"]["known"] = serde_json::json!(false);
                    result["usage"]["unreported_attempts"] = serde_json::json!(2);
                }
                "unused_agents" => {
                    result["tree_cost"] = result["usage"].clone();
                    state["_display"]["agents"] = serde_json::json!({"t1":"previous turn"});
                }
                "historical_unknown_agent" => {
                    result["tree_cost"] = result["usage"].clone();
                    result["tree_cost"]["known"] = serde_json::json!(false);
                }
                "overflow" => {
                    result["usage"]["input_tokens"] = serde_json::json!(i64::MAX);
                    result["usage"]["output_tokens"] = serde_json::json!(1);
                }
                "zero_cost_agents" => {
                    result["tree_cost"] = result["usage"].clone();
                    state["_display"]["agents_ran"] = serde_json::json!(true);
                }
                "agents" => {
                    result["tree_cost"] = result["usage"].clone();
                    result["tree_cost"]["input_tokens"] = serde_json::json!(2200);
                }
                "recap" => cfg["recap"] = serde_json::json!(true),
                "unknown_effect" => {
                    result["effects"] =
                        serde_json::json!([{"effect":"unknown","summary":"command may have run"}])
                }
                "status" => {
                    eprintln!("{}", run_details(&result));
                    return;
                }
                _ => {}
            }
            state["blocked"] = serde_json::json!("");
            result_in_region(
                &result,
                Duration::from_millis(1250),
                &cfg,
                &state,
                scenario != "run",
                None,
            );
            return;
        }
        for scenario in [
            "completed",
            "unused_agents",
            "historical_unknown_agent",
            "overflow",
            "agents",
            "zero_cost_agents",
            "incomplete",
            "cancelled",
            "run",
            "recap",
            "unknown_effect",
            "status",
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "render::tests::completed_chat_summary_is_compact_but_run_errors_recap_and_unknown_usage_remain_visible", "--nocapture"])
                .env(CHILD, scenario).output().unwrap();
            assert!(output.status.success());
            let stderr = String::from_utf8(output.stderr).unwrap();
            match scenario {
                "completed" | "unused_agents" | "historical_unknown_agent" => {
                    assert_eq!(stderr, "✓ 1.2s · 2 tool calls · 1.5k tokens\n")
                }
                "agents" => assert_eq!(
                    stderr,
                    "✓ 1.2s · 2 tool calls · 1.5k tokens · tree 2.5k tokens\n"
                ),
                "zero_cost_agents" => assert_eq!(
                    stderr,
                    "✓ 1.2s · 2 tool calls · 1.5k tokens · tree 1.5k tokens\n"
                ),
                "overflow" => assert_eq!(stderr, "✓ 1.2s · 2 tool calls · tokens unknown\n"),
                "incomplete" => assert_eq!(
                    stderr,
                    "✓ 1.2s · 2 tool calls · tokens unknown (incomplete; 2 unreported retries)\n"
                ),
                "recap" => assert!(stderr.contains("changed file"), "{stderr}"),
                "unknown_effect" => assert!(stderr.contains("command may have run (effect unknown)"), "{stderr}"),
                "status" => assert_eq!(stderr, "last run   completed · 3 steps · 2 tool calls · 1.2k in (700 cached) · 300 out\n"),
                _ => {
                    assert!(
                        stderr.contains(
                            "3 steps · 2 tool calls · 1.2k in (700 cached) · 300 out · 1.2s"
                        ),
                        "{stderr}"
                    );
                    assert!(stderr.contains("trace: not recorded"), "{stderr}");
                }
            }
        }
    }

    #[test]
    fn window_overrides_use_millions_and_respect_catalog_caps() {
        for (tokens, label) in [
            (100, "100"),
            (200_000, "200k"),
            (1_000_000, "1M"),
            (1_050_000, "1.05M"),
        ] {
            assert_eq!(window_label(tokens), label);
        }
        let cfg = serde_json::json!({"provider":"openai","model":"gpt-6-luna","context_window_tokens":1_000_000});
        assert!(model_presentation(&cfg).2.contains("window 1M override"));
    }
    #[test]
    fn retry_delay_matches_go_duration_units() {
        for (milliseconds, expected) in [
            (0, "0s"),
            (500, "500ms"),
            (1000, "1s"),
            (1532, "1.532s"),
            (1500, "1.5s"),
            (60000, "1m0s"),
            (3600000, "1h0m0s"),
        ] {
            assert_eq!(retry_delay(milliseconds), expected);
        }
    }
    #[test]
    fn agent_recap_counts_distinct_files_and_keeps_uncertain_effects_explicit() {
        use serde_json::json;
        let state = json!({"_display":{"agents":{"t1":"writer"}}});
        let result = json!({"effects":[
            {"agent_id":"t1","workspace":"/repo","path":"a","effect":"applied"},
            {"agent_id":"t1","workspace":"/repo","path":"a","effect":"applied"},
            {"agent_id":"t1","workspace":"/repo","path":"b","effect":"applied"},
            {"agent_id":"t1","workspace":"/repo","effect":"applied","omitted_paths":3},
            {"agent_id":"t2","workspace":"/repo","effect":"applied","summary":"check"},
            {"agent_id":"t1","workspace":"/repo","effect":"unknown","summary":"exec timed out\ninspect"},
            {"workspace":"/repo","path":"root","effect":"applied"}
        ]});
        let recap = agent_effect_summary(&result, &state, true);
        assert!(
            recap.contains("writer: 2 files changed · 3 additional paths omitted\n"),
            "{recap}"
        );
        assert!(recap.contains("t2: 1 operation\n"), "{recap}");
        assert!(
            recap.contains("! writer: exec timed out↵inspect (effect unknown) [/repo]"),
            "{recap}"
        );
        assert_eq!(recap.lines().count(), 3);
        let quiet = agent_effect_summary(&result, &state, false);
        assert_eq!(quiet.lines().count(), 1);
        assert!(quiet.contains("effect unknown"));
    }

    #[test]
    fn agent_notices_are_single_safe_lines_without_wait_labels_in_plain_output() {
        const CHILD: &str = "REAGENT_TEST_AGENT_NOTICES";
        if std::env::var_os(CHILD).is_some() {
            agents_refreshed(
                None,
                2,
                Some("waiting on t1 worker (step 3)"),
                &["t1 worker finished · completed\nnew\ttext".into()],
                &[crate::agents::AgentRow {
                    label: "hidden agent".into(),
                    activity: "thinking".into(),
                    started: std::time::Instant::now(),
                }],
            );
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "render::tests::agent_notices_are_single_safe_lines_without_wait_labels_in_plain_output", "--nocapture"])
            .env(CHILD, "1")
            .output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "  ↳ t1 worker finished · completed↵new⇥text\n"
        );
    }
    #[test]
    fn controls_are_visible() {
        assert_eq!(display("x\x1b[31m\r\n", false, 80), "x\\x1b[31m\\x0d\n")
    }
    #[test]
    fn markdown_boundaries() {
        assert_eq!(
            markdown("# title\n```rs\n**literal**\n```", 80),
            format!(
                "{BOLD}{ACCENT}title{RESET}\n{DIM}│ rs{RESET}\n{DIM}│{RESET} {ACCENT}**literal**{RESET}"
            )
        );
        assert_eq!(width("a\x1b[31m界e\u{301}"), 4);
        assert_eq!(truncate("界界", 3), "界…")
    }
    #[test]
    fn headings_restore_the_accent_after_inline_styles_and_wraps() {
        let heading = display("## **one** two three", true, 7);
        assert!(heading.starts_with(&format!("{BOLD}{ACCENT}")));
        assert!(heading.contains(&format!("one{RESET}{BOLD}{ACCENT} two")));
        assert!(heading.contains(&format!("\n{BOLD}{ACCENT}three")));
        assert!(heading.lines().all(|row| width(row) <= 7));
        assert_eq!(
            display("## **one** two three", false, 7),
            "## **one** two three"
        );
    }

    #[test]
    fn code_blocks_hide_fences_keep_whitespace_and_colour_diff_lines() {
        for (source, expected) in [
            ("```rust\n  let x = 1;\n```", format!("{DIM}│ rust{RESET}\n{DIM}│{RESET} {ACCENT}  let x = 1;{RESET}")),
            ("```\n**literal**\n```", format!("{DIM}│{RESET} {ACCENT}**literal**{RESET}")),
            ("```diff\n same\n-old\n+new\n```", format!("{DIM}│ diff{RESET}\n{DIM}│{RESET}  same{RESET}\n{DIM}│{RESET} {ERROR}-old{RESET}\n{DIM}│{RESET} {SUCCESS}+new{RESET}")),
            ("````text\n```\n````", format!("{DIM}│ text{RESET}\n{DIM}│{RESET} {ACCENT}```{RESET}")),
        ] {
            assert_eq!(display(source, true, 80), expected);
            assert_eq!(display(source, false, 80), source);
        }
        let wrapped = display("```rs\n  界界界  x\tend\n```", true, 10);
        assert!(wrapped.lines().all(|row| width(row) <= 10), "{wrapped:?}");
        assert_eq!(wrapped.lines().count(), 4);
        assert!(!wrapped.contains('\t'));
    }

    #[test]
    fn hyperlinks_keep_destinations_out_of_visible_text_and_plain_output_unchanged() {
        let url = "https://example.invalid/a*b?next=c*d";
        let source = format!("[read **these** docs]({url})");
        let linked = display(&source, true, 80);
        assert_eq!(linked, format!("\x1b]8;;{url}\x1b\\{UNDERLINE}read {BOLD}these{RESET}{UNDERLINE} docs{UNDERLINE_OFF}{LINK_CLOSE}"));
        assert_eq!(width(&linked), 15);
        assert_eq!(display(&source, false, 10), source);
        let outer_bold = display(&format!("**read [docs]({url}) now**"), true, 80);
        assert!(outer_bold.starts_with(&format!("{BOLD}read \x1b]8;;{url}\x1b\\")));
        assert!(outer_bold.ends_with(&format!("{LINK_CLOSE} now{RESET}")));
        assert!(!outer_bold.contains("**"));
        let code = display(&format!("`[docs]({url})`"), true, 80);
        assert!(!code.contains("\x1b]"));
        assert!(code.contains(&format!("[docs]({url})")));
        let injected = display(
            "[label](https://example.invalid/\x1b]8;;evil\x07)",
            true,
            80,
        );
        assert!(!injected.contains("evil\x07"));
    }

    #[test]
    fn wrapped_and_truncated_hyperlinks_close_before_leaving_each_row() {
        let source = "[one two three](https://example.invalid/docs) outside";
        let rendered = display(source, true, 7);
        let open = "\x1b]8;;https://example.invalid/docs\x1b\\";
        assert!(
            rendered.starts_with(&format!(
                "{open}{UNDERLINE}one two{LINK_CLOSE}{RESET}\n{UNDERLINE}{open}three"
            )),
            "{rendered:?}"
        );
        for row in rendered.lines() {
            assert_eq!(row.matches(open).count(), row.matches(LINK_CLOSE).count());
            assert!(width(row) <= 7);
        }
        let clipped = truncate(&display(source, true, 80), 6);
        assert_eq!(width(&clipped), 6);
        assert!(clipped.ends_with(&format!("…{LINK_CLOSE}{RESET}")));
        assert_eq!(
            width("\x1b]8;;https://example.invalid\x07界e\u{301}\x1b]8;;\x07"),
            3
        );
    }

    #[test]
    fn wraps_open_styles() {
        assert_eq!(
            markdown("**one two three**", 7),
            format!("{BOLD}one two{RESET}\n{BOLD}three{RESET}")
        )
    }
}
