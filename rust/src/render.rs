use crate::cli::{number, text, yes};
use regex::Regex;
use serde_json::Value;
use std::time::Duration;

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
fn wrap(first: &str, hang: &str, body: &str, columns: usize) -> Vec<String> {
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
pub fn rune_width(c: char) -> usize {
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
        effort.push_str(&format!(
            " · window {} override",
            if cap % 1000 == 0 {
                format!("{}k", cap / 1000)
            } else {
                cap.to_string()
            }
        ))
    }
    (text(cfg, "model").into(), provider, effort)
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
/// Header presents a run's model, workspace, and authority.
pub fn header(cfg: &Value, chat: bool) {
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
            std::env::var("API_PROXY_URL").unwrap_or_default()
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
/// Result prints a reply to stdout and its factual run summary to stderr.
pub fn result(result: &Value, elapsed: Duration, cfg: &Value, chat: bool) {
    let reply = text(result, "reply");
    if !reply.is_empty() {
        let mut reply = reply.to_string();
        if chat {
            if let Some(start) = reply.find("<plan>") {
                if let Some(end) = reply[start..].find("</plan>") {
                    let end = start + end;
                    reply = format!(
                        "{}plan\n{}{}",
                        &reply[..start],
                        reply[start + 6..end].trim(),
                        &reply[end + 7..]
                    );
                }
            }
        }
        println!("{}", display(&reply, styled(1), columns(1).min(100)))
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
    if !text(result, "reason").is_empty() {
        let reason = if text(result, "reason") == "no_followup_step" {
            "the step budget ran out"
        } else {
            text(result, "reason")
        };
        eprintln!("  {}", sanitize(reason))
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
    if chat && yes(result, "resumable") {
        eprintln!("  nothing was lost; your next message continues from the last completed step")
    }
    if chat && styled(2) {
        eprintln!()
    }
    let _ = cfg;
}

#[cfg(test)]
mod tests {
    use super::*;
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
