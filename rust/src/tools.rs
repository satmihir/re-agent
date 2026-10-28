use crate::workspace::{clean, os_outcome, withheld, workspace_outcome, Workspace};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub struct ToolState {
    pub workspace: Workspace,
    pub state: Value,
    handles: HashMap<PathBuf, Workspace>,
    approved: Vec<PathBuf>,
}
impl ToolState {
    pub fn new(config: &Value) -> Result<Self, String> {
        let workspace = Workspace::open(config["workspace"].as_str().unwrap_or("."))?;
        let mut approved = vec![workspace.root.clone()];
        for key in [
            "allow_workspace",
            "allowed_workspaces",
            "approved_workspaces",
        ] {
            if let Some(paths) = config[key].as_array() {
                for path in paths {
                    if let Some(path) = path.as_str() {
                        approved.push(crate::workspace::missing_path(Path::new(path)));
                    }
                }
            }
        }
        Ok(Self {
            workspace,
            state: json!({}),
            handles: HashMap::new(),
            approved,
        })
    }
}

pub fn outcome(ok: bool, code: &str, message: &str, data: Value, effect: &str) -> Value {
    json!({"ok":ok,"code":code,"message":message,"data":data,"truncated":false,"effect":effect})
}
pub fn fail(code: &str, message: &str) -> Value {
    outcome(false, code, message, Value::Null, "none")
}

pub fn specs(config: &Value) -> Value {
    let mut specs: Vec<Value> = serde_json::from_str(include_str!("tool_specs.json")).unwrap();
    if !config["report_friction"].as_bool().unwrap_or(false) {
        specs.retain(|s| s["name"] != "report_friction");
    }
    specs.push(crate::tool_exec::spec());
    specs.push(crate::tool_git::inspect_spec());
    if config["git_do"].as_bool().unwrap_or(false) {
        specs.push(crate::tool_git::git_do_spec(config));
    }
    if config["agents"].as_bool().unwrap_or(false) {
        specs.extend(crate::agents::specs());
    }
    if config["read_only"].as_bool().unwrap_or(false) {
        specs.retain(|s| s["effect"] == "read");
    }
    specs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Value::Array(specs)
}

pub fn optional_int(args: &Value, key: &str, default: usize, least: usize) -> Result<usize, Value> {
    let Some(value) = args.get(key) else {
        return Ok(default);
    };
    if value.is_null() {
        return Err(fail(
            "invalid_arguments",
            &format!("{key} must be omitted rather than null"),
        ));
    }
    let Some(n) = value.as_i64() else {
        return Err(fail(
            "invalid_arguments",
            &format!("{key} must be an integer"),
        ));
    };
    if n < least as i64 {
        return Err(fail(
            "invalid_arguments",
            &format!("{key} must be at least {least}"),
        ));
    }
    Ok(n as usize)
}
pub fn optional_bool(args: &Value, key: &str) -> Result<bool, Value> {
    let Some(value) = args.get(key) else {
        return Ok(false);
    };
    if value.is_null() {
        return Err(fail(
            "invalid_arguments",
            &format!("{key} must be omitted rather than null"),
        ));
    }
    value
        .as_bool()
        .ok_or_else(|| fail("invalid_arguments", &format!("{key} must be a boolean")))
}

fn decode(arguments: &str, spec: &Value) -> Result<Value, Value> {
    if !arguments.trim_start().starts_with('{') {
        return Err(fail(
            "invalid_arguments",
            "arguments must be one JSON object",
        ));
    }
    let mut decoder = serde_json::Deserializer::from_str(arguments);
    let args: Value = serde::Deserialize::deserialize(&mut decoder).map_err(|e| {
        fail(
            "invalid_arguments",
            &crate::errors::json_error(arguments, &e),
        )
    })?;
    if decoder.end().is_err() {
        return Err(fail(
            "invalid_arguments",
            "arguments must be one JSON object",
        ));
    }
    let Some(object) = args.as_object() else {
        return Err(fail(
            "invalid_arguments",
            "arguments must be one JSON object",
        ));
    };
    let schema = &spec["input_schema"]["properties"];
    for (key, value) in object {
        let Some(property) = schema.get(key) else {
            return Err(fail("invalid_arguments", &format!("unknown field {key:?}")));
        };
        // Go string fields accept null as their zero value; RawMessage fields validate it in the tool.
        if property["type"] == "string"
            && !value.is_string()
            && !value.is_null()
            && !["content", "expected_sha256", "append_text", "details"].contains(&key.as_str())
        {
            return Err(fail("invalid_arguments", &crate::errors::field_error(key)));
        }
    }
    Ok(args)
}

pub fn execute(
    state: &mut ToolState,
    config: &Value,
    name: &str,
    arguments: &str,
) -> Result<Value, String> {
    let all = specs(config);
    let spec = all.as_array().unwrap().iter().find(|s| s["name"] == name);
    let write = matches!(
        name,
        "write_file" | "edit_file" | "delete_file" | "exec" | "git_do"
    );
    if spec.is_none() {
        if config["read_only"].as_bool().unwrap_or(false) && write {
            return Ok(fail(
                "permission_denied",
                &format!("{name} is not enabled; the run was started in read only mode"),
            ));
        }
        return Ok(fail("tool_unavailable", &format!("no tool named {name}")));
    }
    if config["plan"].as_bool().unwrap_or(false) && write {
        return Ok(fail("plan_mode",&format!("{name} is refused in plan mode, which only the user can end. Put the change in the plan; to see a command's output, ask the user to run it with !.")));
    }
    if name == "git_inspect" {
        return crate::tool_git::inspect_raw(&mut state.workspace, config, arguments);
    }
    let args = match decode(arguments, spec.unwrap()) {
        Ok(args) => args,
        Err(bad) => return Ok(bad),
    };
    match name {
        "exec" => crate::tool_exec::execute(&mut state.workspace, config, &args),
        "git_inspect" => crate::tool_git::inspect(&mut state.workspace, config, &args),
        "git_do" => crate::tool_git::git_do(&mut state.workspace, config, &args),
        "request_workspace_access" | "switch_workspace" => {
            Ok(workspace_tool(state, config, name, &args).unwrap_or_else(|bad| bad))
        }
        "report_friction" => Ok(report_friction(state, &args).unwrap_or_else(|bad| bad)),
        _ => Ok(
            crate::tool_files::execute(&mut state.workspace, config, name, &args)
                .unwrap_or_else(|bad| bad),
        ),
    }
}

fn destination(path: &str, prospective: bool) -> Result<(PathBuf, bool), Value> {
    if path.trim().is_empty() || !Path::new(path).is_absolute() || path.contains('\0') {
        return Err(fail(
            "invalid_path",
            "workspace path must be a nonblank absolute path without NUL bytes",
        ));
    }
    if path.split('/').any(|p| p == ".." || withheld(p)) {
        return Err(fail(
            "invalid_path",
            "workspace path must not contain parent traversal or withheld names",
        ));
    }
    let path = clean(Path::new(path));
    let Some(parent) = path.parent() else {
        return Err(fail(
            "invalid_path",
            "the filesystem root cannot be selected",
        ));
    };
    let parent = fs::canonicalize(parent).map_err(os_outcome)?;
    if !fs::metadata(&parent).map_err(os_outcome)?.is_dir() {
        return Err(fail("not_directory", "workspace parent is not a directory"));
    }
    let path = parent.join(path.file_name().unwrap());
    let (root, missing) = match fs::symlink_metadata(&path) {
        Err(e) if prospective && e.kind() == std::io::ErrorKind::NotFound => (path, true),
        Err(e) => return Err(os_outcome(e)),
        Ok(_) => {
            let root = fs::canonicalize(&path).map_err(os_outcome)?;
            if !fs::metadata(&root).map_err(os_outcome)?.is_dir() {
                return Err(fail("not_directory", "workspace is not a directory"));
            }
            (root, false)
        }
    };
    if root.parent().is_none()
        || root
            .components()
            .any(|p| withheld(&p.as_os_str().to_string_lossy()))
    {
        return Err(fail(
            "invalid_path",
            "resolved workspace is a filesystem root or withheld location",
        ));
    }
    Ok((root, missing))
}
fn workspace_tool(
    state: &mut ToolState,
    config: &Value,
    name: &str,
    args: &Value,
) -> Result<Value, Value> {
    let path = args["path"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| fail("invalid_arguments", "path must be a nonblank string"))?;
    let(root,missing)=destination(path,name=="request_workspace_access").map_err(|mut v|{if name=="switch_workspace"&&v["code"]=="not_found"{v["message"]=json!("workspace must exist; request_workspace_access can approve the exact destination before creation");}v})?;
    if !state.approved.iter().any(|p| root.starts_with(p)) {
        return Err(fail(
            "permission_required",
            "workspace access needs human consent; use interactive chat or --allow-workspace PATH",
        ));
    }
    if name == "request_workspace_access" {
        return Ok(workspace_outcome(
            &state.workspace,
            json!({"approved_path":root.to_string_lossy(),"workspace":state.workspace.root.to_string_lossy(),"missing":missing}),
            "none",
        ));
    }
    let previous = state.workspace.root.clone();
    if root == previous {
        return Ok(workspace_outcome(
            &state.workspace,
            json!({"previous_workspace":previous.to_string_lossy(),"workspace":root.to_string_lossy(),"changed":false}),
            "none",
        ));
    }
    let mut next_config = config.clone();
    next_config["workspace"] = json!(root.to_string_lossy());
    let mut data = json!({"previous_workspace":previous.to_string_lossy(),"workspace":root.to_string_lossy(),"changed":true,"workspace_state":crate::workspace::snapshot(&next_config)});
    if config["no_project_instructions"].as_bool().unwrap_or(false) {
        data["project_instructions_note"] =
            json!("project instructions are disabled for this session");
    } else {
        match crate::workspace::project_instructions(&root.to_string_lossy()){
        Ok(Some(project))=>data["project_instructions"]=json!(project),
        _=>data["project_instructions_note"]=json!("no AGENTS.md was loaded from the new workspace (see warnings if any); earlier project instructions no longer apply"),
    }
    }
    let mut out = workspace_outcome(&state.workspace, data, "none");
    if crate::types::json_string(&out).len() > crate::workspace::MAX_RESULT_BYTES {
        if let Some(project) = out["data"]
            .as_object_mut()
            .unwrap()
            .remove("project_instructions")
        {
            out["data"]["project_instructions_note"]=json!(format!("AGENTS.md ({} bytes) does not fit in this result; read it with read_file before continuing",project.as_str().unwrap().len()));
        }
    }
    if crate::types::json_string(&out).len() > crate::workspace::MAX_RESULT_BYTES {
        return Err(fail(
            "invalid_arguments",
            "workspace transition metadata exceeds the result limit",
        ));
    }
    let next = state
        .handles
        .remove(&root)
        .map(Ok)
        .unwrap_or_else(|| Workspace::open(&root.to_string_lossy()))
        .map_err(|e| fail("io_error", &e))?;
    let old = std::mem::replace(&mut state.workspace, next);
    state.handles.insert(previous, old);
    if !state.approved.contains(&root) {
        state.approved.push(root);
    }
    Ok(out)
}

fn report_friction(state: &mut ToolState, args: &Value) -> Result<Value, Value> {
    let category = args["category"].as_str().unwrap_or("");
    if ![
        "misleading_error",
        "missing_capability",
        "unclear_description",
        "harness_bug",
        "other",
    ]
    .contains(&category)
    {
        return Err(fail("invalid_arguments","category must be misleading_error, missing_capability, unclear_description, harness_bug, or other"));
    }
    let summary = args["summary"].as_str().unwrap_or("");
    if summary.trim().is_empty() || summary.contains(['\r', '\n']) || summary.chars().count() > 300
    {
        return Err(fail(
            "invalid_arguments",
            "summary must be one nonblank line of at most 300 Unicode characters",
        ));
    }
    if let Some(details) = args.get("details") {
        if !details.as_str().is_some_and(|s| s.chars().count() <= 4000) {
            return Err(fail(
                "invalid_arguments",
                "details must be a string of at most 4000 Unicode characters, or omitted",
            ));
        }
    }
    if let Some(ids) = args.get("related_call_ids") {
        let ids = ids.as_array().filter(|a| a.len() <= 20).ok_or_else(|| {
            fail(
                "invalid_arguments",
                "related_call_ids must be an array of at most 20 strings, or omitted",
            )
        })?;
        if ids.iter().any(|v| !v.is_string()) {
            return Err(fail(
                "invalid_arguments",
                "related_call_ids must contain only strings",
            ));
        }
    }
    let reports = state.state["friction_reports"].as_u64().unwrap_or(0);
    if reports >= 10 {
        return Err(fail(
            "invalid_arguments",
            "report limit reached; carry on with the task",
        ));
    }
    state.state["friction_reports"] = json!(reports + 1);
    Ok(outcome(true, "ok", "", json!({"recorded":true}), "none"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_edits_require_observation_and_detect_external_changes() {
        let root = tempfile::tempdir().unwrap();
        let config = json!({"workspace":root.path().to_str().unwrap()});
        let mut state = ToolState::new(&config).unwrap();
        fs::write(root.path().join("a"), "before\n").unwrap();
        let edit = r#"{"path":"a","old_text":"before","new_text":"after"}"#;
        assert_eq!(
            execute(&mut state, &config, "edit_file", edit).unwrap()["code"],
            "invalid_arguments"
        );
        assert_eq!(
            execute(&mut state, &config, "read_file", r#"{"path":"a"}"#).unwrap()["ok"],
            true
        );
        assert_eq!(
            execute(&mut state, &config, "edit_file", edit).unwrap()["ok"],
            true
        );
        fs::write(root.path().join("a"), "external\n").unwrap();
        assert_eq!(
            execute(
                &mut state,
                &config,
                "write_file",
                r#"{"path":"a","content":"replacement"}"#
            )
            .unwrap()["code"],
            "stale_file"
        );
    }
    #[test]
    fn file_argument_errors_are_stable() {
        let root = tempfile::tempdir().unwrap();
        let config = json!({"workspace":root.path()});
        let mut state = ToolState::new(&config).unwrap();
        for (name, arguments, message) in [
            ("read_file", r#"{"path":1}"#, "field \"path\" must be a string"),
            ("read_file", r#"{"path":"a","max_lines":null}"#, "max_lines must be omitted rather than null"),
            ("read_file", r#"{"path":"a","extra":1}"#, "unknown field \"extra\""),
            ("write_file", r#"{"path":"a","content":null}"#, "content must be a string"),
            ("delete_file", r#"{"path":"a","expected_sha256":null}"#, "expected_sha256 must be a 64-character digest, or omitted to use the version you last read or wrote"),
        ] {
            let result = execute(&mut state, &config, name, arguments).unwrap();
            assert_eq!(result["code"], "invalid_arguments", "{arguments}");
            assert_eq!(result["message"], message, "{arguments}");
        }
    }

    #[test]
    fn missing_parents_refuse_symlink_ancestors_without_publication() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("real")).unwrap();
        symlink("real", root.path().join("alias")).unwrap();
        let config = json!({"workspace":root.path()});
        let mut state = ToolState::new(&config).unwrap();
        let result = execute(
            &mut state,
            &config,
            "write_file",
            r#"{"path":"alias/new/a","content":"hello"}"#,
        )
        .unwrap();
        assert_eq!(result["code"], "symlink_target");
        assert_eq!(result["message"], "parent path is a symlink");
        assert!(!root.path().join("real/new").exists());
        let result = execute(
            &mut state,
            &config,
            "write_file",
            r#"{"path":"new/nested/a","content":"hello"}"#,
        )
        .unwrap();
        assert_eq!(result["data"]["created_dirs"], json!(["new", "new/nested"]));
        assert_eq!(
            execute(
                &mut state,
                &config,
                "edit_file",
                r#"{"path":"new/nested/a","append_text":"!"}"#
            )
            .unwrap()["ok"],
            true
        );
        assert_eq!(
            fs::read_to_string(root.path().join("new/nested/a")).unwrap(),
            "hello!"
        );
    }

    #[test]
    fn explicit_digest_deletes_unread_large_binary_file() {
        let root = tempfile::tempdir().unwrap();
        let bytes = vec![0; crate::workspace::MAX_FILE_BYTES + 1];
        fs::write(root.path().join("binary"), &bytes).unwrap();
        let config = json!({"workspace":root.path()});
        let mut state = ToolState::new(&config).unwrap();
        assert_eq!(
            execute(&mut state, &config, "delete_file", r#"{"path":"binary"}"#).unwrap()["code"],
            "file_too_large"
        );
        let args =
            json!({"path":"binary","expected_sha256":crate::workspace::digest(&bytes)}).to_string();
        let result = execute(&mut state, &config, "delete_file", &args).unwrap();
        assert_eq!(result["effect"], "applied");
        assert!(!root.path().join("binary").exists());
    }

    #[test]
    fn withheld_aliases_and_recursive_delete_are_guarded() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let config = json!({"workspace":root.path().to_str().unwrap()});
        let mut state = ToolState::new(&config).unwrap();
        fs::write(root.path().join(".env.secret"), "secret").unwrap();
        symlink(".env.secret", root.path().join("alias")).unwrap();
        assert_eq!(
            execute(&mut state, &config, "read_file", r#"{"path":"alias"}"#).unwrap()["code"],
            "invalid_path"
        );
        fs::create_dir(root.path().join("tree")).unwrap();
        fs::write(root.path().join("tree/.ENV"), "secret").unwrap();
        assert_eq!(
            execute(
                &mut state,
                &config,
                "delete_file",
                r#"{"path":"tree","recursive":true}"#
            )
            .unwrap()["code"],
            "invalid_path"
        );
        assert!(root.path().join("tree/.ENV").exists());
    }
}
