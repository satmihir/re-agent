use crate::workspace::{clean, os_outcome, withheld, workspace_outcome, Workspace};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// WorkspaceDestination describes a validated location presented for human consent.
#[derive(Clone)]
pub struct WorkspaceDestination {
    /// path is the canonical workspace or the exact prospective destination.
    pub path: PathBuf,
    /// missing distinguishes an exact reservation from an existing directory.
    pub missing: bool,
    spelling: PathBuf,
    requested: PathBuf,
    parent: (u64, u64),
}

/// WorkspaceConsentError distinguishes missing terminal input from unavailable consent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceConsentError {
    /// NotInteractive means input cannot grant dynamic workspace access.
    NotInteractive,
    /// Unavailable means the picker was cancelled or its input failed.
    Unavailable,
}

/// WorkspaceConsent asks the human without granting access or changing authority.
pub type WorkspaceConsent =
    dyn FnMut(&Value, &WorkspaceDestination) -> Result<bool, WorkspaceConsentError> + Send;

pub struct ToolState {
    pub workspace: Workspace,
    pub state: Value,
    handles: HashMap<PathBuf, Workspace>,
    approved: Vec<WorkspaceDestination>,
    /// workspace_consent is a chat-local callback, never checkpointed or model-configured.
    pub workspace_consent: Option<Box<WorkspaceConsent>>,
}
impl ToolState {
    /// forget_seen clears write authorization in every retained workspace without revoking grants.
    pub(crate) fn forget_seen(&mut self) {
        self.workspace.forget_seen();
        for workspace in self.handles.values_mut() {
            workspace.forget_seen();
        }
    }

    /// share_publication_guard coordinates file changes without sharing read authorization.
    pub(crate) fn share_publication_guard(&mut self, parent: &ToolState) {
        self.workspace.publish_mu = parent.workspace.publish_mu.clone();
        for workspace in self.handles.values_mut() {
            workspace.publish_mu = parent.workspace.publish_mu.clone();
        }
    }

    /// approved_paths returns the canonical grants in the reference's trace order.
    pub fn approved_paths(&self) -> Vec<String> {
        let mut paths: Vec<_> = self
            .approved
            .iter()
            .map(|destination| destination.path.to_string_lossy().into_owned())
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }

    pub fn new(config: &Value) -> Result<Self, String> {
        let workspace = Workspace::open(config["workspace"].as_str().unwrap_or("."))?;
        let mut approved = vec![WorkspaceDestination {
            path: workspace.root.clone(),
            missing: false,
            spelling: workspace.root.clone(),
            requested: workspace.root.clone(),
            parent: (0, 0),
        }];
        for key in [
            "allow_workspace",
            "allow_workspaces",
            "allowed_workspaces",
            "approved_workspaces",
        ] {
            if let Some(path) = config[key].as_str().filter(|path| !path.is_empty()) {
                approved.push(
                    workspace_destination(path, true)
                        .map_err(|bad| bad["message"].as_str().unwrap_or("").to_owned())?,
                );
            }
            if let Some(paths) = config[key].as_array() {
                for path in paths {
                    if let Some(path) = path.as_str() {
                        approved.push(
                            workspace_destination(path, true)
                                .map_err(|bad| bad["message"].as_str().unwrap_or("").to_owned())?,
                        );
                    }
                }
            }
        }
        Ok(Self {
            workspace,
            state: json!({}),
            handles: HashMap::new(),
            approved,
            workspace_consent: None,
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
    if matches!(config["git_do"].as_str(), Some("recipe" | "jev")) {
        specs.push(crate::tool_git::git_do_spec(config));
    }
    if config["context_gc"].as_bool().unwrap_or(false) {
        specs.push(json!({"name":"recall","description":"Retrieve an original tool outcome hidden by context GC using its recall call ID. Returns the recorded bounded outcome verbatim as data, with a changed-since note and current digest for stale file reads. IDs not dropped or removed by compaction return not_found. Recall does not authorize a write.","input_schema":serde_json::from_str::<Value>(include_str!("schemas/recall.json")).unwrap(),"effect":"read"}));
    }
    if config["scripted"]
        .as_str()
        .is_some_and(|script| !script.is_empty())
    {
        specs.push(json!({"name":"echo","description":"Return the given text unchanged. A test tool: it reads nothing and changes nothing.","input_schema":serde_json::from_str::<Value>(include_str!("schemas/echo.json")).unwrap(),"effect":"read"}));
    }
    if config["agent"].as_bool().unwrap_or(false) {
        specs.retain(|spec| {
            matches!(
                spec["name"].as_str(),
                Some(
                    "read_file"
                        | "list_files"
                        | "search_text"
                        | "git_inspect"
                        | "recall"
                        | "report_friction"
                        | "edit_file"
                        | "write_file"
                        | "delete_file"
                        | "exec"
                )
            )
        });
    }
    if config["agents"].as_bool().unwrap_or(false) || config["agent"].as_bool().unwrap_or(false) {
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

/// decode_arguments validates one tool argument object against its input schema.
pub fn decode_arguments(arguments: &str, schema: &Value) -> Result<Value, Value> {
    if !arguments.trim_start().starts_with('{') {
        return Err(fail(
            "invalid_arguments",
            "arguments must be one JSON object",
        ));
    }
    let mut decoder = serde_json::Deserializer::from_str(arguments);
    let raw: &serde_json::value::RawValue =
        serde::Deserialize::deserialize(&mut decoder).map_err(|e| {
            fail(
                "invalid_arguments",
                &crate::errors::json_error(arguments, &e),
            )
        })?;
    fn validate(raw: &[u8], schema: &Value, field: &str) -> Option<String> {
        use crate::provider::raw_tokens;
        // Typed Go fields accept null; RawMessage null rules belong to each tool.
        if raw.starts_with(b"null") {
            return None;
        }
        let kind = schema["type"].as_str().unwrap_or("object");
        let valid = match kind {
            "object" => raw.starts_with(b"{"),
            "array" => raw.starts_with(b"["),
            "string" => raw.starts_with(b"\""),
            "boolean" => raw == b"true" || raw == b"false",
            "integer" => serde_json::from_slice::<i64>(raw).is_ok(),
            _ => true,
        };
        if !valid {
            return Some(crate::errors::field_error(field));
        }
        if kind == "array" {
            for item in raw_tokens::array(raw) {
                if let Some(error) = validate(item, &schema["items"], field) {
                    return Some(error);
                }
            }
        }
        if kind == "object" {
            let properties = &schema["properties"];
            for (key, value) in raw_tokens::members(raw) {
                let Some(property) = properties.get(&key) else {
                    return Some(format!("unknown field {}", crate::errors::quote(&key)));
                };
                // These Go fields are RawMessage: only the final occurrence is validated by the tool.
                if field.is_empty()
                    && (matches!(property["type"].as_str(), Some("integer" | "boolean"))
                        || matches!(
                            key.as_str(),
                            "content" | "append_text" | "details" | "related_call_ids" | "edits"
                        )
                        || key == "expected_sha256" && properties.get("old_text").is_none()
                        || key == "paths" && properties.get("query").is_some())
                {
                    continue;
                }
                let path = if field.is_empty() {
                    key
                } else {
                    format!("{field}.{key}")
                };
                if let Some(error) = validate(value, property, &path) {
                    return Some(error);
                }
            }
        }
        None
    }
    // Syntax must win over decoding errors, but a later duplicate cannot erase the first type error.
    if let Some(mut error) = validate(raw.get().as_bytes(), schema, "") {
        // Go's exec decoder replaces failures with its existing argv shape hint.
        if schema["properties"].get("argv").is_some()
            && crate::provider::raw_tokens::members(raw.get().as_bytes())
                .into_iter()
                .rev()
                .find(|(key, _)| key == "argv")
                .is_some_and(|(_, value)| !value.starts_with(b"["))
            && decoder.end().is_ok()
        {
            error = "argv must be an array holding the first command's executable and arguments, such as [\"git\",\"fetch\",\"origin\"]; then holds only the commands after it".into();
        }
        return Err(fail("invalid_arguments", &error));
    }
    if decoder.end().is_err() {
        return Err(fail(
            "invalid_arguments",
            "arguments must be one JSON object",
        ));
    }
    serde_json::from_str(arguments).map_err(|error| {
        fail(
            "invalid_arguments",
            &crate::errors::json_error(arguments, &error),
        )
    })
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
        let mut unrestricted = config.clone();
        unrestricted["read_only"] = json!(false);
        let known = config["agent"] != true
            && specs(&unrestricted)
                .as_array()
                .unwrap()
                .iter()
                .any(|spec| spec["name"] == name);
        if known {
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
    // Both git_do modes decode the same Go struct; only Jev advertises intent.
    let schema = if name == "git_do" {
        serde_json::from_str(include_str!("schemas/git_do_jev.json")).unwrap()
    } else {
        spec.unwrap()["input_schema"].clone()
    };
    let args = match decode_arguments(arguments, &schema) {
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
        "echo" => {
            let text = args["text"].as_str().unwrap_or("");
            if text.is_empty() {
                return Ok(fail("invalid_arguments", "text must not be empty"));
            }
            Ok(outcome(true, "ok", "", json!({"text":text}), "none"))
        }
        "recall" => Ok(if args["id"].as_str().unwrap_or("").is_empty() {
            fail("invalid_arguments", "id must not be empty")
        } else {
            fail("not_found", "no context GC result with that ID")
        }),
        _ => Ok(
            crate::tool_files::execute(&mut state.workspace, config, name, &args)
                .unwrap_or_else(|bad| bad),
        ),
    }
}

pub(crate) fn destination(path: &str, prospective: bool) -> Result<(PathBuf, bool), Value> {
    workspace_destination(path, prospective)
        .map(|destination| (destination.path, destination.missing))
}

fn workspace_destination(path: &str, prospective: bool) -> Result<WorkspaceDestination, Value> {
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
    let requested = clean(Path::new(path));
    let Some(parent) = requested.parent() else {
        return Err(fail(
            "invalid_path",
            "the filesystem root cannot be selected",
        ));
    };
    let parent = fs::canonicalize(parent).map_err(os_outcome)?;
    let parent_info = fs::metadata(&parent).map_err(os_outcome)?;
    if !parent_info.is_dir() {
        return Err(fail("not_directory", "workspace parent is not a directory"));
    }
    let spelling = parent.join(requested.file_name().unwrap());
    let path = spelling.clone();
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
    if !missing {
        crate::workspace::validate_destination(&root)?;
    }
    Ok(WorkspaceDestination {
        path: root,
        missing,
        spelling,
        requested,
        parent: (parent_info.dev(), parent_info.ino()),
    })
}

fn permitted(state: &ToolState, destination: &WorkspaceDestination) -> Result<bool, Value> {
    let mut permitted = false;
    for grant in &state.approved {
        if !destination.path.starts_with(&grant.path)
            && !destination.spelling.starts_with(&grant.path)
            && !destination.requested.starts_with(&grant.requested)
        {
            continue;
        }
        if grant.missing {
            let parent = grant.path.parent().unwrap();
            if fs::metadata(parent)
                .map(|info| (info.dev(), info.ino()))
                .ok()
                != Some(grant.parent)
            {
                return Err(fail("workspace_validation_failed", "the approved destination's parent changed; restart to approve the changed location"));
            }
            if fs::canonicalize(parent).ok().as_deref() != Some(parent) {
                return Err(fail(
                    "workspace_validation_failed",
                    "the approved destination's parent was redirected",
                ));
            }
            if fs::symlink_metadata(&grant.path).is_ok_and(|info| info.file_type().is_symlink()) {
                return Err(fail(
                    "workspace_validation_failed",
                    "the approved destination became a symlink",
                ));
            }
        }
        permitted |= destination.path.starts_with(&grant.path);
    }
    Ok(permitted)
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
    let request = name == "request_workspace_access";
    let destination = workspace_destination(path, request).map_err(|mut bad| {
        if !request && bad["code"] == "not_found" {
            bad["message"] = json!("workspace must exist; request_workspace_access can approve the exact destination before creation");
        }
        bad
    })?;
    if crate::cancellation::cancelled(config) {
        return Err(fail("permission_denied", "workspace request cancelled"));
    }
    if !permitted(state, &destination)? {
        let Some(consent) = state.workspace_consent.as_mut() else {
            return Err(fail("permission_required", "workspace access needs human consent; use interactive chat or --allow-workspace PATH"));
        };
        match consent(config, &destination) {
            Err(WorkspaceConsentError::NotInteractive) => return Err(fail("permission_required", "workspace consent needs a terminal; preapprove the exact path with --allow-workspace PATH")),
            Err(WorkspaceConsentError::Unavailable) => return Err(fail("permission_denied", "workspace consent was cancelled or unavailable")),
            Ok(allowed) => {
                if !allowed || crate::cancellation::cancelled(config) {
                    return Err(fail("permission_denied", "workspace access was not approved"));
                }
            }
        }
    }
    let root = destination.path.clone();
    let missing = destination.missing;
    if !request && root == state.workspace.root {
        return Ok(workspace_outcome(
            &state.workspace,
            json!({"previous_workspace":root.to_string_lossy(),"workspace":root.to_string_lossy(),"changed":false}),
            "none",
        ));
    }
    let current = workspace_destination(
        if request {
            path
        } else {
            destination.spelling.to_str().unwrap()
        },
        request,
    )?;
    if current.path != root
        || current.parent != destination.parent
        || crate::cancellation::cancelled(config)
    {
        return Err(fail(
            "workspace_validation_failed",
            if request {
                "destination changed or request was cancelled during consent"
            } else {
                "workspace changed or request was cancelled during consent"
            },
        ));
    }
    permitted(state, &current)?;
    if request {
        let out = workspace_outcome(
            &state.workspace,
            json!({"approved_path":root.to_string_lossy(),"workspace":state.workspace.root.to_string_lossy(),"missing":missing}),
            "none",
        );
        if crate::types::json_bytes(&out).len() > crate::workspace::MAX_RESULT_BYTES {
            return Err(fail(
                "invalid_arguments",
                "workspace approval metadata exceeds the result limit",
            ));
        }
        if !state.approved.iter().any(|grant| grant.path == root) {
            state.approved.push(destination);
        }
        return Ok(out);
    }
    let previous = state.workspace.root.clone();
    let mut next_config = config.clone();
    next_config["workspace"] = json!(root.to_string_lossy());
    if state
        .approved
        .first()
        .is_some_and(|grant| grant.path == root)
    {
        next_config["read_only"] = json!(false);
        next_config["plan"] = json!(false);
    }
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
    if crate::cancellation::cancelled(config) {
        return Err(fail("permission_denied", "workspace request cancelled"));
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
    let mut next = state
        .handles
        .remove(&root)
        .map(Ok)
        .unwrap_or_else(|| Workspace::open(&root.to_string_lossy()))
        .map_err(|e| fail("io_error", &e))?;
    next.publish_mu = state.workspace.publish_mu.clone();
    let old = std::mem::replace(&mut state.workspace, next);
    state.handles.insert(previous, old);
    if !state.approved.iter().any(|grant| grant.path == root) {
        state.approved.push(destination);
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
    fn child_declarations_preserve_parent_schema_bytes_and_limit_authority() {
        for read_only in [false, true] {
            for plan in [false, true] {
                for context_gc in [false, true] {
                    for friction in [false, true] {
                        let parent = json!({"agents":true,"read_only":read_only,"plan":plan,"context_gc":context_gc,"report_friction":friction,"scripted":"responses.json","git_do":"jev"});
                        let mut child = parent.clone();
                        child["agent"] = json!(true);
                        child["agents"] = json!(false);
                        let declarations = specs(&child);
                        let mut expected =
                            vec!["git_inspect", "list_files", "read_file", "search_text"];
                        if !read_only {
                            expected.extend(["delete_file", "edit_file", "exec", "write_file"]);
                        }
                        if context_gc {
                            expected.push("recall");
                        }
                        if friction {
                            expected.push("report_friction");
                        }
                        let agent_specs = crate::agents::specs();
                        expected.extend(
                            agent_specs
                                .iter()
                                .map(|spec| spec["name"].as_str().unwrap()),
                        );
                        expected.sort();
                        let actual: Vec<_> = declarations
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|spec| spec["name"].as_str().unwrap())
                            .collect();
                        assert_eq!(actual, expected, "{child}");
                        let parent_specs = specs(&parent);
                        for spec in declarations.as_array().unwrap() {
                            let original = parent_specs
                                .as_array()
                                .unwrap()
                                .iter()
                                .find(|original| original["name"] == spec["name"])
                                .unwrap();
                            assert_eq!(spec, original);
                            let name = spec["name"].as_str().unwrap();
                            assert_eq!(
                                crate::schemas::raw_schema(name, &spec["input_schema"]),
                                crate::schemas::raw_schema(name, &original["input_schema"])
                            );
                            assert!(
                                crate::schemas::raw_schema(name, &spec["input_schema"]).is_some()
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn child_dispatch_rejects_root_tools_and_respects_write_authority() {
        let root = tempfile::tempdir().unwrap();
        for read_only in [false, true] {
            let config = json!({"workspace":root.path(),"agent":true,"agents":true,"read_only":read_only,"scripted":"responses.json","git_do":"recipe"});
            let mut state = ToolState::new(&config).unwrap();
            for name in [
                "echo",
                "git_do",
                "request_workspace_access",
                "switch_workspace",
            ] {
                assert_eq!(
                    execute(&mut state, &config, name, "{}").unwrap()["code"],
                    "tool_unavailable",
                    "{name}"
                );
            }
            let result = execute(
                &mut state,
                &config,
                "write_file",
                r#"{"path":"child","content":"created"}"#,
            )
            .unwrap();
            assert_eq!(
                result["code"],
                if read_only { "tool_unavailable" } else { "ok" }
            );
        }
    }

    #[test]
    fn git_do_decodes_intent_before_mode_validation_without_changing_its_spec() {
        let dir = tempfile::tempdir().unwrap();
        for mode in ["recipe", "jev"] {
            let config = json!({"workspace":dir.path(),"git_do":mode});
            let mut state = ToolState::new(&config).unwrap();
            let declarations = specs(&config);
            let spec = declarations
                .as_array()
                .unwrap()
                .iter()
                .find(|spec| spec["name"] == "git_do")
                .unwrap();
            assert_eq!(
                spec["input_schema"]["properties"].get("intent").is_some(),
                mode == "jev"
            );
            assert!(crate::schemas::raw_schema("git_do", &spec["input_schema"]).is_some());
            for (arguments, message) in [
                (
                    r#"{"recipe":"status","intent":7}"#,
                    "field \"intent\" must be a string",
                ),
                (
                    r#"{"recipe":7,"intent":"status"}"#,
                    "field \"recipe\" must be a string",
                ),
                (
                    r#"{"recipe":"status","intent":"status","unexpected":true}"#,
                    "unknown field \"unexpected\"",
                ),
            ] {
                let result = execute(&mut state, &config, "git_do", arguments).unwrap();
                assert_eq!(result["code"], "invalid_arguments", "{mode}: {arguments}");
                assert_eq!(result["message"], message, "{mode}: {arguments}");
            }
            if mode == "recipe" {
                for intent in [json!("status"), json!(" "), json!(""), Value::Null] {
                    let arguments =
                        json!({"recipe":"status","intent":intent,"paths":[]}).to_string();
                    let result = execute(&mut state, &config, "git_do", &arguments).unwrap();
                    assert_eq!(result["code"], "invalid_arguments");
                    assert_eq!(
                        result["message"],
                        if intent.as_str().is_some_and(|s| !s.is_empty()) {
                            "recipe must be one of the listed outcomes"
                        } else {
                            "paths must name at least one file or directory"
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn git_do_recipe_status_still_executes_after_common_decoding() {
        let dir = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "--quiet", "-b", "main"])
            .arg(dir.path())
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "base"
            ])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        let config = json!({"workspace":dir.path(),"git_do":"recipe"});
        let mut state = ToolState::new(&config).unwrap();
        for arguments in [
            r#"{"recipe":"status"}"#,
            r#"{"recipe":"status","intent":null}"#,
            r#"{"recipe":"status","intent":""}"#,
        ] {
            let result = execute(&mut state, &config, "git_do", arguments).unwrap();
            assert_eq!(result["ok"], true, "{result}");
            assert_eq!(result["data"]["recipe"], "status");
        }
    }

    #[test]
    fn recall_argument_decoder_uses_common_json_contract() {
        let schema: Value = serde_json::from_str(include_str!("schemas/recall.json")).unwrap();
        for (arguments, message) in [
            ("[]", "arguments must be one JSON object"),
            ("7", "arguments must be one JSON object"),
            ("null", "arguments must be one JSON object"),
            (r#"{"id":}"#, "invalid JSON at byte 7"),
            ("{", "invalid JSON at byte 2: incomplete input"),
            ("{} {}", "arguments must be one JSON object"),
            (r#"{"unexpected":true}"#, "unknown field \"unexpected\""),
            (r#"{"id":7}"#, "field \"id\" must be a string"),
        ] {
            let result = decode_arguments(arguments, &schema).unwrap_err();
            assert_eq!(result["code"], "invalid_arguments", "{arguments}");
            assert_eq!(result["message"], message, "{arguments}");
        }
        for arguments in ["{}", r#"{"id":null}"#, r#"{"id":"original"}"#] {
            assert!(decode_arguments(arguments, &schema).is_ok(), "{arguments}");
        }
    }

    #[test]
    fn argument_decoder_keeps_the_first_error_across_duplicate_fields() {
        let read: Value = serde_json::from_str(include_str!("schemas/read_file.json")).unwrap();
        let exec: Value = serde_json::from_str(include_str!("schemas/exec.json")).unwrap();
        let edit: Value = serde_json::from_str(include_str!("schemas/edit_file.json")).unwrap();
        let spawn = crate::agents::specs()
            .into_iter()
            .find(|spec| spec["name"] == "spawn")
            .unwrap()["input_schema"]
            .clone();
        let argv_hint = "argv must be an array holding the first command's executable and arguments, such as [\"git\",\"fetch\",\"origin\"]; then holds only the commands after it";
        for (schema, arguments, message) in [
            (
                &read,
                r#"{"path":42,"path":"file"}"#,
                "field \"path\" must be a string",
            ),
            (
                &read,
                r#"{"path":"file","path":42}"#,
                "field \"path\" must be a string",
            ),
            (
                &read,
                r#"{"path":{},"path":"file"}"#,
                "field \"path\" must be a string",
            ),
            (
                &read,
                r#"{"path":[],"path":"file"}"#,
                "field \"path\" must be a string",
            ),
            (
                &read,
                r#"{"path":false,"path":null}"#,
                "field \"path\" must be a string",
            ),
            (
                &read,
                r#"{"path":42,"path":"file","unknown":0}"#,
                "field \"path\" must be a string",
            ),
            (
                &read,
                r#"{"unknown":0,"path":42,"path":"file"}"#,
                "unknown field \"unknown\"",
            ),
            (
                &read,
                r#"{"path":42,"path":"file"} {}"#,
                "field \"path\" must be a string",
            ),
            (&read, r#"{"unknown":0} {}"#, "unknown field \"unknown\""),
            (&exec, r#"{"argv":"echo"}"#, argv_hint),
            (&exec, r#"{"argv":["echo"],"argv":7}"#, argv_hint),
            (&exec, r#"{"argv":[7],"argv":null}"#, argv_hint),
            (&exec, r#"{"unknown":0,"argv":"echo"}"#, argv_hint),
            (&exec, r#"{"argv":null,"cwd":7}"#, argv_hint),
            (
                &exec,
                r#"{"argv":"echo"} {}"#,
                "field \"argv\" must be an array of strings",
            ),
            (
                &exec,
                r#"{"argv":[7],"argv":["echo"]}"#,
                "field \"argv\" must be an array of strings",
            ),
            (
                &exec,
                r#"{"argv":["echo"],"argv":[7]}"#,
                "field \"argv\" must be an array of strings",
            ),
            (
                &exec,
                r#"{"then":[[7]],"then":[["echo"]]}"#,
                "field \"then\" must be an array of command arrays",
            ),
            (
                &exec,
                r#"{"then":[["echo"]],"then":[[7]]}"#,
                "field \"then\" must be an array of command arrays",
            ),
            (
                &spawn,
                r#"{"context":{"brief":7,"brief":"ok"}}"#,
                "field \"context.brief\" must be a string",
            ),
            (
                &spawn,
                r#"{"context":{"brief":"ok","brief":7}}"#,
                "field \"context.brief\" must be a string",
            ),
            (
                &spawn,
                r#"{"context":{"brief":7},"context":{"brief":"ok"}}"#,
                "field \"context.brief\" must be a string",
            ),
            (
                &spawn,
                r#"{"context":{"files":[7]},"context":{"files":["file"]}}"#,
                "field \"context.files\" must be an array of strings",
            ),
            (
                &spawn,
                r#"{"context":{"unknown":0,"brief":7},"task":7}"#,
                "unknown field \"unknown\"",
            ),
            (
                &spawn,
                r#"{"context":{"brief":7,"unknown":0},"task":7}"#,
                "field \"context.brief\" must be a string",
            ),
            (
                &edit,
                r#"{"expected_sha256":7,"expected_sha256":"digest"}"#,
                "field \"expected_sha256\" must be a string",
            ),
        ] {
            let result = decode_arguments(arguments, schema).unwrap_err();
            assert_eq!(result["code"], "invalid_arguments", "{arguments}");
            assert_eq!(result["message"], message, "{arguments}");
        }
        for (arguments, message) in [
            (r#"{"path":42,"path":}"#, "invalid JSON at byte 19"),
            (r#"{"unknown":0,"path":}"#, "invalid JSON at byte 21"),
        ] {
            let result = decode_arguments(arguments, &read).unwrap_err();
            assert_eq!(result["message"], message, "{arguments}");
        }
    }

    #[test]
    fn argument_decoder_preserves_nulls_arrays_and_deferred_raw_fields() {
        let read: Value = serde_json::from_str(include_str!("schemas/read_file.json")).unwrap();
        let exec: Value = serde_json::from_str(include_str!("schemas/exec.json")).unwrap();
        let edit: Value = serde_json::from_str(include_str!("schemas/edit_file.json")).unwrap();
        let write: Value = serde_json::from_str(include_str!("schemas/write_file.json")).unwrap();
        let search: Value = serde_json::from_str(include_str!("schemas/search_text.json")).unwrap();
        for (schema, arguments, expected) in [
            (
                &read,
                r#"{"path":null,"path":"file"}"#,
                json!({"path":"file"}),
            ),
            (
                &read,
                r#"{"path":"file","path":null}"#,
                json!({"path":null}),
            ),
            (
                &read,
                r#"{"path":"first","path":"last"}"#,
                json!({"path":"last"}),
            ),
            (
                &read,
                r#"{"max_lines":"wrong","max_lines":1}"#,
                json!({"max_lines":1}),
            ),
            (
                &exec,
                r#"{"argv":null,"argv":[null,"echo"]}"#,
                json!({"argv":[null,"echo"]}),
            ),
            (
                &exec,
                r#"{"argv":["echo"],"argv":null,"then":[null,[null]]}"#,
                json!({"argv":null,"then":[null,[null]]}),
            ),
            (
                &write,
                r#"{"content":7,"content":"ok","expected_sha256":[],"expected_sha256":"digest"}"#,
                json!({"content":"ok","expected_sha256":"digest"}),
            ),
            (
                &edit,
                r#"{"edits":[{"old_text":7,"old_text":"old"}],"append_text":7,"append_text":"ok"}"#,
                json!({"edits":[{"old_text":"old"}],"append_text":"ok"}),
            ),
            (
                &search,
                r#"{"paths":[7],"paths":["file"],"regex":[],"regex":false}"#,
                json!({"paths":["file"],"regex":false}),
            ),
        ] {
            assert_eq!(
                decode_arguments(arguments, schema).unwrap(),
                expected,
                "{arguments}"
            );
        }
    }

    #[test]
    fn duplicate_wrong_path_cannot_publish_a_write_even_when_the_last_path_is_valid() {
        let directory = tempfile::tempdir().unwrap();
        let config = json!({"workspace":directory.path()});
        let mut state = ToolState::new(&config).unwrap();
        for arguments in [
            r#"{"path":7,"path":"file","content":"created"}"#,
            r#"{"path":"file","path":7,"content":"created"}"#,
        ] {
            let result = execute(&mut state, &config, "write_file", arguments).unwrap();
            assert_eq!(result["code"], "invalid_arguments");
            assert_eq!(result["message"], "field \"path\" must be a string");
            assert!(!directory.path().join("file").exists());
        }
        let result = execute(
            &mut state,
            &config,
            "write_file",
            r#"{"path":"first","path":"file","content":"created"}"#,
        )
        .unwrap();
        assert_eq!(result["ok"], true);
        assert!(!directory.path().join("first").exists());
        assert_eq!(
            std::fs::read_to_string(directory.path().join("file")).unwrap(),
            "created"
        );
    }

    #[test]
    fn search_regex_errors_use_reference_byte_positions() {
        let dir = tempfile::tempdir().unwrap();
        let config = json!({"workspace":dir.path()});
        let mut state = ToolState::new(&config).unwrap();
        for (pattern, message) in [
            ("[", "invalid RE2 regex at byte 1: unclosed character class"),
            (
                "(?=a)",
                "invalid RE2 regex at byte 1: unsupported RE2 construct",
            ),
            (r"(a)\1", "invalid RE2 regex at byte 4: invalid escape"),
        ] {
            let arguments = json!({"path":".","query":pattern,"regex":true}).to_string();
            let result = execute(&mut state, &config, "search_text", &arguments).unwrap();
            assert_eq!(result["code"], "invalid_arguments");
            assert_eq!(result["message"], message);
        }
        fs::write(dir.path().join("file"), "alpha\nbeta\n").unwrap();
        let result = execute(
            &mut state,
            &config,
            "search_text",
            r#"{"path":"file","query":"alpha|beta","regex":true}"#,
        )
        .unwrap();
        assert_eq!(result["data"]["matches"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn workspace_consent_approves_once_without_lifting_read_only_or_plan_mode() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        for read_only in [false, true] {
            for plan in [false, true] {
                for prospective in [false, true] {
                    let dir = tempfile::tempdir().unwrap();
                    let launch = dir.path().join("launch");
                    let target = dir.path().join("target");
                    fs::create_dir(&launch).unwrap();
                    if !prospective {
                        fs::create_dir(&target).unwrap();
                    }
                    let config = json!({"workspace":launch,"read_only":read_only,"plan":plan,"session_id":crate::types::new_id()});
                    let mut state = ToolState::new(&config).unwrap();
                    let calls = Arc::new(AtomicUsize::new(0));
                    let observed = calls.clone();
                    let expected = target.clone();
                    state.workspace_consent = Some(Box::new(move |config, destination| {
                        observed.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(destination.path, expected);
                        assert_eq!(destination.missing, prospective);
                        assert_eq!(config["read_only"], read_only);
                        assert_eq!(config["plan"], plan);
                        Ok(true)
                    }));
                    let arguments = json!({"path":target}).to_string();
                    let first = if prospective {
                        "request_workspace_access"
                    } else {
                        "switch_workspace"
                    };
                    assert_eq!(
                        execute(&mut state, &config, first, &arguments).unwrap()["ok"],
                        true
                    );
                    if prospective {
                        assert!(!target.exists());
                        assert_eq!(state.workspace.root, launch);
                        fs::create_dir(&target).unwrap();
                    }
                    let result =
                        execute(&mut state, &config, "switch_workspace", &arguments).unwrap();
                    assert_eq!(result["ok"], true, "{result}");
                    assert_eq!(state.workspace.root, target);
                    assert_eq!(calls.load(Ordering::SeqCst), 1);
                    assert!(state
                        .approved_paths()
                        .contains(&target.to_string_lossy().into_owned()));
                    let write = execute(
                        &mut state,
                        &config,
                        "write_file",
                        r#"{"path":"file","content":"new"}"#,
                    )
                    .unwrap();
                    assert_eq!(
                        write["code"],
                        if read_only {
                            "permission_denied"
                        } else if plan {
                            "plan_mode"
                        } else {
                            "ok"
                        }
                    );
                    assert_eq!(config["plan"], plan);
                }
            }
        }
    }

    #[test]
    fn workspace_consent_keeps_grants_across_reset_and_restricts_following_tools() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        for (read_only, plan) in [(true, false), (false, true)] {
            let dir = tempfile::tempdir().unwrap();
            let launch = dir.path().join("launch");
            let target = dir.path().join("target");
            fs::create_dir(&launch).unwrap();
            fs::create_dir(&target).unwrap();
            let script = dir.path().join("script.json");
            let call = |id: &str, name: &str, arguments: Value| json!({"blocks":[{"kind":"tool_call","call":{"call_id":id,"name":name,"arguments":arguments.to_string()}}]});
            fs::write(
                &script,
                json!([
                    call("select", "switch_workspace", json!({"path":target})),
                    call("write", "write_file", json!({"path":"forbidden","content":"bad"})),
                    {"blocks":[{"kind":"text","text":"done"}]}
                ])
                .to_string(),
            )
            .unwrap();
            let config = json!({"workspace":launch,"provider":"scripted","model":"scripted","scripted":script,"read_only":read_only,"plan":plan,"trace_file":dir.path().join("trace.jsonl")});
            let mut session = crate::runtime::Session::new(config.clone()).unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            session.tools.workspace_consent = Some(Box::new(move |_, _| {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            }));
            let result = session.submit("select the workspace and try the change");
            assert_eq!(result["status"], "completed", "{result}");
            assert_eq!(session.tools.workspace.root, target);
            assert!(!target.join("forbidden").exists());
            let refused = session
                .history
                .iter()
                .find(|entry| entry["tool"]["call_id"] == "write")
                .unwrap();
            assert_eq!(
                refused["tool"]["outcome"]["code"],
                if read_only {
                    "permission_denied"
                } else {
                    "plan_mode"
                }
            );
            session.reset();
            assert!(session.tools.workspace_consent.is_some());
            for path in [&launch, &target] {
                let result = execute(
                    &mut session.tools,
                    &session.config,
                    "switch_workspace",
                    &json!({"path":path}).to_string(),
                )
                .unwrap();
                assert_eq!(result["ok"], true, "{result}");
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let fresh = ToolState::new(&config).unwrap();
            assert!(fresh.workspace_consent.is_none());
            assert_eq!(
                fresh.approved_paths(),
                vec![launch.to_string_lossy().into_owned()]
            );
        }
    }

    #[test]
    fn workspace_consent_denial_and_input_errors_leave_selection_and_grants_unchanged() {
        for tool in ["request_workspace_access", "switch_workspace"] {
            for (reply, code, message) in [
                (None, "permission_required", "workspace access needs human consent; use interactive chat or --allow-workspace PATH"),
                (Some(Ok(false)), "permission_denied", "workspace access was not approved"),
                (Some(Err(WorkspaceConsentError::NotInteractive)), "permission_required", "workspace consent needs a terminal; preapprove the exact path with --allow-workspace PATH"),
                (Some(Err(WorkspaceConsentError::Unavailable)), "permission_denied", "workspace consent was cancelled or unavailable"),
            ] {
                let dir = tempfile::tempdir().unwrap();
                let launch = dir.path().join("launch");
                let target = dir.path().join("target");
                fs::create_dir(&launch).unwrap();
                fs::create_dir(&target).unwrap();
                let config = json!({"workspace":launch,"session_id":crate::types::new_id()});
                let mut state = ToolState::new(&config).unwrap();
                let original = state.approved_paths();
                if let Some(reply) = reply {
                    state.workspace_consent = Some(Box::new(move |_, _| reply));
                }
                let result = execute(&mut state, &config, tool, &json!({"path":target}).to_string()).unwrap();
                assert_eq!(result["code"], code);
                assert_eq!(result["message"], message);
                assert_eq!(state.workspace.root, launch);
                assert_eq!(state.approved_paths(), original);
            }
        }
    }

    #[test]
    fn workspace_consent_revalidates_parent_and_symlinks_before_publication() {
        use std::os::unix::fs::symlink;
        for tool in ["request_workspace_access", "switch_workspace"] {
            for change in ["parent-swap", "leaf-redirect", "parent-redirect", "cancel"] {
                let dir = tempfile::tempdir().unwrap();
                let launch = dir.path().join("launch");
                let parent = dir.path().join("parent");
                let other = dir.path().join("other");
                let target = parent.join("target");
                for path in [&launch, &parent, &other] {
                    fs::create_dir(path).unwrap();
                }
                fs::create_dir(&target).unwrap();
                fs::create_dir(other.join("target")).unwrap();
                let config = json!({"workspace":launch,"session_id":crate::types::new_id()});
                let mut state = ToolState::new(&config).unwrap();
                let old = dir.path().join("old");
                let parent = parent.clone();
                let destination = target.clone();
                state.workspace_consent = Some(Box::new(move |config, _| {
                    match change {
                        "parent-swap" => {
                            fs::rename(&parent, &old).unwrap();
                            fs::create_dir(&parent).unwrap();
                            fs::create_dir(&destination).unwrap();
                        }
                        "leaf-redirect" => {
                            fs::remove_dir(&destination).unwrap();
                            symlink(&other, &destination).unwrap();
                        }
                        "parent-redirect" => {
                            fs::rename(&parent, &old).unwrap();
                            symlink(&other, &parent).unwrap();
                        }
                        "cancel" => crate::cancellation::cancel(config),
                        _ => unreachable!(),
                    }
                    Ok(true)
                }));
                let original = state.approved_paths();
                let result = execute(
                    &mut state,
                    &config,
                    tool,
                    &json!({"path":target}).to_string(),
                )
                .unwrap();
                assert_eq!(
                    result["code"],
                    if change == "cancel" {
                        "permission_denied"
                    } else {
                        "workspace_validation_failed"
                    },
                    "{tool}: {change}: {result}"
                );
                assert_eq!(
                    result["message"],
                    if change == "cancel" {
                        "workspace access was not approved"
                    } else if tool == "request_workspace_access" {
                        "destination changed or request was cancelled during consent"
                    } else {
                        "workspace changed or request was cancelled during consent"
                    }
                );
                assert_eq!(state.workspace.root, launch);
                assert_eq!(state.approved_paths(), original);
            }
        }
    }

    #[test]
    fn workspace_consent_does_not_prompt_for_invalid_or_cancelled_requests() {
        let dir = tempfile::tempdir().unwrap();
        let launch = dir.path().join("launch");
        let target = dir.path().join("target");
        fs::create_dir(&launch).unwrap();
        fs::create_dir(&target).unwrap();
        let config = json!({"workspace":launch,"session_id":crate::types::new_id()});
        let mut state = ToolState::new(&config).unwrap();
        state.workspace_consent = Some(Box::new(|_, _| panic!("must validate before prompting")));
        let bad = execute(
            &mut state,
            &config,
            "request_workspace_access",
            r#"{"path":"relative"}"#,
        )
        .unwrap();
        assert_eq!(bad["code"], "invalid_path");
        crate::cancellation::cancel(&config);
        let result = execute(
            &mut state,
            &config,
            "switch_workspace",
            &json!({"path":target}).to_string(),
        )
        .unwrap();
        assert_eq!(result["message"], "workspace request cancelled");
        assert_eq!(
            state.approved_paths(),
            vec![launch.to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn workspace_grants_are_sorted_canonical_and_keep_exact_reservations() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let launch = dir.path().join("z_launch");
        let target = dir.path().join("a_target");
        let alias = dir.path().join("alias");
        fs::create_dir(&launch).unwrap();
        fs::create_dir(&target).unwrap();
        symlink(&target, &alias).unwrap();
        let config = json!({"workspace":launch,"allow_workspaces":[alias,target]});
        let mut state = ToolState::new(&config).unwrap();
        assert_eq!(
            state.approved_paths(),
            vec![
                target.to_string_lossy().into_owned(),
                launch.to_string_lossy().into_owned()
            ]
        );
        let future = target.join("future");
        let arguments = json!({"path":future}).to_string();
        assert_eq!(
            execute(&mut state, &config, "request_workspace_access", &arguments).unwrap()["ok"],
            true
        );
        assert!(state
            .approved_paths()
            .contains(&future.to_string_lossy().into_owned()));
    }

    #[test]
    fn malformed_worktree_metadata_is_rejected_before_consent() {
        use std::os::unix::fs::symlink;
        for symlink_metadata in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let launch = dir.path().join("launch");
            let target = dir.path().join("unapproved");
            fs::create_dir(&launch).unwrap();
            fs::create_dir(&target).unwrap();
            if symlink_metadata {
                fs::create_dir(dir.path().join("metadata")).unwrap();
                symlink(dir.path().join("metadata"), target.join(".git")).unwrap();
            } else {
                fs::write(target.join(".git"), "not linked-worktree metadata\n").unwrap();
            }
            let config = json!({"workspace":launch});
            let mut state = ToolState::new(&config).unwrap();
            let arguments = json!({"path":target}).to_string();
            for name in ["request_workspace_access", "switch_workspace"] {
                let result = execute(&mut state, &config, name, &arguments).unwrap();
                assert_eq!(result["code"], "workspace_validation_failed");
                assert_eq!(
                    result["message"],
                    if symlink_metadata {
                        "cannot validate workspace Git metadata"
                    } else {
                        "cannot identify linked worktree"
                    }
                );
                assert_eq!(state.workspace.root, launch);
            }
        }
    }

    #[test]
    fn returning_to_launch_restores_full_snapshot_in_read_only_mode() {
        let dir = tempfile::tempdir().unwrap();
        let launch = dir.path().join("launch");
        let target = dir.path().join("target");
        fs::create_dir(&launch).unwrap();
        fs::create_dir(&target).unwrap();
        let config = json!({"workspace":launch,"read_only":true,"allow_workspaces":[target]});
        let mut state = ToolState::new(&config).unwrap();
        let result = execute(
            &mut state,
            &config,
            "switch_workspace",
            &json!({"path":target}).to_string(),
        )
        .unwrap();
        assert_eq!(
            result["data"]["workspace_state"]["counts"],
            "omitted in read-only/plan mode"
        );
        let result = execute(
            &mut state,
            &config,
            "switch_workspace",
            &json!({"path":launch}).to_string(),
        )
        .unwrap();
        assert!(result["data"]["workspace_state"].get("counts").is_none());
    }

    #[test]
    fn optional_declarations_use_launch_feature_flags_and_raw_schemas() {
        for config in [
            json!({}),
            json!({"context_gc":true,"scripted":"responses.json","git_do":"recipe"}),
            json!({"context_gc":true,"git_do":"jev","read_only":true}),
        ] {
            let specs = specs(&config);
            let tools = specs.as_array().unwrap();
            let find = |name| tools.iter().find(|spec| spec["name"] == name);
            assert_eq!(find("recall").is_some(), config["context_gc"] == true);
            assert_eq!(find("echo").is_some(), config["scripted"].is_string());
            assert_eq!(
                find("git_do").is_some(),
                config["git_do"].is_string() && config["read_only"] != true
            );
            for spec in tools {
                assert!(
                    crate::schemas::raw_schema(
                        spec["name"].as_str().unwrap(),
                        &spec["input_schema"]
                    )
                    .is_some(),
                    "{}",
                    spec["name"]
                );
            }
        }
    }

    #[test]
    fn scripted_echo_and_disabled_feature_dispatch_follow_registry() {
        let dir = tempfile::tempdir().unwrap();
        let config = json!({"workspace":dir.path(),"scripted":"responses.json","read_only":true});
        let mut state = ToolState::new(&config).unwrap();
        for name in ["threads", "git_do", "recall", "report_friction"] {
            assert_eq!(
                execute(&mut state, &config, name, "{}").unwrap()["code"],
                "tool_unavailable"
            );
        }
        assert_eq!(
            execute(&mut state, &config, "write_file", "{}").unwrap()["code"],
            "permission_denied"
        );
        let args = json!({"text":"héllo <>&\n\u{2028}"});
        assert_eq!(
            execute(&mut state, &config, "echo", &args.to_string()).unwrap()["data"],
            args
        );
        assert_eq!(
            execute(&mut state, &config, "echo", "{}").unwrap()["code"],
            "invalid_arguments"
        );
    }

    #[test]
    fn launch_grants_allow_creation_switch_and_rebound_writes() {
        let dir = tempfile::tempdir().unwrap();
        let launch = dir.path().join("launch");
        let target = dir.path().join("future");
        fs::create_dir(&launch).unwrap();
        let config = json!({"workspace":launch,"allow_workspaces":[target]});
        let mut state = ToolState::new(&config).unwrap();
        let args = json!({"path":target}).to_string();
        assert_eq!(
            execute(&mut state, &config, "request_workspace_access", &args).unwrap()["data"]
                ["missing"],
            true
        );
        fs::create_dir(&target).unwrap();
        fs::write(target.join("AGENTS.md"), "destination instructions\n").unwrap();
        let result = execute(&mut state, &config, "switch_workspace", &args).unwrap();
        assert_eq!(
            result["data"]["project_instructions"],
            "destination instructions\n"
        );
        assert_eq!(
            execute(
                &mut state,
                &config,
                "write_file",
                r#"{"path":"new","content":"created"}"#
            )
            .unwrap()["ok"],
            true
        );
        assert!(target.join("new").exists());
        assert!(!launch.join("new").exists());
    }

    #[test]
    fn prospective_grants_refuse_replaced_parents_and_symlink_destinations() {
        use std::os::unix::fs::symlink;
        for change in ["parent-swap", "parent-redirect", "leaf-symlink"] {
            let dir = tempfile::tempdir().unwrap();
            let launch = dir.path().join("launch");
            let parent = dir.path().join("parent");
            let other = dir.path().join("other");
            for path in [&launch, &parent, &other] {
                fs::create_dir(path).unwrap();
            }
            let target = parent.join("future");
            let config = json!({"workspace":launch,"allow_workspaces":[target]});
            let mut state = ToolState::new(&config).unwrap();
            let args = json!({"path":target}).to_string();
            assert_eq!(
                execute(&mut state, &config, "request_workspace_access", &args).unwrap()["ok"],
                true
            );
            match change {
                "parent-swap" => {
                    fs::rename(&parent, dir.path().join("original")).unwrap();
                    fs::create_dir(&parent).unwrap();
                    fs::create_dir(&target).unwrap();
                }
                "parent-redirect" => {
                    fs::rename(&parent, dir.path().join("original")).unwrap();
                    symlink(&other, &parent).unwrap();
                    fs::create_dir(&target).unwrap();
                }
                _ => symlink(&other, &target).unwrap(),
            }
            assert_eq!(
                execute(&mut state, &config, "switch_workspace", &args).unwrap()["code"],
                "workspace_validation_failed",
                "{change}"
            );
            assert_eq!(state.workspace.root, fs::canonicalize(launch).unwrap());
        }
    }

    #[test]
    fn publication_guard_survives_switches_without_sharing_observations() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let launch = dir.path().join("launch");
        let target = dir.path().join("target");
        fs::create_dir(&launch).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(launch.join("file"), "base\n").unwrap();
        let config = json!({"workspace":launch,"allow_workspaces":[target]});
        let mut parent = ToolState::new(&config).unwrap();
        assert_eq!(
            execute(&mut parent, &config, "read_file", r#"{"path":"file"}"#).unwrap()["ok"],
            true
        );
        let mut child = ToolState::new(&config).unwrap();
        child.share_publication_guard(&parent);
        let publication = parent.workspace.publish_mu.clone();
        assert!(Arc::ptr_eq(&publication, &child.workspace.publish_mu));
        assert!(child.workspace.seen.is_empty());
        assert_eq!(
            execute(
                &mut child,
                &config,
                "write_file",
                r#"{"path":"file","content":"unread"}"#
            )
            .unwrap()["code"],
            "invalid_arguments"
        );
        for path in [&target, &launch, &target] {
            let result = execute(
                &mut parent,
                &config,
                "switch_workspace",
                &json!({"path":path}).to_string(),
            )
            .unwrap();
            assert_eq!(result["ok"], true, "{result}");
            assert!(Arc::ptr_eq(&publication, &parent.workspace.publish_mu));
            assert!(parent
                .handles
                .values()
                .all(|ws| Arc::ptr_eq(&publication, &ws.publish_mu)));
        }
        // Sharing also updates cached handles, not just the currently selected workspace.
        let other = ToolState::new(&config).unwrap();
        parent.share_publication_guard(&other);
        assert!(Arc::ptr_eq(
            &other.workspace.publish_mu,
            &parent.workspace.publish_mu
        ));
        assert!(parent
            .handles
            .values()
            .all(|ws| Arc::ptr_eq(&other.workspace.publish_mu, &ws.publish_mu)));
        assert_eq!(fs::read(launch.join("file")).unwrap(), b"base\n");
    }

    fn publication_args(name: &str, writer: usize) -> Value {
        match name {
            "write_file" => json!({"path":"file","content":format!("{writer}-write\n")}),
            "edit_file" => {
                json!({"path":"file","old_text":"base","new_text":format!("{writer}-edit")})
            }
            "delete_file" => json!({"path":"file"}),
            _ => unreachable!(),
        }
    }

    #[test]
    fn competing_file_publications_have_one_winner_and_no_lost_update() {
        use std::sync::{mpsc, Arc, Barrier};
        use std::time::Duration;
        for explicit in [false, true] {
            for first in ["write_file", "edit_file", "delete_file"] {
                for second in ["write_file", "edit_file", "delete_file"] {
                    let dir = tempfile::tempdir().unwrap();
                    fs::write(dir.path().join("file"), "base\n").unwrap();
                    let config = json!({"workspace":dir.path(),"agents":true,"session_id":crate::types::new_id()});
                    let mut parent = ToolState::new(&config).unwrap();
                    let mut child = ToolState::new(&config).unwrap();
                    child.share_publication_guard(&parent);
                    for state in [&mut parent, &mut child] {
                        assert_eq!(
                            execute(state, &config, "read_file", r#"{"path":"file"}"#).unwrap()
                                ["ok"],
                            true
                        );
                    }
                    let publication = parent.workspace.publish_mu.clone();
                    let held = publication.lock().unwrap();
                    let start = Arc::new(Barrier::new(3));
                    let (sent, received) = mpsc::channel();
                    let (blocked, results) = std::thread::scope(|scope| {
                        for (i, (mut state, name)) in
                            [(parent, first), (child, second)].into_iter().enumerate()
                        {
                            let start = start.clone();
                            let sent = sent.clone();
                            let config = &config;
                            scope.spawn(move || {
                                let mut args = publication_args(name, i);
                                if explicit {
                                    args["expected_sha256"] =
                                        json!(crate::workspace::digest(b"base\n"));
                                }
                                start.wait();
                                sent.send((
                                    i,
                                    execute(&mut state, config, name, &args.to_string()).unwrap(),
                                ))
                                .unwrap();
                            });
                        }
                        start.wait();
                        let early = received.recv_timeout(Duration::from_millis(50));
                        let blocked = early.is_err();
                        drop(held);
                        let mut results = Vec::new();
                        if let Ok(result) = early {
                            results.push(result);
                        }
                        while results.len() < 2 {
                            results.push(received.recv_timeout(Duration::from_secs(5)).unwrap());
                        }
                        (blocked, results)
                    });
                    assert!(blocked, "file tools bypassed the publication guard");
                    let winners: Vec<_> = results
                        .iter()
                        .filter(|(_, out)| out["ok"] == true)
                        .collect();
                    assert_eq!(
                        winners.len(),
                        1,
                        "{first}/{second}, explicit={explicit}: {results:?}"
                    );
                    let (winner, outcome) = winners[0];
                    assert_eq!(outcome["effect"], "applied");
                    assert_eq!(
                        outcome["data"]["before_sha256"],
                        crate::workspace::digest(b"base\n")
                    );
                    let name = [first, second][*winner];
                    let loser = results.iter().find(|(_, out)| out["ok"] == false).unwrap();
                    assert_eq!(loser.1["effect"], "none");
                    let expected_code = if name == "delete_file"
                        && (explicit || [first, second][loser.0] != "write_file")
                    {
                        "not_found"
                    } else {
                        "stale_file"
                    };
                    assert_eq!(loser.1["code"], expected_code, "{results:?}");
                    if name == "delete_file" {
                        assert!(!dir.path().join("file").exists());
                    } else {
                        let expected = format!(
                            "{winner}-{}\n",
                            if name == "write_file" {
                                "write"
                            } else {
                                "edit"
                            }
                        );
                        let actual = fs::read(dir.path().join("file")).unwrap();
                        assert_eq!(actual, expected.as_bytes());
                        assert_eq!(
                            outcome["data"]["after_sha256"],
                            crate::workspace::digest(&actual)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn cancelled_file_publications_wait_for_guard_then_leave_files_unchanged() {
        use std::sync::{mpsc, Arc, Barrier};
        use std::time::Duration;
        for name in ["write_file", "edit_file", "delete_file"] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("file"), "base\n").unwrap();
            let config =
                json!({"workspace":dir.path(),"agents":true,"session_id":crate::types::new_id()});
            let parent = ToolState::new(&config).unwrap();
            let mut child = ToolState::new(&config).unwrap();
            child.share_publication_guard(&parent);
            assert_eq!(
                execute(&mut child, &config, "read_file", r#"{"path":"file"}"#).unwrap()["ok"],
                true
            );
            let held = parent.workspace.publish_mu.lock().unwrap();
            let start = Arc::new(Barrier::new(2));
            let (sent, received) = mpsc::channel();
            let (blocked, result) = std::thread::scope(|scope| {
                let child_start = start.clone();
                let config = &config;
                scope.spawn(move || {
                    child_start.wait();
                    sent.send(
                        execute(
                            &mut child,
                            config,
                            name,
                            &publication_args(name, 0).to_string(),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                });
                start.wait();
                let early = received.recv_timeout(Duration::from_millis(50));
                let blocked = early.is_err();
                crate::cancellation::cancel(config);
                drop(held);
                (
                    blocked,
                    early
                        .unwrap_or_else(|_| received.recv_timeout(Duration::from_secs(5)).unwrap()),
                )
            });
            assert!(blocked, "{name} bypassed the publication guard");
            let message = if name == "delete_file" {
                "cancelled before deletion"
            } else {
                "cancelled before file publication"
            };
            assert_eq!(result, fail("not_executed", message));
            assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"base\n");
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

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
