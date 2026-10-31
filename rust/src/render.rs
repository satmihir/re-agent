use crate::cli::{number, text, yes};
use regex::Regex;
use serde_json::Value;
use std::time::Duration;
#[path = "cli/activity.rs"]
mod activity;
pub use activity::{
    begin_turn, progress_text, progress_text_in_region, tool_finished, tool_finished_in_region,
};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CODE: &str = "\x1b[36m";

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
fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}
fn spans(text: &str) -> String {
    let text = re(r"\[([^\]]*)\]\(([^)\s]+)\)")
        .replace_all(text, format!("${{1}}{DIM} (${{2}}){RESET}"))
        .into_owned();
    let parts: Vec<_> = text.split('`').collect();
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        if i % 2 == 1 && i != parts.len() - 1 {
            out.push_str(&format!("{CODE}{p}{RESET}"));
            continue;
        }
        let bold = re(r"\*\*([^*]+)\*\*").replace_all(p, format!("{BOLD}${{1}}{RESET}"));
        out.push_str(&re(r"\*([^*]+)\*").replace_all(&bold, format!("\x1b[3m${{1}}{RESET}")));
    }
    out
}
fn line(text: &str) -> (String, String, String) {
    let content = text.trim_start_matches([' ', '\t']);
    let indent = &text[..text.len() - content.len()];
    let level = content.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&level) && content.as_bytes().get(level) == Some(&b' ') {
        return (
            indent.into(),
            indent.into(),
            format!("{BOLD}{}{RESET}", spans(content[level..].trim())),
        );
    }
    if re(r"^(-{3,}|\*{3,}|_{3,})$").is_match(content) {
        return (
            format!("{indent}{DIM}{}{RESET}", "─".repeat(32)),
            String::new(),
            String::new(),
        );
    }
    if let Some(m) = re(r"^[-*+][ \t]+").find(content) {
        return (
            format!("{indent}{DIM}•{RESET} "),
            format!("{indent}  "),
            spans(&content[m.end()..]),
        );
    }
    if let Some(m) = re(r"^\d+[.)][ \t]+").find(content) {
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
    for word in body.split(' ') {
        if word.is_empty() && words == 0 && !rows.is_empty() {
            continue;
        }
        let w = width(word);
        if words > 0 && cells + 1 + w > columns {
            if !open.is_empty() {
                row.push_str(RESET)
            }
            rows.push(row);
            row = format!("{hang}{open}");
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
        for seq in re("\x1b\\[[0-9;]*m").find_iter(word) {
            if seq.as_str() == RESET {
                open.clear()
            } else {
                open.push_str(seq.as_str())
            }
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
/// Markdown renders the reference terminal subset, including fences and tables.
pub fn markdown(text: &str, columns: usize) -> String {
    let lines: Vec<_> = text.split('\n').collect();
    let mut rendered = Vec::new();
    let mut fence = false;
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if l.trim().starts_with("```") {
            fence = !fence;
            rendered.push(format!("{DIM}{l}{RESET}"));
            i += 1;
            continue;
        }
        if fence {
            rendered.push(format!("{CODE}{l}{RESET}"));
            i += 1;
            continue;
        }
        let h = l.trim();
        if h.starts_with('|')
            && h.ends_with('|')
            && h.len() >= 2
            && i + 1 < lines.len()
            && re(r"^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$").is_match(lines[i + 1])
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
    static ZERO_WIDTH: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    if ZERO_WIDTH
        .get_or_init(|| re(r"^[\p{Mn}\p{Me}\p{Cf}]$"))
        .is_match(&c.to_string())
    {
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
/// Width ignores ANSI CSI sequences while measuring terminal cells.
pub fn width(text: &str) -> usize {
    let mut chars = text.chars().peekable();
    let mut n = 0;
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
            continue;
        }
        n += rune_width(c)
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
    for c in text.chars() {
        if n + rune_width(c) > cells - 1 {
            break;
        }
        n += rune_width(c);
        out.push(c)
    }
    out.push('…');
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
fn welcome(cfg: &Value, columns: usize) -> String {
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
    let shades = [0, 0, 17, 18, 19, 20, 56, 57, 57, 93, 93, 0, 0];
    let bubble = "\x1b[38;5;99m";
    let teal = "\x1b[1;38;5;86m";
    let room = columns - 17;
    let dim = |s: &str| format!("{DIM}{}{RESET}", truncate(s, room));
    let (model, provider, effort) = model_presentation(cfg);
    let name = sanitize(&model);
    let line = if provider.is_empty() {
        name.clone()
    } else {
        format!("{name} · {} · {effort}", sanitize(&provider))
    };
    let line = truncate(&line, room);
    let model_line = if line.len() > name.len() && line.starts_with(&name) {
        format!("{BOLD}{name}{RESET}{DIM}{}{RESET}", &line[name.len()..])
    } else {
        format!("{BOLD}{line}{RESET}")
    };
    let mut mode = mode(cfg).to_string();
    if yes(cfg, "plan") {
        mode.push_str(" · plan mode");
    }
    let mode =
        truncate(&mode, room).replacen("plan mode", &format!("{teal}plan mode\x1b[22;39m{DIM}"), 1);
    let session = if text(cfg, "provider") != "scripted" && !text(cfg, "session_id").is_empty() {
        dim(&format!("session ID: {}", text(cfg, "session_id")))
    } else {
        String::new()
    };
    let via = if yes(cfg, "proxied") {
        dim(&format!(
            "requests go to {} (API_PROXY_URL)",
            sanitize(&crate::transport::redacted(
                &std::env::var("API_PROXY_URL").unwrap_or_default()
            ))
        ))
    } else {
        String::new()
    };
    let loaded = cfg["project_instructions"]
        .as_str()
        .map(|s| {
            dim(&format!(
                "AGENTS.md loaded ({:.1} KB)",
                s.len() as f64 / 1000.
            ))
        })
        .unwrap_or_default();
    let details = [
        session,
        format!("{BOLD}re{RESET}{bubble}{BOLD}:{RESET}{BOLD}agent{RESET}"),
        model_line,
        truncate(&sanitize(&short_path(text(cfg, "workspace"))), room),
        format!("{DIM}{mode}{RESET}"),
        via,
        format!("/help{DIM} commands · {RESET}!cmd{DIM} shell · {RESET}Ctrl-D{DIM} exit{RESET}"),
        loaded,
    ];
    let mut out = String::new();
    for (row, detail) in art.iter().zip(details) {
        for (col, c) in row.chars().enumerate() {
            match c {
                '●' => out.push_str(&format!("{bubble}●{RESET}")),
                'B' => out.push_str(&format!("\x1b[38;5;63m•{RESET}")),
                'P' => out.push_str(&format!("{bubble}•{RESET}")),
                'L' => out.push_str(&format!("\x1b[48;5;{}m {RESET}", shades[col])),
                '>' => out.push_str(&format!("\x1b[48;5;{}m{teal}❯{RESET}", shades[col])),
                _ => out.push(c),
            }
        }
        if !detail.is_empty() {
            out.push_str("   ");
            out.push_str(&detail);
        }
        out.push('\n');
    }
    out
}

/// Header presents a run's model, workspace, and authority.
pub fn header(cfg: &Value, chat: bool) {
    if chat && styled(2) && columns(2) >= 72 {
        eprint!("{}", welcome(cfg, columns(2)));
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
    let prefix = if chat {
        "re:agent chat · "
    } else {
        "re:agent · "
    };
    let suffix = if chat {
        details
    } else {
        format!("{details} · {mode} · {workspace}")
    };
    if styled(2) {
        eprintln!("{DIM}{prefix}{RESET}{BOLD}{model}{RESET}{DIM}{suffix}{RESET}")
    } else {
        eprintln!("{prefix}{model}{suffix}")
    }
    if chat && !text(cfg, "session_id").is_empty() && text(cfg, "provider") != "scripted" {
        header_line(&format!("session ID: {}", text(cfg, "session_id")))
    }
    if yes(cfg, "proxied") {
        header_line(&format!(
            "requests go to {} (API_PROXY_URL)",
            crate::transport::redacted(&std::env::var("API_PROXY_URL").unwrap_or_default())
        ))
    }
    if chat {
        let limit = |key| {
            let n = number(cfg, key);
            if n == 0 {
                "unlimited".into()
            } else {
                n.to_string()
            }
        };
        header_line(&format!(
            "workspace {workspace} · {mode} · {} steps, {} tool calls per turn",
            limit("max_steps"),
            limit("max_tool_calls")
        ));
        if let Some(s) = cfg["project_instructions"].as_str() {
            header_line(&format!(
                "AGENTS.md loaded ({:.1} KB)",
                s.len() as f64 / 1000.
            ))
        }
    }
    if yes(cfg, "plan") {
        header_line("! plan mode: exec and file changes are refused while plan mode is on")
    } else if !yes(cfg, "read_only") {
        header_line(&format!("! exec mode: commands run as you, in {workspace}, and can read, write, and use the network"))
    }
    if chat {
        header_line("/help for commands · Ctrl-D to exit")
    }
}
/// Model started shows the active request without committing a transcript line.
pub fn model_started(
    region: Option<&crate::terminal::RegionHandle>,
    model: &str,
    step: i64,
    max_steps: i64,
) {
    let mut label = format!("waiting for {} · step {step}", sanitize(model));
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
) {
    if let Some(region) = region {
        let label = wait_label.map(|label| sanitize(label).replace('\n', "↵").replace('\t', "⇥"));
        let _ = region
            .lock()
            .unwrap()
            .refresh_agents(count, label.as_deref());
    }
    for notice in notices {
        let notice = sanitize(notice).replace('\n', "↵").replace('\t', "⇥");
        crate::terminal::output(region, 2, &format!("  ↳ {notice}\n"));
    }
}

/// Tool started shows progress for an executing command.
pub fn tool_started(region: Option<&crate::terminal::RegionHandle>, call: &Value) {
    if text(call, "name") != "exec" {
        return;
    }
    if let Some(region) = region {
        let label = format!("running {}", activity::call_target(call));
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
        let label = format!("waiting for model · attempt {attempt}");
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
        "/reset to continue; /compact works before the window fills (watch the 60% warning)"
    } else {
        "/compact or /reset to continue"
    }
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
    let (mark, color) = match status {
        "completed" => ("✓", "\x1b[32m"),
        "cancelled" => ("–", DIM),
        "effect_unknown" => ("!", "\x1b[33m"),
        _ => ("✗", "\x1b[31m"),
    };
    let elapsed = if elapsed.as_secs() < 60 {
        format!("{:.1}s", elapsed.as_secs_f64())
    } else {
        format!("{}m{:02}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
    };
    let suffix = format!(
        " {status} · {} · {} · {} · {elapsed}",
        plural(number(result, "steps"), "step", "steps"),
        plural(number(result, "tool_calls"), "tool call", "tool calls"),
        usage(&result["usage"])
    );
    if styled(2) {
        eprintln!("\n{color}{mark}{RESET}{DIM}{suffix}{RESET}")
    } else {
        eprintln!("{mark}{suffix}")
    }
    if !result["tree_cost"].is_null() {
        let c = &result["tree_cost"];
        eprintln!(
            "  tree cost: {} input · {} output tokens{}",
            count(number(c, "input_tokens")),
            count(number(c, "output_tokens")),
            if !yes(c, "known") || number(c, "unreported_attempts") > 0 {
                " (incomplete)"
            } else {
                ""
            }
        )
    }
    eprint!("{}", activity::browsing_summary(state, styled(2)));
    if !text(result, "reason").is_empty() {
        let reason = if text(result, "reason") == "no_followup_step" {
            "the step budget ran out"
        } else {
            text(result, "reason")
        };
        eprintln!("  {}", sanitize(reason))
    }
    if yes(cfg, "recap") || status != "completed" {
        for effect in result["effects"].as_array().into_iter().flatten() {
            if !text(effect, "agent_id").is_empty() {
                let description = if text(effect, "path").is_empty() {
                    text(effect, "summary").to_string()
                } else {
                    format!("{} {}", text(effect, "operation"), text(effect, "path"))
                };
                eprintln!(
                    "  agent {}: {} ({}) [{}]",
                    sanitize(text(effect, "agent_id")),
                    sanitize(&description),
                    text(effect, "effect"),
                    sanitize(text(effect, "workspace"))
                );
            }
        }
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
    eprint!("{}", router_usage_note(&result["router_usage"], styled(2)));
    if chat && text(state, "blocked").is_empty() && yes(result, "resumable") {
        let line = "  nothing was lost; your next message continues from the last completed step";
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
    fn welcome_keeps_eight_rows_and_fits_session_details_beside_flask() {
        let cfg = serde_json::json!({"model":"gpt-6-luna", "provider":"openai", "auth_mode":"key", "reasoning_effort":"low", "workspace":"/work/repo", "session_id":"session", "plan":true, "project_instructions":"frozen"});
        for columns in [72, 80, 100] {
            let out = welcome(&cfg, columns);
            let rows: Vec<_> = out.lines().collect();
            assert_eq!(rows.len(), 8);
            assert!(rows.iter().all(|row| width(row) < columns));
            assert!(rows[0].contains("session ID: session"));
            assert!(rows[2].contains("gpt-6-luna"));
            assert!(rows[4].contains("\x1b[1;38;5;86mplan mode\x1b[22;39m"));
            assert!(rows[6].contains("/help"));
            assert!(rows[7].contains("AGENTS.md loaded"));
        }
        let scripted = welcome(&serde_json::json!({"provider":"scripted"}), 80);
        assert!(!scripted.contains("session ID:"));
        assert_eq!(scripted.lines().count(), 8);
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
    fn agent_notices_are_single_safe_lines_without_wait_labels_in_plain_output() {
        const CHILD: &str = "REAGENT_TEST_AGENT_NOTICES";
        if std::env::var_os(CHILD).is_some() {
            agents_refreshed(
                None,
                2,
                Some("waiting on t1 worker (step 3)"),
                &["t1 worker finished · completed\nnew\ttext".into()],
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
                "{BOLD}title{RESET}\n{DIM}```rs{RESET}\n{CODE}**literal**{RESET}\n{DIM}```{RESET}"
            )
        );
        assert_eq!(width("a\x1b[31m界e\u{301}"), 4);
        assert_eq!(truncate("界界", 3), "界…")
    }
    #[test]
    fn wraps_open_styles() {
        assert_eq!(
            markdown("**one two three**", 7),
            format!("{BOLD}one two{RESET}\n{BOLD}three{RESET}")
        )
    }
}
