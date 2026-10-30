use serde_json::Value;
use std::sync::LazyLock;

// Preserve the literal bytes from internal/reagent/tool_*.go: cache keys hash
// InputSchema before JSON serialization removes its insignificant whitespace.
const RAW_SCHEMAS: &[(&str, &str)] = &[
    ("echo", include_str!("schemas/echo.json")),
    ("read_file", include_str!("schemas/read_file.json")),
    ("list_files", include_str!("schemas/list_files.json")),
    ("search_text", include_str!("schemas/search_text.json")),
    ("write_file", include_str!("schemas/write_file.json")),
    ("edit_file", include_str!("schemas/edit_file.json")),
    ("delete_file", include_str!("schemas/delete_file.json")),
    ("exec", include_str!("schemas/exec.json")),
    ("git_inspect", include_str!("schemas/git_inspect.json")),
    ("git_do", include_str!("schemas/git_do_recipe.json")),
    ("git_do", include_str!("schemas/git_do_jev.json")),
    ("recall", include_str!("schemas/recall.json")),
    (
        "report_friction",
        include_str!("schemas/report_friction.json"),
    ),
    (
        "request_workspace_access",
        include_str!("schemas/workspace_path.json"),
    ),
    (
        "switch_workspace",
        include_str!("schemas/workspace_path.json"),
    ),
    ("spawn", include_str!("schemas/spawn.json")),
    ("send", include_str!("schemas/send.json")),
    ("wait", include_str!("schemas/wait.json")),
    ("threads", include_str!("schemas/threads.json")),
    ("cancel", include_str!("schemas/agent_target.json")),
    ("dismiss", include_str!("schemas/agent_target.json")),
    ("reset", include_str!("schemas/agent_target.json")),
];

static PARSED_SCHEMAS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    RAW_SCHEMAS
        .iter()
        .map(|(_, raw)| serde_json::from_str(raw).expect("embedded tool schema is valid JSON"))
        .collect()
});

/// Returns Go's original schema bytes only when both name and parsed schema match.
pub fn raw_schema(name: &str, schema: &Value) -> Option<String> {
    RAW_SCHEMAS
        .iter()
        .zip(PARSED_SCHEMAS.iter())
        .find(|((candidate, _), parsed)| *candidate == name && *parsed == schema)
        .map(|((_, raw), _)| (*raw).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recovers_literal_bytes_from_compact_and_reordered_values() {
        for (name, raw) in RAW_SCHEMAS {
            let mut schema: Value = serde_json::from_str(raw).unwrap();
            let object = schema.as_object_mut().unwrap();
            let entries: Vec<_> = object.clone().into_iter().collect();
            object.clear();
            object.extend(entries.into_iter().rev());
            assert_eq!(raw_schema(name, &schema).as_deref(), Some(*raw), "{name}");
        }
        let raw = include_str!("schemas/exec.json");
        assert!(raw.starts_with("{\n  \"type\": \"object\","));
        assert!(raw.ends_with("\n}"));
    }

    #[test]
    fn unknown_names_and_changed_schemas_do_not_borrow_known_bytes() {
        let mut schema: Value = serde_json::from_str(include_str!("schemas/echo.json")).unwrap();
        assert!(raw_schema("custom_echo", &schema).is_none());
        assert!(raw_schema("read_file", &schema).is_none());
        schema["properties"]["text"]["description"] = json!("Changed schema");
        assert!(raw_schema("echo", &schema).is_none());
        assert!(raw_schema("echo", &Value::Null).is_none());
    }

    #[test]
    fn existing_specs_have_matching_original_schemas() {
        let specs = crate::tools::specs(&json!({"agents": true, "report_friction": true}));
        for spec in specs.as_array().unwrap() {
            let name = spec["name"].as_str().unwrap();
            assert!(raw_schema(name, &spec["input_schema"]).is_some(), "{name}");
        }
        for mode in ["recipe", "jev"] {
            let spec = crate::tool_git::git_do_spec(&json!({"git_do": mode}));
            let raw = raw_schema("git_do", &spec["input_schema"]).unwrap();
            let asset = if mode == "jev" {
                include_str!("schemas/git_do_jev.json")
            } else {
                include_str!("schemas/git_do_recipe.json")
            };
            assert_eq!(raw, asset);
        }
    }
}
