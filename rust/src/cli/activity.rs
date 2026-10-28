use super::{
    columns, display, number, plural, sanitize, styled, text, truncate, width, yes, DIM, RESET,
};
use serde_json::{json, Value};

/// Begin turn resets presentation-only state without changing session history.
pub fn begin_turn(state: &mut Value) {
    state["_display"] = json!({"printed":false,"reads":[],"searches":0,"listings":0,"recap":[]});
}

/// Tool finished displays one outcome immediately and accumulates browsing and effects.
pub fn tool_finished(call: &Value, outcome: &Value, config: &Value, state: &mut Value) {
    tool_finished_in_region(call, outcome, config, state, None);
}

/// Tool finished in region inserts an outcome above retained terminal input.
pub fn tool_finished_in_region(
    call: &Value,
    outcome: &Value,
    config: &Value,
    state: &mut Value,
    region: Option<&crate::terminal::RegionHandle>,
) {
    if !yes(config, "_display") {
        return;
    }
    super::model_finished(region);
    if text(call, "name") == "switch_workspace"
        && yes(outcome, "ok")
        && yes(&outcome["data"], "changed")
        && !yes(config, "read_only")
        && !yes(state, "plan")
    {
        let workspace = sanitize(&super::short_path(text(&outcome["data"], "workspace")));
        let warning = format!("! exec mode: commands run as you, in {workspace}, and can read, write, and use the network");
        let warning = if styled(2) {
            format!("{DIM}{warning}{RESET}\n")
        } else {
            format!("{warning}\n")
        };
        crate::terminal::output(region, 2, &warning);
    }
    crate::terminal::output(
        region,
        2,
        &activity_text(call, outcome, state, styled(2), columns(2)),
    );
}

/// Progress text writes intermediate model commentary to stderr, not the final reply.
pub fn progress_text(message: &str, config: &Value, state: &mut Value) {
    progress_text_in_region(message, config, state, None);
}

/// Progress text in region leaves model commentary above the retained caret.
pub fn progress_text_in_region(
    message: &str,
    config: &Value,
    state: &mut Value,
    region: Option<&crate::terminal::RegionHandle>,
) {
    if message.is_empty() || !yes(config, "_display") {
        return;
    }
    if styled(2) && yes(&state["_display"], "printed") {
        crate::terminal::output(region, 2, "\n");
    }
    crate::terminal::output(
        region,
        2,
        &format!("{}\n", display(message, styled(2), columns(2).min(100))),
    );
    state["_display"]["printed"] = json!(true);
}

fn args(call: &Value) -> Option<Value> {
    let a: Value = serde_json::from_str(text(call, "arguments")).ok()?;
    if !a.is_object() && !a.is_null() {
        return None;
    }
    let fields: &[&str] = match text(call, "name") {
        "read_file"
        | "list_files"
        | "write_file"
        | "delete_file"
        | "switch_workspace"
        | "request_workspace_access" => &["path"],
        "edit_file" => &["path", "expected_sha256", "old_text", "new_text"],
        "search_text" => &["path", "query"],
        "report_friction" => &["category", "summary"],
        "exec" => &["cwd"],
        "echo" => &["text"],
        _ => &[],
    };
    if fields
        .iter()
        .any(|f| !a[*f].is_null() && !a[*f].is_string())
    {
        return None;
    }
    if text(call, "name") == "exec" {
        let strings = |v: &Value| {
            v.is_null()
                || v.as_array()
                    .is_some_and(|a| a.iter().all(|v| v.is_string() || v.is_null()))
        };
        if !strings(&a["argv"])
            || (!a["then"].is_null()
                && !a["then"].as_array().is_some_and(|a| a.iter().all(strings)))
        {
            return None;
        }
    }
    Some(a)
}

fn go_value(v: &Value) -> String {
    match v {
        Value::Null => "<nil>".into(),
        Value::String(s) => s.clone(),
        Value::Array(a) => format!("[{}]", a.iter().map(go_value).collect::<Vec<_>>().join(" ")),
        Value::Object(o) => {
            let mut keys: Vec<_> = o.keys().collect();
            keys.sort();
            format!(
                "map[{}]",
                keys.iter()
                    .map(|k| format!("{k}:{}", go_value(&o[*k])))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
        _ => v.to_string(),
    }
}

fn argument_summary(raw: &str) -> String {
    let parsed = serde_json::from_str::<Value>(raw).ok();
    if let Some(o) = parsed.as_ref().and_then(Value::as_object) {
        let mut keys: Vec<_> = o.keys().collect();
        keys.sort();
        return keys
            .into_iter()
            .filter(|k| {
                !matches!(
                    k.as_str(),
                    "content" | "old_text" | "new_text" | "edits" | "append_text"
                )
            })
            .filter_map(|k| {
                let value = go_value(&o[k]);
                (value.len() <= 60).then(|| format!("{k}={value}"))
            })
            .collect::<Vec<_>>()
            .join(" ");
    }
    if parsed.is_some_and(|v| v.is_null()) {
        return String::new();
    }
    let mut end = raw.len().min(60);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    raw[..end].into()
}

fn one_row(s: &str) -> String {
    sanitize(s).replace('\n', "↵")
}

fn command(a: &Value) -> String {
    let mut result = a["argv"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            let original = v.as_str().unwrap_or("");
            let safe = one_row(original);
            if original.contains('\n')
                || safe.is_empty()
                || safe.chars().any(|c| " $`\\\"'*?[]{}()<>|&;".contains(c))
            {
                format!("'{}'", safe.replace('\'', "'\\''"))
            } else {
                safe
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    for argv in a["then"].as_array().into_iter().flatten() {
        result.push_str(&format!(" && {}", command(&json!({"argv":argv}))));
    }
    if !text(a, "cwd").is_empty() && text(a, "cwd") != "." {
        result.push_str(&format!(" in {}", one_row(text(a, "cwd"))));
    }
    result
}

/// Call target describes the command shown during live tool progress.
pub(super) fn call_target(call: &Value) -> String {
    target(call, args(call).as_ref())
}

fn target(call: &Value, a: Option<&Value>) -> String {
    if let Some(a) = a {
        match text(call, "name") {
            "read_file"
            | "list_files"
            | "edit_file"
            | "write_file"
            | "delete_file"
            | "switch_workspace"
            | "request_workspace_access" => return sanitize(text(a, "path")),
            "search_text" => {
                let delimiter = if yes(a, "regex") { "/" } else { "\"" };
                return format!(
                    "{delimiter}{}{delimiter} in {}",
                    one_row(text(a, "query")),
                    sanitize(text(a, "path"))
                );
            }
            "exec" => return command(a),
            "report_friction" => {
                return sanitize(&format!("{}: {}", text(a, "category"), text(a, "summary")))
            }
            "echo" => return format!("\"{}\"", sanitize(text(a, "text"))),
            _ => {}
        }
    }
    truncate(&sanitize(&argument_summary(text(call, "arguments"))), 160)
}

fn elapsed(ms: i64) -> String {
    if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.)
    } else {
        format!("{}m{:02}s", ms / 60_000, ms / 1000 % 60)
    }
}

fn edit_summary(data: &Value) -> String {
    let mut parts = Vec::new();
    if number(data, "edits") > 0 {
        parts.push(plural(number(data, "edits"), "edit", "edits"));
    }
    if number(data, "appended_bytes") > 0 {
        parts.push("append".into());
    }
    parts.join(" + ")
}

fn recap(call: &Value, outcome: &Value, target: &str, a: Option<&Value>) -> String {
    if text(outcome, "effect") == "none" {
        return String::new();
    }
    let data = &outcome["data"];
    match text(call, "name") {
        "edit_file" => {
            let summary = edit_summary(data);
            if !summary.is_empty()
                && a.is_some_and(|a| a.get("edits").is_some() || a.get("append_text").is_some())
            {
                return format!("changed {target} ({summary})");
            }
            format!("changed {target}")
        }
        "write_file" => format!(
            "{} {target}",
            if text(data, "operation") == "create" {
                "created"
            } else {
                "replaced"
            }
        ),
        "delete_file" if yes(data, "recursive") => {
            let mut s = format!(
                "deleted {} paths under {target}",
                number(data, "removed_count")
            );
            if !yes(outcome, "ok") {
                s.push_str(&format!(
                    " (partial; stopped at {})",
                    sanitize(text(data, "failed_path"))
                ));
            }
            s
        }
        "delete_file" => format!("deleted {target}"),
        "exec" => format!("ran {target}"),
        _ => String::new(),
    }
}

fn activity_text(
    call: &Value,
    outcome: &Value,
    state: &mut Value,
    styled: bool,
    columns: usize,
) -> String {
    if !state["_display"].is_object() {
        begin_turn(state);
    }
    let name = text(call, "name");
    let a = args(call);
    if yes(outcome, "ok") {
        match name {
            "read_file" if a.is_some() => {
                let key = json!([outcome["workspace"], text(a.as_ref().unwrap(), "path")]);
                let reads = state["_display"]["reads"].as_array_mut().unwrap();
                if !reads.contains(&key) {
                    reads.push(key);
                }
                return String::new();
            }
            "list_files" | "search_text" => {
                let key = if name == "list_files" {
                    "listings"
                } else {
                    "searches"
                };
                state["_display"][key] = json!(number(&state["_display"], key) + 1);
                return String::new();
            }
            _ => {}
        }
    }
    let original_target = target(call, a.as_ref());
    let mut target = original_target.clone();
    let (mut mark, mut color) = if yes(outcome, "ok") {
        ("✓", "\x1b[32m")
    } else {
        ("✗", "\x1b[31m")
    };
    let code = text(outcome, "code");
    let mut result = String::new();
    let mut previews = Vec::new();
    if code == "not_executed" {
        mark = "–";
        color = DIM;
        result = format!("not run: {}", sanitize(text(outcome, "message")));
    } else {
        if !yes(outcome, "ok") {
            result = truncate(
                &sanitize(&format!("{code}: {}", text(outcome, "message"))),
                120,
            );
        }
        if yes(outcome, "ok") || name == "exec" {
            let data = &outcome["data"];
            match name {
                "write_file" => match text(data, "operation") {
                    "create" => {
                        let n = number(data, "size_bytes");
                        let size = if n < 1000 {
                            format!("{n} B")
                        } else if n < 1_000_000 {
                            format!("{:.1} KB", n as f64 / 1000.)
                        } else {
                            format!("{:.1} MB", n as f64 / 1e6)
                        };
                        target.push_str(&format!(" (created, {size})"));
                    }
                    "overwrite" => target.push_str(" (replaced)"),
                    _ => {}
                },
                "edit_file" => {
                    if let Some(a) = a.as_ref() {
                        if !yes(data, "changed") {
                            result = "no change".into();
                        } else if a.get("edits").is_some() || a.get("append_text").is_some() {
                            result = edit_summary(data);
                        } else {
                            let mut old: Vec<_> = text(a, "old_text").split('\n').collect();
                            let mut new: Vec<_> = text(a, "new_text").split('\n').collect();
                            while !old.is_empty() && !new.is_empty() && old.first() == new.first() {
                                old.remove(0);
                                new.remove(0);
                            }
                            while !old.is_empty() && !new.is_empty() && old.last() == new.last() {
                                old.pop();
                                new.pop();
                            }
                            result = format!("+{} -{}", new.len(), old.len());
                            for (sign, lines) in [("-", old), ("+", new)] {
                                for s in lines.iter().take(6) {
                                    previews.push(format!(
                                        "{sign} {}",
                                        sanitize(&s.replace('\t', "    "))
                                    ));
                                }
                                if lines.len() > 6 {
                                    previews.push(format!(
                                        "… {}",
                                        plural((lines.len() - 6) as i64, "more line", "more lines")
                                    ));
                                }
                            }
                        }
                    }
                }
                "exec" if a.is_some() && (data.is_object() || data.is_null()) => {
                    let steps = data["steps"].as_array();
                    let chained = a.as_ref().unwrap()["then"]
                        .as_array()
                        .is_some_and(|a| !a.is_empty())
                        && steps.is_some_and(|s| !s.is_empty());
                    let last = if chained {
                        steps.unwrap().last().unwrap()
                    } else {
                        data
                    };
                    let ms = if chained {
                        steps
                            .unwrap()
                            .iter()
                            .map(|s| number(s, "duration_ms"))
                            .sum()
                    } else {
                        number(data, "duration_ms")
                    };
                    match code {
                        "timeout" => {
                            mark = "!";
                            color = "\x1b[33m";
                            result = format!("timed out after {}; effects unknown", elapsed(ms));
                        }
                        "cancelled" => {
                            mark = "!";
                            color = "\x1b[33m";
                            result = "cancelled; effects unknown".into();
                        }
                        _ => {
                            if let Some(signal) = last["signal"].as_str() {
                                mark = "✗";
                                color = "\x1b[31m";
                                result = format!("signal: {} in {}", sanitize(signal), elapsed(ms));
                            } else if let Some(exit) = last["exit_code"].as_i64() {
                                result = format!("exit {exit} in {}", elapsed(ms));
                            }
                            let skipped = data["not_run"].as_array().map_or(0, Vec::len);
                            if chained && skipped > 0 {
                                result.push_str(&format!(
                                    " at step {} of {}",
                                    steps.unwrap().len(),
                                    steps.unwrap().len() + skipped
                                ));
                            }
                        }
                    }
                }
                "switch_workspace" => {
                    target = sanitize(text(data, "workspace"));
                    result = if yes(data, "changed") {
                        "selected"
                    } else {
                        "already active"
                    }
                    .into();
                }
                "request_workspace_access" => result = "approved for this conversation".into(),
                "echo" if a.is_some() => {
                    target = serde_json::to_string(&sanitize(text(a.as_ref().unwrap(), "text")))
                        .unwrap()
                        .replace('\u{2028}', "\\u2028")
                        .replace('\u{2029}', "\\u2029")
                }
                _ => {}
            }
        }
    }
    let effect = recap(call, outcome, &original_target, a.as_ref());
    if !effect.is_empty() {
        state["_display"]["recap"]
            .as_array_mut()
            .unwrap()
            .push(json!(effect));
    }
    let prefix = format!("  {mark} {} ", sanitize(name));
    if columns == 0 {
        target = truncate(&target, 160);
    } else {
        let available =
            columns.saturating_sub(width(&prefix) + if result.is_empty() { 0 } else { 3 });
        if available < 1 {
            target.clear();
            result.clear();
        } else if width(&target) + width(&result) > available {
            let budget = (available - width(&result).min(available / 3)).max(1);
            target = truncate(&target, budget);
            let remaining = available.saturating_sub(width(&target));
            if remaining == 0 {
                result.clear();
            } else {
                result = truncate(&result, remaining);
            }
        }
    }
    let mut out = if styled {
        format!("  {color}{mark}{RESET} {} {target}", sanitize(name))
    } else {
        format!("{prefix}{target}")
    };
    if !result.is_empty() {
        out.push_str(&if styled {
            format!("{DIM} → {result}{RESET}")
        } else {
            format!(" → {result}")
        });
    }
    out.push('\n');
    for p in previews {
        let p = truncate(
            &p,
            if columns == 0 {
                160
            } else {
                columns.saturating_sub(6).max(1)
            },
        );
        let color = if p.starts_with("- ") {
            "\x1b[31m"
        } else if p.starts_with("+ ") {
            "\x1b[32m"
        } else {
            ""
        };
        out.push_str(&if styled && !color.is_empty() {
            format!("      {color}{p}{RESET}\n")
        } else {
            format!("      {p}\n")
        });
    }
    state["_display"]["printed"] = json!(true);
    out
}

pub(super) fn browsing_summary(state: &Value, styled: bool) -> String {
    let d = &state["_display"];
    let mut parts = Vec::new();
    let reads = d["reads"].as_array().map_or(0, Vec::len);
    if reads > 0 {
        parts.push(format!("read {}", plural(reads as i64, "file", "files")));
    }
    for (key, one, many) in [
        ("searches", "search", "searches"),
        ("listings", "listing", "listings"),
    ] {
        if number(d, key) > 0 {
            parts.push(plural(number(d, key), one, many));
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    if styled {
        format!("  \x1b[32m✓{RESET}{DIM} {}{RESET}\n", parts.join(" · "))
    } else {
        format!("  ✓ {}\n", parts.join(" · "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn echo_quotes_unicode_line_separators() {
        let mut state = json!({});
        let call = json!({"name":"echo","arguments":json!({"text":"a\u{2028}\u{2029}\n\t\"\\"}).to_string()});
        let out = activity_text(
            &call,
            &json!({"ok":true,"effect":"none"}),
            &mut state,
            false,
            0,
        );
        assert_eq!(out, "  ✓ echo \"a\\u2028\\u2029\\n\\t\\\"\\\\\"\n");
    }

    #[test]
    fn invalid_argument_activity_matches_reference() {
        let mut state = json!({});
        let outcome = json!({"ok":false,"code":"invalid_arguments","message":"unknown field \"unrecognized\"","effect":"none"});
        for (name, arguments, target) in [
            ("delete_file", r#"{"path":"f","unrecognized":true}"#, "f"),
            ("read_file", "[]", "[]"),
            ("read_file", r#"{"path":42}"#, "path=42"),
            ("read_file", r#"{"path":"f"} {}"#, r#"{"path":"f"} {}"#),
            ("echo", "null", "\"\""),
            ("search_text", "{}", "\"\" in "),
            (
                "exec",
                r#"{"argv":["printf","a b"],"cwd":"."}"#,
                "printf 'a b'",
            ),
        ] {
            let call = json!({"name":name,"arguments":arguments});
            assert_eq!(
                activity_text(&call, &outcome, &mut state, false, 0),
                format!(
                    "  ✗ {name} {target} → invalid_arguments: unknown field \"unrecognized\"\n"
                )
            );
        }
    }
    #[test]
    fn browsing_is_deduplicated_per_workspace_and_reset_per_turn() {
        let mut state = json!({});
        let call = json!({"name":"read_file","arguments":"{\"path\":\"f\"}"});
        for workspace in ["a", "a", "b"] {
            assert_eq!(
                activity_text(
                    &call,
                    &json!({"ok":true,"workspace":workspace}),
                    &mut state,
                    false,
                    0
                ),
                ""
            );
        }
        assert_eq!(browsing_summary(&state, false), "  ✓ read 2 files\n");
        begin_turn(&mut state);
        assert_eq!(browsing_summary(&state, false), "");
    }
    #[test]
    fn effects_commands_and_previews_keep_activity_order() {
        let mut state = json!({});
        let call = json!({"name":"edit_file","arguments":"{\"path\":\"f\",\"old_text\":\"a\\n\",\"new_text\":\"b\\n\"}"});
        assert_eq!(
            activity_text(
                &call,
                &json!({"ok":true,"effect":"applied","data":{"changed":true}}),
                &mut state,
                false,
                0
            ),
            "  ✓ edit_file f → +1 -1\n      - a\n      + b\n"
        );
        assert_eq!(state["_display"]["recap"], json!(["changed f"]));
        let call = json!({"name":"exec","arguments":"{\"argv\":[\"sleep\",\"30\"]}"});
        assert_eq!(
            activity_text(
                &call,
                &json!({"ok":false,"code":"timeout","effect":"unknown","data":{"duration_ms":10000}}),
                &mut state,
                false,
                0
            ),
            "  ! exec sleep 30 → timed out after 10.0s; effects unknown\n"
        );
        let out = activity_text(
            &call,
            &json!({"code":"not_executed","message":"budget","effect":"none"}),
            &mut state,
            true,
            40,
        );
        assert!(out.contains("\x1b[2m–\x1b[0m"));
        assert!(out.ends_with("budget\x1b[0m\n"));
    }
}
