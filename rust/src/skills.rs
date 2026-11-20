use crate::workspace::Workspace;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use yaml_rust2::YamlLoader;

const MAX_SKILL_BYTES: u64 = 32 << 10;

/// Skill is one Agent Skills folder whose SKILL.md a user runs as /name or $name.
pub struct Skill {
    pub name: String,
    pub description: String,
    /// location names the skill folder: workspace-relative for project skills, absolute otherwise.
    pub location: String,
    pub body: String,
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
    let field = |key: &str| {
        fields
            .as_ref()
            .and_then(|f| f[key].as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string()
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
}
