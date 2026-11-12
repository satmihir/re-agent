use crate::cli::{text, yes};
use crate::render::{sanitize, truncate, width};
use crate::workspace::{Workspace, MAX_FILE_BYTES as SNAPSHOT_BYTES};
use serde_json::{json, Value};
use std::path::Path;

const TURN_BYTES: usize = 8 << 20;
const TURN_FILES: usize = 128;

/// Capturing includes silent children of a displayed root without displaying their tools.
pub(crate) fn capturing(config: &Value) -> bool {
    yes(config, "_display") || yes(config, "_capture_file_changes")
}

/// capture retains only validated, published bytes for human presentation.
pub(crate) fn capture(
    ws: &mut Workspace,
    config: &Value,
    path: &Path,
    before: Option<&str>,
    after: Option<&str>,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
) {
    if !capturing(config) {
        return;
    }
    if ws.file_previews.len() >= TURN_FILES {
        if ws.file_previews.len() == TURN_FILES {
            crate::agents::publish_file_change(config, json!({"omitted":true}));
            ws.file_previews.push(json!({"omitted":true}));
        }
        return;
    }
    let identity = std::fs::canonicalize(path).unwrap_or_else(|_| {
        path.parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .and_then(|p| path.file_name().map(|name| p.join(name)))
            .unwrap_or_else(|| path.to_path_buf())
    });
    let bytes: usize = ws
        .file_previews
        .iter()
        .map(|f| text(f, "old").len() + text(f, "new").len())
        .sum();
    let (mut old, mut new) = (
        content(before.is_some(), old),
        content(after.is_some(), new),
    );
    if bytes + old.map_or(0, str::len) + new.map_or(0, str::len) > TURN_BYTES {
        old = None;
        new = None;
    }
    let change = json!({"workspace":ws.root,"identity":identity,"path":ws.relative(path),
        "before":before,"after":after,"old":old,"new":new});
    crate::agents::publish_file_change(config, change.clone());
    ws.file_previews.push(change);
}

fn content(exists: bool, bytes: Option<&[u8]>) -> Option<&str> {
    if !exists {
        return Some("");
    }
    bytes
        .filter(|b| b.len() <= SNAPSHOT_BYTES && !b.contains(&0))
        .and_then(|b| std::str::from_utf8(b).ok())
}

/// accept merges publication snapshots into this turn's net file changes.
pub(crate) fn accept(state: &mut Value, changes: Vec<Value>) {
    if !state["_display"].is_object() {
        crate::render::begin_turn(state);
    }
    state["_display"]["file_change"] = Value::Null;
    if changes.is_empty() {
        return;
    }
    if !state["_display"]["files"].is_array() {
        state["_display"]["files"] = json!([]);
    }
    let immediate = changes.last().cloned().unwrap_or(Value::Null);
    let display = &mut state["_display"];
    let files = display["files"].as_array_mut().unwrap();
    let mut omitted = false;
    for mut change in changes {
        if yes(&change, "omitted") {
            omitted = true;
            continue;
        }
        let position = files.iter().position(|f| {
            f["workspace"] == change["workspace"]
                && if change["identity"].is_string() {
                    f["identity"] == change["identity"]
                } else {
                    f["path"] == change["path"]
                }
        });
        if let Some(i) = position {
            let previous = files.remove(i);
            if previous["after"] != change["before"] || yes(&previous, "gap") {
                change["gap"] = json!(true);
            }
            change["before"] = previous["before"].clone();
            change["old"] = previous["old"].clone();
        }
        if change["before"] == change["after"] && !yes(&change, "gap") {
            continue;
        }
        if files.len() >= TURN_FILES {
            omitted = true;
            continue;
        }
        let bytes: usize = files
            .iter()
            .map(|f| text(f, "old").len() + text(f, "new").len())
            .sum();
        if bytes + text(&change, "old").len() + text(&change, "new").len() > TURN_BYTES {
            change["old"] = Value::Null;
            change["new"] = Value::Null;
        }
        files.push(change);
    }
    if omitted {
        display["files_omitted"] = json!(true);
    }
    display["file_change"] = immediate;
}

fn patch(change: &Value) -> Option<String> {
    if yes(change, "gap") {
        return None;
    }
    let (Some(old), Some(new)) = (change["old"].as_str(), change["new"].as_str()) else {
        return None;
    };
    crate::tool_git::diff::unified_context(old.as_bytes(), new.as_bytes(), "old", "new", 2)
}

fn counts(change: &Value) -> Option<(usize, usize)> {
    let patch = patch(change)?;
    let mut added = 0;
    let mut removed = 0;
    for line in patch.lines().skip(2) {
        added += usize::from(line.starts_with('+'));
        removed += usize::from(line.starts_with('-'));
    }
    Some((added, removed))
}

/// totals describes net file changes, marking unavailable line counts explicitly.
pub(crate) fn totals(state: &Value, styled: bool) -> String {
    let display = &state["_display"];
    let files = display["files"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if files.is_empty() && !yes(display, "files_omitted") {
        return String::new();
    }
    let (mut added, mut removed, mut partial) = (0, 0, yes(display, "files_omitted"));
    for file in files {
        if let Some((a, r)) = counts(file) {
            added += a;
            removed += r;
        } else {
            partial = true;
        }
    }
    let label = if yes(display, "files_omitted") {
        format!("{}+ files changed", files.len())
    } else {
        crate::render::plural(files.len() as i64, "file changed", "files changed")
    };
    let detail = format!(
        "{label} · +{added} −{removed}{}",
        if partial {
            " (partial; diff unavailable for some files)"
        } else {
            ""
        }
    );
    if styled {
        format!(
            "  {}{detail}{}\n",
            crate::render::palette::DIM,
            crate::render::palette::RESET
        )
    } else {
        format!("  {detail}\n")
    }
}

fn fallback(change: &Value) -> &'static str {
    if yes(change, "gap") {
        return "diff unavailable: file changed outside the captured tool publications";
    }
    "diff unavailable: uncaptured, binary, oversized, or diff budget exceeded"
}

/// preview renders numbered interleaved hunks with an optional row limit.
pub(crate) fn preview(change: &Value, styled: bool, columns: usize, limit: usize) -> String {
    if !change.is_object() {
        return String::new();
    }
    let Some(patch) = patch(change) else {
        return format!(
            "      {}\n",
            truncate(
                fallback(change),
                if columns == 0 {
                    160
                } else {
                    columns.saturating_sub(6).max(1)
                }
            )
        );
    };
    let mut rows = Vec::new();
    let (mut old, mut new) = (0, 0);
    for line in patch.lines().skip(2) {
        if line.starts_with("@@ ") {
            let mut fields = line.split_whitespace();
            fields.next();
            let number = |s: &str| {
                s[1..]
                    .split(',')
                    .next()
                    .unwrap_or("0")
                    .parse::<usize>()
                    .unwrap_or(0)
            };
            old = number(fields.next().unwrap_or("-0"));
            new = number(fields.next().unwrap_or("+0"));
            rows.push((' ', line.to_string()));
            continue;
        }
        if line.starts_with('\\') {
            rows.push((' ', line.to_string()));
            continue;
        }
        let sign = line.chars().next().unwrap_or(' ');
        let left = if sign == '+' {
            String::new()
        } else {
            old.to_string()
        };
        let right = if sign == '-' {
            String::new()
        } else {
            new.to_string()
        };
        rows.push((
            sign,
            format!(
                "{left:>4} {right:>4} {sign} {}",
                sanitize(&line[1..]).replace('\t', "    ")
            ),
        ));
        old += usize::from(sign != '+');
        new += usize::from(sign != '-');
    }
    let budget = if columns == 0 {
        160
    } else {
        columns.saturating_sub(6).max(1)
    };
    let mut out = String::new();
    for (sign, row) in rows.iter().take(limit) {
        let row = if limit == usize::MAX {
            row.clone()
        } else {
            truncate(row, budget)
        };
        let colour = match sign {
            '-' => crate::render::palette::DIFF_REMOVED,
            '+' => crate::render::palette::DIFF_ADDED,
            _ => "",
        };
        if styled && !colour.is_empty() {
            let padding = " ".repeat(budget.saturating_sub(width(&row)));
            out += &format!(
                "      {colour}{row}{padding}{}\n",
                crate::render::palette::RESET
            );
        } else {
            out += &format!("      {row}\n");
        }
    }
    if rows.len() > limit {
        out += &format!("      … {} more lines\n", rows.len() - limit);
    }
    out
}

/// full renders this opening's last turn using the same hunks without the preview cap.
pub(crate) fn full(state: &Value, styled: bool, columns: usize) -> String {
    let files = state["_display"]["files"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if files.is_empty() && !yes(&state["_display"], "files_omitted") {
        return "No file changes in the last turn.".into();
    }
    let mut out = totals(state, styled);
    let roots: std::collections::HashSet<_> = files.iter().map(|f| text(f, "workspace")).collect();
    for file in files {
        let label = if roots.len() > 1 {
            format!("{} [{}]", text(file, "path"), text(file, "workspace"))
        } else {
            text(file, "path").to_string()
        };
        out += &format!(
            "  {}\n",
            sanitize(&label).replace('\n', "↵").replace('\t', "⇥")
        );
        out += &preview(file, styled, columns, usize::MAX);
    }
    out.trim_end_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(ws: &mut Workspace, state: &mut Value, name: &str, args: Value) -> Value {
        let outcome =
            crate::tool_files::execute(ws, &json!({"_display":true}), name, &args).unwrap();
        accept(state, std::mem::take(&mut ws.file_previews));
        outcome
    }

    #[test]
    fn actual_publications_cover_multi_edit_overwrite_append_and_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let alias = dir.path().join("alias");
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        std::fs::write(root.join("f"), "header\none\ncontext\ntwo\nfooter\n").unwrap();
        let mut ws = Workspace::open(alias.to_str().unwrap()).unwrap();
        let mut state = json!({"_display":{}});
        tool(&mut ws, &mut state, "read_file", json!({"path":"f"}));
        tool(
            &mut ws,
            &mut state,
            "edit_file",
            json!({"path":"f","edits":[{"old_text":"one","new_text":"ONE"},{"old_text":"two","new_text":"TWO"}]}),
        );
        let diff = full(&state, false, 120);
        assert!(diff.contains("1 file changed · +2 −2"), "{diff}");
        assert!(diff.contains("   1    1   header"), "{diff}");
        assert!(diff.contains("   2      - one"), "{diff}");
        assert!(diff.contains("        2 + ONE"), "{diff}");
        assert!(diff.find("- one").unwrap() < diff.find("+ ONE").unwrap());
        assert!(diff.find("+ ONE").unwrap() < diff.find("- two").unwrap());
        tool(
            &mut ws,
            &mut state,
            "write_file",
            json!({"path":"f","content":"replacement\n"}),
        );
        assert_eq!(
            state["_display"]["file_change"]["old"],
            "header\nONE\ncontext\nTWO\nfooter\n"
        );
        assert!(full(&state, false, 120).contains("1 file changed · +1 −5"));
        tool(
            &mut ws,
            &mut state,
            "edit_file",
            json!({"path":"f","append_text":"last\n"}),
        );
        tool(
            &mut ws,
            &mut state,
            "write_file",
            json!({"path":"created","content":"new\n"}),
        );
        assert!(full(&state, false, 120).contains("2 files changed · +3 −5"));
        assert_eq!(
            state["_display"]["files"][0]["workspace"],
            root.canonicalize().unwrap().to_str().unwrap()
        );
    }

    #[test]
    fn alias_writes_and_recursive_create_delete_cancel_from_net_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::os::unix::fs::symlink("real", dir.path().join("alias")).unwrap();
        std::fs::write(dir.path().join("real/f"), "a\n").unwrap();
        let mut ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let mut state = json!({"_display":{}});
        tool(&mut ws, &mut state, "read_file", json!({"path":"real/f"}));
        tool(
            &mut ws,
            &mut state,
            "write_file",
            json!({"path":"real/f","content":"b\n"}),
        );
        tool(
            &mut ws,
            &mut state,
            "write_file",
            json!({"path":"alias/f","content":"a\n"}),
        );
        assert_eq!(totals(&state, false), "");
        tool(
            &mut ws,
            &mut state,
            "write_file",
            json!({"path":"new/f","content":"new\n"}),
        );
        tool(
            &mut ws,
            &mut state,
            "delete_file",
            json!({"path":"new","recursive":true}),
        );
        assert_eq!(totals(&state, false), "");
        assert_eq!(
            full(&state, false, 100),
            "No file changes in the last turn."
        );
    }

    #[test]
    fn repeated_edits_are_net_and_create_delete_or_revert_disappear() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), "a\n").unwrap();
        let mut ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let mut state = json!({"_display":{}});
        tool(&mut ws, &mut state, "read_file", json!({"path":"f"}));
        for content in ["b\n", "c\n"] {
            tool(
                &mut ws,
                &mut state,
                "write_file",
                json!({"path":"f","content":content}),
            );
        }
        assert_eq!(totals(&state, false), "  1 file changed · +1 −1\n");
        tool(
            &mut ws,
            &mut state,
            "write_file",
            json!({"path":"f","content":"a\n"}),
        );
        assert_eq!(
            full(&state, false, 120),
            "No file changes in the last turn."
        );
        tool(
            &mut ws,
            &mut state,
            "write_file",
            json!({"path":"new","content":"x\n"}),
        );
        tool(&mut ws, &mut state, "delete_file", json!({"path":"new"}));
        assert!(totals(&state, false).is_empty());
        tool(
            &mut ws,
            &mut state,
            "write_file",
            json!({"path":"empty","content":""}),
        );
        assert_eq!(totals(&state, false), "  1 file changed · +0 −0\n");
    }

    #[test]
    fn full_diff_keeps_long_line_tails_that_the_preview_clips() {
        let line = format!("{}TAIL", "x".repeat(200));
        let change = json!({"old":"","new":line});
        assert!(!preview(&change, false, 40, 12).contains("TAIL"));
        assert!(preview(&change, false, 40, usize::MAX).contains("TAIL"));
    }

    #[test]
    fn preview_caps_rows_but_full_diff_preserves_all_hunks_without_colour() {
        let change = json!({"path":"new","workspace":"root","before":null,"after":"digest","old":"","new":(1..=30).map(|i|format!("row{i}\n")).collect::<String>()});
        let shown = preview(&change, true, 80, 12);
        assert!(shown.contains("… 19 more lines"), "{shown}");
        assert!(shown.contains(crate::render::palette::DIFF_ADDED));
        let state = json!({"_display":{"files":[change]}});
        let full = full(&state, false, 80);
        assert!(full.contains("+ row30"));
        assert!(!full.contains("more lines"));
        assert!(!full.contains('\x1b'));
        let deletion = json!({"old":"x\n","new":""});
        assert!(preview(&deletion, true, 80, 12).contains(crate::render::palette::DIFF_REMOVED));
    }

    #[test]
    fn stale_checks_cannot_publish_previews_and_snapshot_gaps_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), "a\n").unwrap();
        let mut ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let mut state = json!({"_display":{}});
        tool(&mut ws, &mut state, "read_file", json!({"path":"f"}));
        tool(
            &mut ws,
            &mut state,
            "write_file",
            json!({"path":"f","content":"b\n"}),
        );
        std::fs::write(dir.path().join("f"), "external\n").unwrap();
        let error = crate::tool_files::execute(
            &mut ws,
            &json!({"_display":true}),
            "edit_file",
            &json!({"path":"f","old_text":"external","new_text":"bad"}),
        )
        .unwrap_err();
        assert_eq!(error["code"], "stale_file");
        assert!(ws.file_previews.is_empty());
        tool(&mut ws, &mut state, "read_file", json!({"path":"f"}));
        tool(
            &mut ws,
            &mut state,
            "write_file",
            json!({"path":"f","content":"c\n"}),
        );
        assert!(full(&state, false, 120).contains("outside the captured"));
        assert!(totals(&state, false).contains("partial"));
    }

    #[test]
    fn binary_and_large_deletions_have_bounded_partial_summaries() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let mut state = json!({"_display":{}});
        for (name, bytes) in [
            ("binary", vec![0, 255]),
            ("large", vec![b'x'; SNAPSHOT_BYTES + 1]),
        ] {
            std::fs::write(dir.path().join(name), &bytes).unwrap();
            tool(
                &mut ws,
                &mut state,
                "delete_file",
                json!({"path":name,"expected_sha256":crate::workspace::digest(&bytes)}),
            );
        }
        let diff = full(&state, false, 120);
        assert!(diff.contains("2 files changed"));
        assert!(diff.contains("partial"));
        assert_eq!(diff.matches("diff unavailable:").count(), 2);
        assert!(diff.len() < 600);
    }

    #[test]
    fn turn_snapshot_limits_retain_explicit_fallbacks() {
        let mut state = json!({"_display":{}});
        let content = "a".repeat(SNAPSHOT_BYTES);
        for i in 0..TURN_FILES + 1 {
            accept(
                &mut state,
                vec![
                    json!({"workspace":"root","path":i.to_string(),"before":null,"after":"digest","old":"","new":content}),
                ],
            );
        }
        assert_eq!(
            state["_display"]["files"].as_array().unwrap().len(),
            TURN_FILES
        );
        assert_eq!(state["_display"]["files_omitted"], true);
        assert!(state["_display"]["files"][8]["new"].is_null());
        assert!(totals(&state, false).contains("partial"));
    }
}
