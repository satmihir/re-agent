use crate::tools::{fail, outcome};
use crate::workspace::Workspace;
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use yaml_rust2::{Yaml, YamlLoader};

const MAX_SKILL_BYTES: u64 = 32 << 10;
// Codex budgets its skill list at 8,000 characters when the context window is unknown.
const MAX_INDEX_BYTES: usize = 8000;
const INDEX_HEADING: &str = "\n# Skills\n\nThe user installed these skills: instructions for particular kinds of task. When a request matches a skill's description, call load_skill with its name before you start, then follow what it returns. A message that begins with \"re:agent skill NAME\" already carries that skill's instructions.\n\n";

/// Skill is one Agent Skills folder whose SKILL.md a user runs as /name or $name.
pub struct Skill {
    pub name: String,
    pub description: String,
    /// location names the skill folder: workspace-relative for project skills, absolute otherwise.
    pub location: String,
    pub body: String,
    /// model_invocable is false when frontmatter sets disable-model-invocation, as in Claude Code.
    pub model_invocable: bool,
}

/// home is the directory holding the user's ~/.agents and ~/.claude skill folders.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// discover reads skills from ~/.agents, ~/.claude, then the workspace's .agents and .claude; the first of a name wins.
pub fn discover(workspace: &Path, home: Option<&Path>) -> (Vec<Skill>, Vec<String>) {
    let mut roots: Vec<(PathBuf, bool)> = Vec::new();
    if let Some(home) = home {
        roots.push((home.join(".agents/skills"), false));
        roots.push((home.join(".claude/skills"), false));
    }
    let ws = Workspace::open(&workspace.to_string_lossy()).ok();
    if let Some(ws) = &ws {
        roots.push((ws.root.join(".agents/skills"), true));
        roots.push((ws.root.join(".claude/skills"), true));
    }
    let mut seen_roots: Vec<PathBuf> = Vec::new();
    let mut skills: Vec<Skill> = Vec::new();
    let mut warnings = Vec::new();
    for (root, project) in roots {
        // Launching from the home directory makes the user and project roots the same folder.
        let Ok(canonical) = fs::canonicalize(&root) else {
            continue;
        };
        if seen_roots.contains(&canonical) {
            continue;
        }
        seen_roots.push(canonical);
        let mut folders: Vec<String> = match fs::read_dir(&root) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| !name.starts_with('.'))
                .collect(),
            Err(e) => {
                warnings.push(format!(
                    "{}: {}",
                    root.display(),
                    crate::errors::stable_io(&e)
                ));
                continue;
            }
        };
        folders.sort();
        for folder in folders {
            let dir = root.join(&folder);
            let project_ws = ws.as_ref().filter(|_| project);
            let location = match project_ws {
                Some(ws) => ws.relative(&dir),
                None => dir.to_string_lossy().into_owned(),
            };
            let skill = match read(&dir, &folder, project_ws) {
                Ok(Some((skill, warning))) => {
                    if let Some(warning) = warning {
                        warnings.push(format!("{location}: {warning}"));
                    }
                    Skill { location, ..skill }
                }
                Ok(None) => continue,
                Err(reason) => {
                    warnings.push(format!("{location}: skipped: {reason}"));
                    continue;
                }
            };
            if let Some(first) = skills.iter().find(|s| s.name == skill.name) {
                warnings.push(format!(
                    "{}: skipped: {} already names {}",
                    skill.location, skill.name, first.location
                ));
                continue;
            }
            skills.push(skill);
        }
    }
    (skills, warnings)
}

/// read loads one skill folder; a project skill must resolve inside the workspace like AGENTS.md.
fn read(
    dir: &Path,
    folder: &str,
    workspace: Option<&Workspace>,
) -> Result<Option<(Skill, Option<String>)>, String> {
    let path = dir.join("SKILL.md");
    if let Some(ws) = workspace {
        ws.resolve(&ws.relative(&path))
            .map_err(|_| "SKILL.md resolves outside the workspace or is withheld".to_string())?;
    }
    let info = match fs::metadata(&path) {
        Ok(info) => info,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot read SKILL.md".into()),
    };
    if !info.is_file() {
        return Err("SKILL.md is not a regular file".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .and_then(|f| f.take(MAX_SKILL_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|_| "cannot read SKILL.md".to_string())?;
    if bytes.len() as u64 > MAX_SKILL_BYTES {
        return Err("SKILL.md is larger than 32 KiB".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "SKILL.md is not UTF-8".to_string())?;
    let (frontmatter, body) = split_frontmatter(&text).unwrap_or(("", &text));
    // Like Claude Code, a skill with unparseable frontmatter still loads, without its fields.
    let (fields, warning) = match YamlLoader::load_from_str(frontmatter) {
        Ok(docs) => (docs.into_iter().next(), None),
        Err(e) => (None, Some(format!("frontmatter ignored: {e}"))),
    };
    let value = |key: &str| fields.as_ref().map_or(&Yaml::BadValue, |f| &f[key]);
    let field = |key: &str| value(key).as_str().map(str::trim).unwrap_or("").to_string();
    // Claude Code also accepts yes, on and 1 for booleans.
    let disabled = match value("disable-model-invocation") {
        Yaml::Boolean(b) => *b,
        Yaml::Integer(n) => *n == 1,
        Yaml::String(s) => matches!(s.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1"),
        _ => false,
    };
    let mut name = field("name");
    if name.is_empty() {
        name = folder.to_string();
    }
    if !valid_name(&name) {
        return Err(format!(
            "name {name:?} must be 1-64 ASCII letters, digits, '-' or '_'"
        ));
    }
    let body = body.trim().to_string();
    let mut description = field("description");
    if description.is_empty() {
        description = body
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_string();
    }
    let skill = Skill {
        name,
        description,
        location: String::new(),
        body,
        model_invocable: !disabled,
    };
    Ok(Some((skill, warning)))
}

/// Frontmatter counts only when `---` is the file's first line, as in Claude Code.
fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(name_char)
}

/// invocation is the user message that runs a skill; /name arguments fill $ARGUMENTS or follow the body.
pub fn invocation(skill: &Skill, arguments: Option<&str>) -> String {
    let mut body = skill.body.clone();
    if let Some(arguments) = arguments.map(str::trim) {
        if body.contains("$ARGUMENTS") {
            body = body.replace("$ARGUMENTS", arguments);
        } else if !arguments.is_empty() {
            body.push_str(&format!("\n\nARGUMENTS: {arguments}"));
        }
    }
    format!(
        "re:agent skill {}\nSkill folder: {} (relative paths in the skill start there)\n\n{body}\n",
        skill.name, skill.location
    )
}

/// prompt_section lists the skills the model may load, for pinning into a Session's instructions.
pub fn prompt_section(config: &Value) -> Option<String> {
    if config["tool_free"] == true || config["no_project_instructions"] == true {
        return None;
    }
    let workspace = config["workspace"].as_str().filter(|w| !w.is_empty())?;
    let (skills, _) = discover(Path::new(workspace), home().as_deref());
    index(&skills)
}

fn index(skills: &[Skill]) -> Option<String> {
    let listed: Vec<_> = skills.iter().filter(|s| s.model_invocable).collect();
    if listed.is_empty() {
        return None;
    }
    let mut text = INDEX_HEADING.to_string();
    let mut omitted = 0;
    for skill in listed {
        let description: String = skill
            .description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(1024)
            .collect();
        let line = format!("- {}: {description}\n", skill.name);
        if text.len() + line.len() > MAX_INDEX_BYTES {
            omitted += 1;
            continue;
        }
        text.push_str(&line);
    }
    if omitted > 0 {
        text.push_str(&format!(
            "({omitted} more skills are installed but do not fit in this list.)\n"
        ));
    }
    Some(text)
}

/// load runs the load_skill tool, rereading the named skill from the current workspace and home.
pub fn load(workspace: &Path, home: Option<&Path>, args: &Value) -> Value {
    let name = args["name"].as_str().unwrap_or("");
    let (skills, _) = discover(workspace, home);
    let Some(skill) = skills.iter().find(|s| s.name == name && s.model_invocable) else {
        return fail(
            "not_found",
            &format!(
                "no skill named {} can be loaded; use a name from the Skills list",
                crate::errors::quote(name)
            ),
        );
    };
    let out = outcome(
        true,
        "ok",
        "",
        json!({"name":skill.name,"folder":skill.location,"instructions":skill.body}),
        "none",
    );
    if crate::types::json_string(&out).len() > crate::workspace::MAX_RESULT_BYTES {
        return fail(
            "too_large",
            &format!("SKILL.md does not fit in one result; ask the user to run /{name}"),
        );
    }
    out
}

/// mentioned returns the skills named by $name words in text, once each, in order of mention.
pub fn mentioned<'a>(skills: &'a [Skill], text: &str) -> Vec<&'a Skill> {
    let mut found: Vec<&Skill> = Vec::new();
    for (at, _) in text.match_indices('$') {
        if text[..at]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace())
        {
            continue;
        }
        let name: String = text[at + 1..]
            .chars()
            .take_while(|c| name_char(*c))
            .collect();
        if let Some(skill) = skills.iter().find(|s| s.name == name) {
            if !found.iter().any(|f| f.name == name) {
                found.push(skill);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, relative: &str, text: &str) {
        let path = dir.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn names(skills: &[Skill]) -> Vec<(&str, &str)> {
        skills
            .iter()
            .map(|s| (s.name.as_str(), s.location.as_str()))
            .collect()
    }

    #[test]
    fn user_skills_win_over_project_skills_and_agents_wins_over_claude() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let skill = |name: &str| format!("---\ndescription: {name}\n---\nbody of {name}\n");
        write(
            home.path(),
            ".agents/skills/deploy/SKILL.md",
            &skill("user agents"),
        );
        write(
            home.path(),
            ".claude/skills/deploy/SKILL.md",
            &skill("user claude"),
        );
        write(
            home.path(),
            ".claude/skills/notes/SKILL.md",
            &skill("user notes"),
        );
        write(
            workspace.path(),
            ".agents/skills/notes/SKILL.md",
            &skill("project notes"),
        );
        write(
            workspace.path(),
            ".agents/skills/test/SKILL.md",
            &skill("project agents"),
        );
        write(
            workspace.path(),
            ".claude/skills/test/SKILL.md",
            &skill("project claude"),
        );
        write(
            workspace.path(),
            ".claude/skills/lint/SKILL.md",
            &skill("project lint"),
        );
        write(
            workspace.path(),
            ".claude/skills/empty/README.md",
            "not a skill",
        );
        let (skills, warnings) = discover(workspace.path(), Some(home.path()));
        let user = |path: &str| home.path().join(path).to_string_lossy().into_owned();
        let (deploy, notes) = (user(".agents/skills/deploy"), user(".claude/skills/notes"));
        assert_eq!(
            names(&skills),
            [
                ("deploy", deploy.as_str()),
                ("notes", notes.as_str()),
                ("test", ".agents/skills/test"),
                ("lint", ".claude/skills/lint"),
            ]
        );
        assert_eq!(skills[0].description, "user agents");
        assert_eq!(skills[2].description, "project agents");
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings.iter().all(|w| w.contains("already names")));
    }

    #[test]
    fn skill_files_parse_like_claude_code() {
        for (text, name, description, body, warned) in [
            (
                "---\nname: ship\ndescription: Ship it\n---\nDo it.\n",
                "ship",
                "Ship it",
                "Do it.",
                false,
            ),
            (
                "---\r\ndescription: CRLF\r\n---\r\nBody\r\n",
                "folder",
                "CRLF",
                "Body",
                false,
            ),
            (
                "No frontmatter here.\n",
                "folder",
                "No frontmatter here.",
                "No frontmatter here.",
                false,
            ),
            (
                "---\ndescription: [unclosed\n---\nStill loads\n",
                "folder",
                "Still loads",
                "Still loads",
                true,
            ),
            (
                "---\nname: ship\n",
                "folder",
                "---",
                "---\nname: ship",
                false,
            ),
        ] {
            let workspace = tempfile::tempdir().unwrap();
            write(workspace.path(), ".agents/skills/folder/SKILL.md", text);
            let (skills, warnings) = discover(workspace.path(), None);
            assert_eq!(skills.len(), 1, "{text:?} {warnings:?}");
            assert_eq!(skills[0].name, name, "{text:?}");
            assert_eq!(skills[0].description, description, "{text:?}");
            assert_eq!(skills[0].body, body, "{text:?}");
            assert_eq!(!warnings.is_empty(), warned, "{text:?} {warnings:?}");
        }
    }

    #[test]
    fn unusable_skills_are_skipped_with_a_warning() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "escape/SKILL.md", "outside the workspace");
        write(workspace.path(), ".agents/skills/bad name/SKILL.md", "body");
        write(
            workspace.path(),
            ".agents/skills/big/SKILL.md",
            &"x".repeat(33 << 10),
        );
        fs::create_dir_all(workspace.path().join(".agents/skills/dir/SKILL.md")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("escape"),
            workspace.path().join(".agents/skills/escape"),
        )
        .unwrap();
        let (skills, warnings) = discover(workspace.path(), None);
        assert!(skills.is_empty());
        let warnings = warnings.join("\n");
        for reason in [
            "must be 1-64",
            "larger than 32 KiB",
            "not a regular file",
            "outside the workspace",
        ] {
            assert!(warnings.contains(reason), "{reason}: {warnings}");
        }
        // A user's own skill folders may link anywhere.
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join(".agents/skills")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("escape"),
            home.path().join(".agents/skills/escape"),
        )
        .unwrap();
        let (skills, _) = discover(workspace.path(), Some(home.path()));
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "escape");
    }

    #[test]
    fn invocation_fills_arguments_or_appends_them() {
        let skill = |body: &str| Skill {
            name: "deploy".into(),
            description: String::new(),
            location: ".agents/skills/deploy".into(),
            body: body.into(),
            model_invocable: true,
        };
        for (body, arguments, expected) in [
            (
                "Deploy $ARGUMENTS now.",
                Some(" staging "),
                "Deploy staging now.",
            ),
            (
                "Deploy now.",
                Some("staging"),
                "Deploy now.\n\nARGUMENTS: staging",
            ),
            ("Deploy now.", Some(""), "Deploy now."),
            ("Deploy $ARGUMENTS now.", None, "Deploy $ARGUMENTS now."),
        ] {
            assert_eq!(
                invocation(&skill(body), arguments),
                format!("re:agent skill deploy\nSkill folder: .agents/skills/deploy (relative paths in the skill start there)\n\n{expected}\n")
            );
        }
    }

    #[test]
    fn mentions_match_whole_skill_names_after_whitespace() {
        let skill = |name: &str| Skill {
            name: name.into(),
            description: String::new(),
            location: String::new(),
            body: String::new(),
            model_invocable: true,
        };
        let skills = [skill("deploy"), skill("lint")];
        for (text, expected) in [
            ("$deploy to staging", vec!["deploy"]),
            ("run $lint then $deploy, then $lint", vec!["lint", "deploy"]),
            ("$deployment and a$lint", vec![]),
            ("echo $HOME costs $5", vec![]),
        ] {
            let found: Vec<_> = mentioned(&skills, text)
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            assert_eq!(found, expected, "{text:?}");
        }
    }

    #[test]
    fn model_list_omits_disabled_skills_and_fits_its_budget() {
        let workspace = tempfile::tempdir().unwrap();
        for (folder, setting) in [
            ("a-shown", ""),
            ("b-false", "disable-model-invocation: false\n"),
            ("c-true", "disable-model-invocation: true\n"),
            ("d-yes", "disable-model-invocation: yes\n"),
            ("e-one", "disable-model-invocation: 1\n"),
        ] {
            write(
                workspace.path(),
                &format!(".agents/skills/{folder}/SKILL.md"),
                &format!("---\ndescription: |\n  Use for\n  {folder}.\n{setting}---\nBody\n"),
            );
        }
        let config = json!({"workspace": workspace.path()});
        let section = prompt_section(&config).unwrap();
        assert!(section.starts_with(INDEX_HEADING));
        assert!(
            section.contains("- a-shown: Use for a-shown.\n"),
            "{section}"
        );
        assert!(
            section.contains("- b-false: Use for b-false.\n"),
            "{section}"
        );
        for hidden in ["c-true", "d-yes", "e-one"] {
            assert!(!section.contains(hidden), "{hidden}: {section}");
        }
        for key in ["no_project_instructions", "tool_free"] {
            let mut off = config.clone();
            off[key] = json!(true);
            assert_eq!(prompt_section(&off), None, "{key}");
        }
        let many: Vec<_> = (0..40)
            .map(|n| Skill {
                name: format!("skill-{n}"),
                description: "d".repeat(2000),
                location: String::new(),
                body: String::new(),
                model_invocable: true,
            })
            .collect();
        let section = index(&many).unwrap();
        assert!(section.len() <= MAX_INDEX_BYTES + 100, "{}", section.len());
        assert!(section.ends_with("(33 more skills are installed but do not fit in this list.)\n"));
    }

    #[test]
    fn load_skill_rereads_by_name_and_refuses_hidden_skills() {
        let workspace = tempfile::tempdir().unwrap();
        write(
            workspace.path(),
            ".agents/skills/manual/SKILL.md",
            "---\ndisable-model-invocation: true\n---\nOnly the user runs this\n",
        );
        write(
            workspace.path(),
            ".agents/skills/huge/SKILL.md",
            &"\"\n".repeat(12 << 10),
        );
        write(
            workspace.path(),
            ".claude/skills/deploy/SKILL.md",
            "---\ndescription: Deploy\n---\nEdited version\n",
        );
        let result = load(workspace.path(), None, &json!({"name":"deploy"}));
        assert_eq!(
            result["data"],
            json!({"name":"deploy","folder":".claude/skills/deploy","instructions":"Edited version"})
        );
        for (name, code) in [
            ("manual", "not_found"),
            ("missing", "not_found"),
            ("huge", "too_large"),
        ] {
            let result = load(workspace.path(), None, &json!({"name":name}));
            assert_eq!(result["code"], code, "{name}: {result}");
        }
    }
}
