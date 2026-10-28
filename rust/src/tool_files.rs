use crate::tools::{fail, optional_bool, optional_int};
use crate::workspace::*;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub fn execute(
    ws: &mut Workspace,
    config: &Value,
    name: &str,
    args: &Value,
) -> Result<Value, Value> {
    match name {
        "read_file" => read_file(ws, config, args),
        "list_files" => list_files(ws, args),
        "search_text" => search_text(ws, args),
        "write_file" => write_file(ws, config, args),
        "edit_file" => edit_file(ws, args),
        "delete_file" => delete_file(ws, args),
        _ => Err(fail("tool_unavailable", &format!("no tool named {name}"))),
    }
}

fn path(args: &Value) -> &str {
    args["path"].as_str().unwrap_or("")
}

fn read_file(ws: &mut Workspace, config: &Value, args: &Value) -> Result<Value, Value> {
    let start = optional_int(args, "start_line", 1, 1)?;
    let limit = optional_int(args, "max_lines", 250, 1)?;
    let abs = ws.resolve(path(args))?;
    let snap = match read_snapshot(&abs) {
        Ok(snap) => snap,
        Err(bad) => {
            if config["agents"].as_bool().unwrap_or(false) && bad["code"] == "not_found" {
                ws.seen.remove(&missing_path(&abs));
            }
            return Err(bad);
        }
    };
    let total = snap.lines.len();
    if start > total && !(start == 1 && total == 0) {
        return Err(fail(
            "line_out_of_range",
            &format!("file has {total} lines"),
        ));
    }
    let count = total.saturating_sub(start - 1).min(limit);
    let build = |n: usize| {
        let end = start - 1 + n;
        json!({"path":ws.relative(&abs),"sha256":snap.sha256,"size_bytes":snap.content.len(),"total_lines":total,
            "lines":snap.lines.iter().skip(start-1).take(n).enumerate().map(|(i,s)|json!({"number":start+i,"text":s})).collect::<Vec<_>>(),
            "next_line":if end<total {Some(end+1)}else{None},"eof":end==total})
    };
    let shown = fit_elements(ws, count, build);
    if shown == 0 && count > 0 {
        return Err(fail(
            "line_too_long",
            &format!("line {start} alone does not fit in one result"),
        ));
    }
    let mut out = workspace_outcome(ws, build(shown), "none");
    out["truncated"] = json!(start - 1 + shown < total);
    ws.remember(&abs, &snap.sha256);
    Ok(out)
}

fn list_files(ws: &Workspace, args: &Value) -> Result<Value, Value> {
    let offset = optional_int(args, "offset", 0, 0)?;
    let limit = optional_int(args, "limit", usize::MAX, 1)?;
    let abs = ws.resolve(path(args))?;
    if !fs::metadata(&abs).map_err(os_outcome)?.is_dir() {
        return Err(fail("not_directory", "path is not a directory"));
    }
    let mut dir = fs::read_dir(&abs)
        .map_err(os_outcome)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(os_outcome)?;
    dir.sort_by_key(|e| e.file_name());
    let mut skipped = 0;
    let mut entries = vec![];
    for entry in dir {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            skipped += 1;
            continue;
        };
        if withheld(name) {
            skipped += 1;
            continue;
        }
        let typ = entry.file_type().map_err(os_outcome)?;
        entries.push(json!({"path":ws.relative(&entry.path()),"name":name,"kind":if typ.is_symlink(){"symlink"}else if typ.is_dir(){"dir"}else{"file"}}));
    }
    let count = entries.len().saturating_sub(offset).min(limit);
    let build = |n: usize| json!({"path":path(args),"entries":entries.iter().skip(offset).take(n).collect::<Vec<_>>(),"next_offset":if offset+n<entries.len(){Some(offset+n)}else{None},"skipped_entries":skipped,"complete":true});
    let shown = fit_elements(ws, count, build);
    let mut out = workspace_outcome(ws, build(shown), "none");
    out["truncated"] = json!(offset + shown < entries.len());
    Ok(out)
}

struct Scan {
    matches: Vec<Value>,
    files: usize,
    bytes: usize,
    skipped: usize,
    complete: bool,
    reason: Option<String>,
    seen: HashSet<PathBuf>,
    limit: usize,
}
impl Scan {
    fn full(&self) -> bool {
        self.matches.len() >= self.limit
    }
    fn skip(&mut self) {
        self.skipped += 1;
        self.complete = false;
    }
    fn stop(&mut self, reason: &str) {
        self.complete = false;
        if self.reason.is_none() {
            self.reason = Some(reason.into());
        }
    }
    fn data(&self, n: usize) -> Value {
        json!({"matches":&self.matches[..n],"files_scanned":self.files,"bytes_scanned":self.bytes,"skipped_files":self.skipped,"complete":self.complete,"stop_reason":self.reason})
    }
    fn scan(
        &mut self,
        ws: &Workspace,
        p: &Path,
        snap: Snapshot,
        query: &str,
        re: Option<&regex::Regex>,
    ) {
        self.files += 1;
        self.bytes += snap.content.len();
        for (i, line) in snap.lines.into_iter().enumerate() {
            if self.full() {
                self.stop("max_results");
                return;
            }
            if re.map_or_else(|| line.contains(query), |r| r.is_match(&line)) {
                self.matches.push(
                    json!({"path":ws.relative(p),"line":i+1,"text":line,"preview_truncated":false}),
                );
            }
        }
        if self.full() {
            self.stop("max_results");
        }
    }
    fn walk(
        &mut self,
        ws: &Workspace,
        p: &Path,
        query: &str,
        re: Option<&regex::Regex>,
        root: bool,
    ) {
        if self.full() {
            return;
        }
        let Ok(info) = fs::symlink_metadata(p) else {
            self.skip();
            return;
        };
        if info.is_dir() || root && fs::metadata(p).is_ok_and(|i| i.is_dir()) {
            let Ok(dir) = fs::read_dir(p) else {
                self.skip();
                return;
            };
            let mut dir = dir.collect::<Result<Vec<_>, _>>().unwrap_or_else(|_| {
                self.skip();
                vec![]
            });
            dir.sort_by_key(|e| e.file_name());
            for entry in dir {
                if self.full() {
                    break;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if withheld(&name) {
                    continue;
                }
                if entry.file_type().is_ok_and(|t| t.is_dir())
                    && ["node_modules", "vendor", ".venv", "venv", "dist", "build"]
                        .contains(&name.as_str())
                {
                    continue;
                }
                self.walk(ws, &entry.path(), query, re, false);
            }
            return;
        }
        if !info.is_file() {
            self.skip();
            return;
        }
        let Ok(canonical) = fs::canonicalize(p) else {
            self.skip();
            return;
        };
        if !self.seen.insert(canonical) {
            return;
        }
        match read_snapshot(p) {
            Ok(snap) => self.scan(ws, p, snap, query, re),
            Err(_) => self.skip(),
        }
    }
}

fn search_text(ws: &Workspace, args: &Value) -> Result<Value, Value> {
    let limit = optional_int(args, "max_results", usize::MAX, 1)?;
    let regex = optional_bool(args, "regex")?;
    let query = args["query"].as_str().unwrap_or("");
    if query.is_empty() {
        return Err(fail("invalid_arguments", "query must not be empty"));
    }
    if query.contains(['\n', '\r', '\0']) {
        return Err(fail(
            "invalid_arguments",
            "query must be one line without NUL bytes",
        ));
    }
    let re = if regex {
        let re = regex::Regex::new(query)
            .map_err(|e| fail("invalid_arguments", &crate::errors::regex_error(query, &e)))?;
        if re.is_match("") {
            return Err(fail(
                "invalid_arguments",
                "pattern matches the empty string, so it would match every line",
            ));
        }
        Some(re)
    } else {
        None
    };
    let has_path = args.get("path").is_some();
    let has_paths = args.get("paths").is_some();
    if has_path == has_paths {
        return Err(fail(
            "invalid_arguments",
            "provide exactly one of path or paths",
        ));
    }
    let mut paths = vec![path(args).to_string()];
    if has_paths {
        let paths_raw = args["paths"]
            .as_array()
            .filter(|p| !p.is_empty() && p.len() <= 20)
            .ok_or_else(|| {
                fail(
                    "invalid_arguments",
                    "paths must be an array of 1 to 20 strings",
                )
            })?;
        paths.clear();
        for (i, p) in paths_raw.iter().enumerate() {
            paths.push(
                p.as_str()
                    .ok_or_else(|| {
                        fail(
                            "invalid_arguments",
                            &format!("paths[{i}]: path must be a string"),
                        )
                    })?
                    .into(),
            );
        }
    }
    let mut targets = vec![];
    for (i, p) in paths.iter().enumerate() {
        let target: Result<(PathBuf, bool), Value> = (|| {
            let abs = ws.resolve(p)?;
            let info = fs::metadata(&abs).map_err(os_outcome)?;
            Ok((abs, info.is_dir()))
        })();
        match target {
            Ok(t) => targets.push(t),
            Err(mut v) => {
                if has_paths {
                    v["message"] = json!(format!(
                        "paths[{i}]: {}",
                        v["message"].as_str().unwrap_or("")
                    ));
                }
                return Err(v);
            }
        }
    }
    let mut scan = Scan {
        matches: vec![],
        files: 0,
        bytes: 0,
        skipped: 0,
        complete: true,
        reason: None,
        seen: HashSet::new(),
        limit,
    };
    for (p, dir) in targets {
        if scan.full() {
            break;
        }
        if dir {
            scan.walk(ws, &p, query, re.as_ref(), true);
            continue;
        }
        let canonical = fs::canonicalize(&p).map_err(os_outcome)?;
        if !scan.seen.insert(canonical) {
            continue;
        }
        scan.scan(ws, &p, read_snapshot(&p)?, query, re.as_ref());
    }
    let mut shown = fit_elements(ws, scan.matches.len(), |n| scan.data(n));
    if shown < scan.matches.len() {
        scan.stop("result_bytes");
        shown = fit_elements(ws, scan.matches.len(), |n| scan.data(n));
    }
    let mut data = scan.data(shown);
    if shown == 0 && !scan.matches.is_empty() {
        let mut first = scan.matches[0].clone();
        loop {
            data = scan.data(0);
            data["matches"] = json!([first.clone()]);
            if fits(ws, &data) {
                shown = 1;
                break;
            }
            let text = first["text"].as_str().unwrap();
            if text.is_empty() {
                data = scan.data(0);
                break;
            }
            let mut end = text.len() / 2;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            first["text"] = json!(&text[..end]);
            first["preview_truncated"] = json!(true);
        }
    }
    let mut out = workspace_outcome(ws, data, "none");
    out["truncated"] = json!(shown < scan.matches.len());
    if !regex && scan.matches.is_empty() && query.contains('|') {
        out["message"]=json!("no matches; the query contains \"|\" but regex is false, so it was searched literally; set regex: true to search alternatives");
    }
    Ok(out)
}

const INVALID_DIGEST:&str="expected_sha256 must be a 64-character digest, or omitted to use the version you last read or wrote";
fn expected(args: &Value) -> Result<Option<&str>, Value> {
    let Some(v) = args.get("expected_sha256") else {
        return Ok(None);
    };
    let Some(s) = v.as_str() else {
        return Err(fail("invalid_arguments", INVALID_DIGEST));
    };
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(fail("invalid_arguments", INVALID_DIGEST));
    }
    Ok(Some(s))
}
fn file_target(info: &fs::Metadata) -> Result<(), Value> {
    if info.file_type().is_symlink() {
        return Err(fail("symlink_target", "path is a symlink"));
    }
    if !info.is_file() {
        return Err(fail("invalid_arguments", "path is not a regular file"));
    }
    Ok(())
}
fn text_content(text: &str, label: &str) -> Result<(), Value> {
    if text.len() > MAX_FILE_BYTES {
        return Err(fail(
            "file_too_large",
            &format!("{label} exceeds the {MAX_FILE_BYTES} byte limit"),
        ));
    }
    if text.contains('\0') {
        return Err(fail("binary_file", &format!("{label} contains NUL bytes")));
    }
    Ok(())
}
fn create_parents(ws: &Workspace, parent: &Path) -> Result<Vec<PathBuf>, Value> {
    let mut created = vec![];
    let result = (|| {
        let mut current = ws.root.clone();
        for part in parent.strip_prefix(&ws.root).unwrap().components() {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(info) if info.file_type().is_symlink() => {
                    return Err(fail("symlink_target", "parent path is a symlink"))
                }
                Ok(info) if !info.is_dir() => {
                    return Err(fail("not_directory", "parent path is not a directory"))
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&current).map_err(os_outcome)?;
                    created.push(current.clone());
                }
                Err(e) => return Err(os_outcome(e)),
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        for path in created.iter().rev() {
            if fs::remove_dir(path).is_err() {
                break;
            }
        }
        return Err(error);
    }
    Ok(created)
}

fn write_file(ws: &mut Workspace, config: &Value, args: &Value) -> Result<Value, Value> {
    let content = args["content"]
        .as_str()
        .ok_or_else(|| fail("invalid_arguments", "content must be a string"))?;
    text_content(content, "content")?;
    let expected = expected(args)?;
    let abs = ws.resolve(path(args))?;
    let mut created = vec![];
    let (operation, before, mode) = match fs::symlink_metadata(&abs) {
        Ok(info) => {
            file_target(&info)?;
            let snap = ws.check_digest(&abs, expected)?;
            ("overwrite", Some(snap.sha256), info.permissions().mode())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if expected.is_some() {
                return Err(fail("not_found", "no such path in the workspace"));
            }
            if config["agents"].as_bool().unwrap_or(false)
                && ws.seen.contains_key(&missing_path(&abs))
            {
                return Err(fail("stale_file","the file you read or wrote was deleted; read_file must observe its absence before recreating it"));
            }
            let parent = abs.parent().unwrap();
            match fs::metadata(parent) {
                Ok(info) => {
                    if !info.is_dir() {
                        return Err(fail("not_directory", "parent path is not a directory"));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    created = create_parents(ws, parent)?;
                }
                Err(e) => return Err(os_outcome(e)),
            }
            ("create", None, 0o644)
        }
        Err(e) => return Err(os_outcome(e)),
    };
    if let Err(e) = publish(&abs, content.as_bytes(), mode, operation == "create") {
        for p in created.iter().rev() {
            let _ = fs::remove_dir(p);
        }
        return Err(e);
    }
    let sha = digest(content.as_bytes());
    let mut data = json!({"operation":operation,"path":ws.relative(&abs),"before_sha256":before,"after_sha256":sha,"size_bytes":content.len()});
    if !created.is_empty() {
        data["created_dirs"] = json!(created.iter().map(|p| ws.relative(p)).collect::<Vec<_>>());
    }
    ws.remember(&abs, &sha);
    Ok(workspace_outcome(ws, data, "applied"))
}

struct Edit {
    old: String,
    new: String,
    all: bool,
}
fn changes(args: &Value) -> Result<(Vec<Edit>, String, bool), Value> {
    let single = args.get("old_text").is_some() || args.get("new_text").is_some();
    let multi = args.get("edits").is_some();
    if single && multi {
        return Err(fail(
            "invalid_arguments",
            "old_text/new_text and edits[0] cannot be combined",
        ));
    }
    if !single && !multi && args.get("append_text").is_none() {
        return Err(fail("invalid_arguments", "at least one change is required"));
    }
    let append = if let Some(v) = args.get("append_text") {
        v.as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                fail(
                    "invalid_arguments",
                    "append_text must be a non-empty string",
                )
            })?
            .to_string()
    } else {
        String::new()
    };
    let mut edits = vec![];
    if single {
        if args.get("old_text").is_none() || args.get("new_text").is_none() {
            return Err(fail(
                "invalid_arguments",
                "old_text and new_text are both required",
            ));
        }
        let (old, new) = (args["old_text"].as_str(), args["new_text"].as_str());
        let (Some(old), Some(new)) = (old, new) else {
            return Err(fail(
                "invalid_arguments",
                "old_text and new_text must be strings",
            ));
        };
        edits.push(Edit {
            old: old.into(),
            new: new.into(),
            all: false,
        });
    } else if multi {
        let items = args["edits"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| fail("invalid_arguments", "edits must be a non-empty array"))?;
        for (i, item) in items.iter().enumerate() {
            let prefix = |s: &str| fail("invalid_arguments", &format!("edits[{i}]: {s}"));
            let object = item
                .as_object()
                .ok_or_else(|| prefix("arguments must be one JSON object"))?;
            for key in object.keys() {
                if !["old_text", "new_text", "replace_all"].contains(&key.as_str()) {
                    return Err(prefix(&format!("unknown field {key:?}")));
                }
            }
            if item.get("old_text").is_none() || item.get("new_text").is_none() {
                return Err(prefix("old_text and new_text are both required"));
            }
            let (Some(old), Some(new)) = (item["old_text"].as_str(), item["new_text"].as_str())
            else {
                return Err(prefix("old_text and new_text must be strings"));
            };
            let all = match item.get("replace_all") {
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| prefix("replace_all must be a boolean"))?,
                None => false,
            };
            edits.push(Edit {
                old: old.into(),
                new: new.into(),
                all,
            });
        }
    }
    for (i, edit) in edits.iter().enumerate() {
        if edit.old.is_empty() {
            return Err(fail(
                "invalid_arguments",
                &format!(
                    "{}old_text must not be empty",
                    if multi {
                        format!("edits[{i}]: ")
                    } else {
                        String::new()
                    }
                ),
            ));
        }
    }
    Ok((edits, append, multi))
}
fn positions(text: &str, query: &str, limit: usize) -> Vec<usize> {
    if query.is_empty() {
        return vec![];
    }
    text.char_indices()
        .filter_map(|(i, _)| {
            if text[i..].starts_with(query) {
                Some(i)
            } else {
                None
            }
        })
        .take(limit + 1)
        .collect()
}
fn format_lines(text: &str, positions: &[usize], limit: usize) -> String {
    positions
        .iter()
        .take(limit)
        .map(|p| (text[..*p].bytes().filter(|b| *b == b'\n').count() + 1).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
fn normalize(text: &str) -> (String, Vec<usize>) {
    let bytes = text.as_bytes();
    let mut normalized = vec![];
    let mut offsets = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b' ' && bytes[i] != b'\t' {
            normalized.push(bytes[i]);
            offsets.push(i);
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i == bytes.len() || bytes[i] == b'\n' {
            continue;
        }
        normalized.push(b' ');
        offsets.push(start);
    }
    (String::from_utf8(normalized).unwrap(), offsets)
}
fn edit_file(ws: &mut Workspace, args: &Value) -> Result<Value, Value> {
    let expected = expected(args)?;
    let (edits, append, multi) = changes(args)?;
    let abs = ws.resolve(path(args))?;
    let info = fs::symlink_metadata(&abs).map_err(os_outcome)?;
    if info.file_type().is_symlink() {
        return Err(fail("symlink_target", "path is a symlink"));
    }
    let snap = ws.check_digest(&abs, expected)?;
    let before = std::str::from_utf8(&snap.content).unwrap();
    let mut matches = vec![];
    for (i, edit) in edits.iter().enumerate() {
        let prefix = if multi {
            format!("edits[{i}]: ")
        } else {
            String::new()
        };
        let found = positions(before, &edit.old, 10);
        if found.is_empty() {
            let (normalized, offsets) = normalize(before);
            let (query, _) = normalize(&edit.old);
            let near = positions(&normalized, &query, 5)
                .iter()
                .map(|p| offsets[*p])
                .collect::<Vec<_>>();
            let suffix = if near.is_empty() {
                "; read the range again before retrying".into()
            } else {
                format!("; it matches with different whitespace at line {} (read that range again and copy it exactly)",format_lines(before,&near,5))
            };
            return Err(fail(
                "edit_not_found",
                &format!("{prefix}old_text does not occur in the file{suffix}"),
            ));
        }
        if found.len() > 1 && (!edit.all || edit.old == edit.new) {
            let count = if found.len() > 10 {
                "at least 10".into()
            } else {
                found.len().to_string()
            };
            let location = if found.len() > 10 {
                "first at lines"
            } else {
                "at lines"
            };
            return Err(fail("ambiguous_edit",&format!("{prefix}old_text occurs {count} times, {location} {}; include enough surrounding text to make it unique or set replace_all if every occurrence should change",format_lines(before,&found,10))));
        }
        for (start, _) in before.match_indices(&edit.old) {
            matches.push((start, start + edit.old.len(), i));
            if !edit.all {
                break;
            }
        }
    }
    matches.sort_by_key(|m| m.0);
    let skipped = edits
        .iter()
        .enumerate()
        .filter(|(_, e)| e.old == e.new)
        .map(|(i, _)| format!("edits[{i}] changed nothing and was skipped"))
        .collect::<Vec<_>>();
    if !edits.is_empty() && skipped.len() == edits.len() {
        return Err(fail("invalid_arguments", "every edit changes nothing"));
    }
    let mut counts = vec![0usize; edits.len()];
    let mut after = String::new();
    let mut from = 0;
    let mut previous = 0;
    for (start, end, i) in matches {
        if start < from {
            return Err(fail(
                "invalid_arguments",
                &format!("edits[{previous}] and edits[{i}]: matched ranges overlap"),
            ));
        }
        after.push_str(&before[from..start]);
        after.push_str(&edits[i].new);
        if edits[i].old != edits[i].new {
            counts[i] += 1;
        }
        from = end;
        previous = i;
    }
    after.push_str(&before[from..]);
    after.push_str(&append);
    if after.len() > MAX_FILE_BYTES {
        return Err(fail(
            "file_too_large",
            &format!("the result would exceed the {MAX_FILE_BYTES} byte limit"),
        ));
    }
    if after.contains('\0') {
        return Err(fail("binary_file", "the result would contain NUL bytes"));
    }
    publish(&abs, after.as_bytes(), info.permissions().mode(), false)?;
    let sha = digest(after.as_bytes());
    let mut data = json!({"operation":"update","path":ws.relative(&abs),"changed":true,"before_sha256":snap.sha256,"after_sha256":sha,"size_bytes":after.len()});
    let count: usize = counts.iter().sum();
    if count > 0 {
        data["edits"] = json!(count);
    }
    if !counts.is_empty() {
        data["edit_counts"] = json!(counts);
    }
    if !skipped.is_empty() {
        data["skipped_edits"] = json!(skipped);
    }
    if !append.is_empty() {
        data["appended_bytes"] = json!(append.len());
    }
    ws.remember(&abs, &sha);
    Ok(workspace_outcome(ws, data, "applied"))
}

fn delete_path(ws: &Workspace, p: &str) -> Result<PathBuf, Value> {
    ws.check_root()?;
    if p.contains('\0') {
        return Err(fail(
            "invalid_path",
            "path must be valid UTF-8 without NUL bytes",
        ));
    }
    if p.split('/').any(|part| part == ".." || withheld(part)) {
        return Err(fail(
            "invalid_path",
            "path contains a parent or withheld component",
        ));
    }
    let abs = clean(&ws.root.join(p));
    if !abs.starts_with(&ws.root) || abs == ws.root {
        return Err(fail(
            "invalid_path",
            "deletion must stay inside the workspace and cannot remove its root",
        ));
    }
    let mut current = ws.root.clone();
    let parts = abs
        .strip_prefix(&ws.root)
        .unwrap()
        .components()
        .collect::<Vec<_>>();
    for (i, part) in parts.iter().enumerate() {
        current.push(part);
        let info = fs::symlink_metadata(&current).map_err(os_outcome)?;
        if i == parts.len() - 1 {
            break;
        }
        if info.file_type().is_symlink() {
            return Err(fail(
                "symlink_target",
                "recursive deletion refuses symlink ancestors",
            ));
        }
        if !info.is_dir() {
            return Err(fail("not_directory", "parent path is not a directory"));
        }
    }
    Ok(abs)
}
fn plan_delete(
    ws: &Workspace,
    p: &Path,
    plan: &mut Vec<(PathBuf, fs::Metadata)>,
) -> Result<(), Value> {
    delete_path(ws, &p.to_string_lossy())?;
    let info = fs::symlink_metadata(p).map_err(os_outcome)?;
    if !info.is_dir() && !info.is_file() && !info.file_type().is_symlink() {
        return Err(fail(
            "invalid_arguments",
            "recursive deletion refuses special files",
        ));
    }
    let dir = info.is_dir();
    plan.push((p.into(), info));
    if dir {
        let mut entries = fs::read_dir(p)
            .map_err(os_outcome)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(os_outcome)?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            plan_delete(ws, &entry.path(), plan)?;
        }
    }
    Ok(())
}
fn delete_file(ws: &mut Workspace, args: &Value) -> Result<Value, Value> {
    let expected = expected(args)?;
    let recursive = optional_bool(args, "recursive")?;
    ws.check_root()?;
    let abs = if recursive {
        delete_path(ws, path(args))?
    } else {
        ws.resolve(path(args))?
    };
    let info = fs::symlink_metadata(&abs).map_err(os_outcome)?;
    if recursive && (info.is_dir() || info.file_type().is_symlink()) {
        if expected.is_some() {
            return Err(fail(
                "invalid_arguments",
                "expected_sha256 is only valid for regular files",
            ));
        }
        let mut plan = vec![];
        plan_delete(ws, &abs, &mut plan)?;
        let mut removed = vec![];
        let mut count = 0;
        let mut bytes = 0;
        let mut problem = None;
        let mut failed = String::new();
        for (p, old) in plan.into_iter().rev() {
            let result = (|| {
                delete_path(ws, &p.to_string_lossy())?;
                let current = fs::symlink_metadata(&p).map_err(os_outcome)?;
                if (old.dev(), old.ino(), old.mode() & libc::S_IFMT)
                    != (current.dev(), current.ino(), current.mode() & libc::S_IFMT)
                {
                    return Err(fail("stale_file", "path changed after preflight"));
                }
                if current.is_dir() {
                    fs::remove_dir(&p)
                } else {
                    fs::remove_file(&p)
                }
                .map_err(os_outcome)
            })();
            if let Err(e) = result {
                problem = Some(e);
                failed = ws.relative(&p);
                break;
            }
            ws.seen
                .retain(|seen, _| seen != &p && !(old.is_dir() && seen.starts_with(&p)));
            count += 1;
            let rel = ws.relative(&p);
            let n = crate::types::json_string(&json!(rel)).len() + 1;
            if bytes + n <= 8192 {
                removed.push(rel);
                bytes += n;
            }
        }
        let mut data = json!({"operation":"delete","path":ws.relative(&abs),"recursive":true,"removed_paths":removed,"removed_count":count,"omitted":count-removed.len()});
        if !failed.is_empty() {
            data["failed_path"] = json!(failed);
        }
        let mut out = workspace_outcome(ws, data, if count > 0 { "applied" } else { "none" });
        if let Some(e) = problem {
            out["ok"] = json!(false);
            out["code"] = e["code"].clone();
            out["message"] = e["message"].clone();
        }
        while crate::types::json_string(&out).len() > MAX_RESULT_BYTES
            && !out["data"]["removed_paths"].as_array().unwrap().is_empty()
        {
            out["data"]["removed_paths"].as_array_mut().unwrap().pop();
            out["data"]["omitted"] = json!(out["data"]["omitted"].as_u64().unwrap() + 1);
        }
        out["truncated"] = json!(out["data"]["omitted"].as_u64().unwrap() > 0);
        return Ok(out);
    }
    file_target(&info)?;
    let before = if let Some(expected) = expected {
        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        let mut file = fs::File::open(&abs).map_err(os_outcome)?;
        let opened = file.metadata().map_err(os_outcome)?;
        if !opened.is_file() || (opened.dev(), opened.ino()) != (info.dev(), info.ino()) {
            return Err(fail("stale_file", "path changed while preparing deletion"));
        }
        let mut buffer = [0u8; 65536];
        loop {
            let n = file.read(&mut buffer).map_err(os_outcome)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        let before = format!("{:x}", hasher.finalize());
        ws.compare_digest(&abs, expected, &before)?;
        before
    } else {
        ws.check_digest(&abs, None)?.sha256
    };
    let current = fs::symlink_metadata(&abs).map_err(os_outcome)?;
    if (current.dev(), current.ino(), current.mode() & libc::S_IFMT)
        != (info.dev(), info.ino(), info.mode() & libc::S_IFMT)
    {
        return Err(fail("stale_file", "path changed while preparing deletion"));
    }
    let canonical = fs::canonicalize(&abs).map_err(os_outcome)?;
    ws.check_root()?;
    fs::remove_file(&abs).map_err(os_outcome)?;
    ws.seen.remove(&canonical);
    Ok(workspace_outcome(
        ws,
        json!({"operation":"delete","path":ws.relative(&abs),"before_sha256":before}),
        "applied",
    ))
}
