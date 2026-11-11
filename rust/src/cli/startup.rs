use super::text;
use crate::terminal::{Choice, Input, Selection};
use serde_json::Value;

/// Implicit_chat recognizes terminal-default chat and explicit resume aliases.
pub(super) fn implicit_chat(args: &[String]) -> bool {
    let first = args.first().map(String::as_str);
    if matches!(first, Some("--help" | "-h" | "--version")) {
        return false;
    }
    // Explicit resume selectors also work in scripts; only bare startup needs a terminal.
    if first.is_some_and(|arg| {
        matches!(arg, "--continue" | "--resume")
            || arg.starts_with("--resume=")
            || arg.starts_with("--continue=")
    }) {
        return true;
    }
    interactive() && unsafe { libc::isatty(1) == 1 } && first.is_none_or(|arg| arg.starts_with('-'))
}

fn interactive() -> bool {
    unsafe { libc::isatty(0) == 1 && libc::isatty(2) == 1 }
}

/// Select_session resolves a workspace continuation or asks the human to choose a saved Session.
pub(super) fn select_session(cfg: &Value, continuing: bool) -> Result<Option<String>, String> {
    if !continuing && !interactive() {
        return Err(
            "--resume without an ID requires a terminal; use --resume ID or --continue".into(),
        );
    }
    let sessions = crate::checkpoint::list()?;
    if continuing {
        let workspace = crate::workspace::Workspace::open(text(cfg, "workspace"))?;
        return sessions
            .iter()
            .find(|session| {
                std::fs::canonicalize(&session.workspace).is_ok_and(|root| root == workspace.root)
            })
            .map(|session| Some(session.id.clone()))
            .ok_or_else(|| {
                format!(
                    "no saved session for {}; start with reagent chat",
                    workspace.root.display()
                )
            });
    }
    if sessions.is_empty() {
        return Err("no saved sessions; start with reagent chat".into());
    }
    let mut input = Input::new();
    input.restore().map_err(terminal_error)?;
    let result = pick(&mut input, &sessions);
    input.handoff().map_err(terminal_error)?;
    result
}

fn pick(
    input: &mut Input,
    sessions: &[crate::checkpoint::SavedSession],
) -> Result<Option<String>, String> {
    let mut page = 0;
    loop {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut size) } != 0 {
            return Err(terminal_error(std::io::Error::last_os_error()));
        }
        // Leave space for the picker frame and both page controls, even on short terminals.
        let per_page = (size.ws_row as usize).saturating_sub(7).clamp(1, 7);
        let pages = sessions.len().div_ceil(per_page);
        page = page.min(pages - 1);
        let start = page * per_page;
        let end = (start + per_page).min(sessions.len());
        let mut options: Vec<Choice> = sessions[start..end]
            .iter()
            .map(|session| {
                let time: chrono::DateTime<chrono::Local> = session.modified.into();
                let workspace_width = (size.ws_col as usize / 4).clamp(8, 30);
                Choice {
                    label: time.format("%Y-%m-%d %H:%M").to_string(),
                    detail: crate::render::truncate(
                        &crate::render::sanitize(&crate::render::short_path(&session.workspace)),
                        workspace_width,
                    ),
                    note: if session.first_message.is_empty() {
                        "(no message saved)".into()
                    } else {
                        session.first_message.clone()
                    },
                    disabled: false,
                }
            })
            .collect();
        if page > 0 {
            options.push(page_choice("Previous page"));
        }
        if page + 1 < pages {
            options.push(page_choice("Next page"));
        }
        let title = if pages == 1 {
            "Resume session".into()
        } else {
            format!("Resume session · {}/{pages}", page + 1)
        };
        match input.choose(&title, &options, 0).map_err(terminal_error)? {
            Selection::Chosen(index) if index < end - start => {
                return Ok(Some(sessions[start + index].id.clone()));
            }
            Selection::Chosen(index) if page > 0 && index == end - start => page -= 1,
            Selection::Chosen(_) => page += 1,
            Selection::Cancelled => return Ok(None),
            Selection::NotInteractive => {
                return Err(
                    "terminal too small for the session picker; use --resume ID or --continue"
                        .into(),
                )
            }
        }
    }
}

fn page_choice(label: &str) -> Choice {
    Choice {
        label: label.into(),
        detail: String::new(),
        note: String::new(),
        disabled: false,
    }
}

fn terminal_error(error: std::io::Error) -> String {
    format!("session picker: {}", crate::errors::stable_io(&error))
}
