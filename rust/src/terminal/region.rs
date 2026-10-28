use super::editor::layout;
use crate::render::{sanitize, styled, truncate, width};
use serde_json::Value;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const TEAL: &str = "\x1b[1;38;5;86m";

/// RegionHandle shares the active terminal pane between input and presentation.
pub type RegionHandle = Arc<Mutex<Region>>;

/// Status retains the session labels shown below input.
#[derive(Default)]
pub(super) struct Status {
    model: String,
    effort: String,
    plan: bool,
    blocked: bool,
    auto: bool,
    context: Option<f64>,
}

impl Status {
    /// Update reads the labels from the active session.
    pub(super) fn update(&mut self, config: &Value, state: &Value) {
        self.model = config["model"].as_str().unwrap_or("").into();
        self.effort = config["reasoning_effort"].as_str().unwrap_or("").into();
        self.plan = state["plan"].as_bool().unwrap_or(false);
        self.blocked = !state["blocked"].as_str().unwrap_or("").is_empty();
        self.auto = state["auto_enabled"].as_bool().unwrap_or(false);
        let window = crate::catalog::effective_context_window(config);
        if config["context_window_tokens"].as_i64().unwrap_or(0) > 0 {
            let window = crate::render::window_label(window);
            self.effort
                .push_str(&format!(" · window {window} override"));
        }
        let auth = match config["auth_mode"].as_str().unwrap_or("") {
            "key" => "API key",
            "proxy" => "API proxy",
            "chatgpt" => "ChatGPT plan",
            _ => "",
        };
        if !auth.is_empty() {
            if self.effort.is_empty() {
                self.model.push_str(&format!(" · {auth}"));
            } else {
                self.effort.push_str(&format!(" · {auth}"));
            }
        }
        self.context = if state["last_request"]["known"].as_bool().unwrap_or(false) && window > 0 {
            Some(
                100. * state["last_request"]["input_tokens"].as_i64().unwrap_or(0) as f64
                    / window as f64,
            )
        } else {
            None
        };
    }

    fn prompt(&self, color: bool) -> String {
        let prompt = if self.plan && color {
            format!("{TEAL}❯{RESET} ")
        } else {
            "❯ ".into()
        };
        if self.blocked {
            format!("(blocked) {prompt}")
        } else {
            prompt
        }
    }

    fn row(&self, columns: usize, color: bool) -> String {
        let label = |s: &str| sanitize(s).replace('\n', "↵").replace('\t', "⇥");
        let mut parts = Vec::new();
        if self.plan {
            parts.push("plan mode".to_string());
        }
        if !self.model.is_empty() {
            let mut model = label(&self.model);
            if !self.effort.is_empty() {
                model.push_str(&format!(" / {}", label(&self.effort)));
            }
            parts.push(model);
        }
        if self.auto {
            parts.push("auto".into());
        }
        if let Some(context) = self.context {
            parts.push(format!("{context:.0}% of context"));
        }
        let mut row = String::from("  ");
        for part in &parts {
            let join = if row == "  " { "" } else { " · " };
            if width(&format!("{row}{join}{part}")) > columns {
                break;
            }
            row.push_str(join);
            if part == "plan mode" && color {
                row.push_str(&format!("{TEAL}{part}{RESET}"));
            } else {
                row.push_str(part);
            }
        }
        if row == "  " {
            if let Some(first) = parts.first() {
                row.push_str(&truncate(first, columns.saturating_sub(2).max(1)));
            }
        }
        row
    }
}

/// Region owns a framed input pane and inserts output above its retained caret.
pub struct Region {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) active: bool,
    pub(super) initial_row: usize,
    pub(super) status: Status,
    pub(super) queued: bool,
    progress: Option<Progress>,
    buffer: Vec<char>,
    caret: usize,
    prefix: String,
    top: usize,
    caret_row: usize,
    caret_col: usize,
    visible: Vec<String>,
    pending: String,
    color: bool,
}

struct Progress {
    label: String,
    started: Instant,
    ticked: Instant,
    frame: usize,
}

impl Progress {
    fn text(&self, columns: usize, color: bool) -> String {
        let frames: Vec<_> = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".chars().collect();
        let mut text = format!("{} {}", frames[self.frame % frames.len()], self.label);
        let elapsed = self.started.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let elapsed = if elapsed.as_secs() < 60 {
                format!("{:.1}s", elapsed.as_secs_f64())
            } else {
                format!("{}m{:02}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
            };
            text.push_str(&format!(" · {elapsed}"));
        }
        text = truncate(&text, columns.saturating_sub(1));
        if color {
            format!("{DIM}{text}{RESET}")
        } else {
            text
        }
    }
}

fn emit(text: &str) -> io::Result<()> {
    let mut out = io::stderr().lock();
    out.write_all(text.as_bytes())?;
    out.flush()
}

impl Region {
    /// New creates an inactive pane for the controlling terminal.
    pub(super) fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            active: false,
            initial_row: 0,
            status: Status::default(),
            queued: false,
            progress: None,
            buffer: Vec::new(),
            caret: 0,
            prefix: String::new(),
            top: 0,
            caret_row: 0,
            caret_col: 0,
            visible: Vec::new(),
            pending: String::new(),
            color: styled(2),
        }
    }

    /// Prompt reflects plan and blocked state without changing editor input.
    pub(super) fn prompt(&self) -> String {
        self.status.prompt(self.color)
    }

    fn rule(&self) -> String {
        let rule = "─".repeat(self.width);
        if self.color {
            format!("{DIM}{rule}{RESET}")
        } else {
            rule
        }
    }

    /// Input paints the current editable submission and restores its caret.
    pub(super) fn input(
        &mut self,
        buffer: &[char],
        caret: usize,
        prefix: &str,
        columns: usize,
        height: usize,
    ) -> io::Result<()> {
        self.buffer = buffer.to_vec();
        self.caret = caret;
        self.prefix = prefix.to_string();
        self.width = columns;
        let prompt = self.prompt();
        let (mut rows, mut row, col) = if prefix.is_empty() {
            layout(buffer, caret, &prompt, columns)
        } else {
            let mut rows = Vec::new();
            for (index, line) in prefix
                .strip_suffix('\n')
                .unwrap_or(prefix)
                .split('\n')
                .enumerate()
            {
                let line: Vec<_> = line.chars().collect();
                let (part, _, _) = layout(
                    &line,
                    line.len(),
                    if index == 0 { &prompt } else { "  " },
                    columns,
                );
                rows.extend(part);
            }
            let (part, row, col) = layout(buffer, caret, "  ", columns);
            let row = row + rows.len();
            rows.extend(part);
            (rows, row, col)
        };
        if columns >= 20 {
            rows.insert(0, self.rule());
            rows.push(self.rule());
            rows.push(self.status.row(columns, self.color));
            if let Some(progress) = &self.progress {
                let text = progress.text(columns.saturating_sub(2), self.color);
                rows.push(format!("  {}", truncate(&text, columns - 2)));
            }
            if self.queued {
                rows.push("  queued".into());
            }
            row += 1;
        }
        self.draw(rows, row, col, columns, height)
    }

    fn draw(
        &mut self,
        mut rows: Vec<String>,
        mut row: usize,
        col: usize,
        columns: usize,
        height: usize,
    ) -> io::Result<()> {
        if columns < 2 || height < 2 {
            return Ok(());
        }
        self.width = columns;
        self.height = height;
        if rows.len() >= height {
            let cut = rows.len() - height + 1;
            rows.drain(..cut);
            row = row.saturating_sub(cut);
        }
        let mut out = String::new();
        let mut top = self.top;
        if !self.active {
            if self.initial_row > 0 && self.initial_row + rows.len() - 1 <= height {
                top = self.initial_row;
            } else {
                top = height - rows.len() + 1;
                out.push_str(&"\r\n".repeat(rows.len() - 1));
            }
        } else if top + rows.len() - 1 > height {
            let overflow = top + rows.len() - 1 - height;
            out.push_str(&format!(
                "\x1b[?2026h\x1b[{height};1H{}",
                "\r\n".repeat(overflow)
            ));
            top -= overflow;
        }
        out.push_str(&format!(
            "\x1b[?2026h\x1b[{top};1H\x1b[J{}",
            rows.join("\r\n")
        ));
        self.top = top;
        self.caret_row = row.min(rows.len() - 1);
        self.caret_col = col;
        self.visible = rows;
        self.active = true;
        out.push_str(&format!(
            "\x1b[{};{}H\x1b[?2026l",
            top + self.caret_row,
            col + 1
        ));
        emit(&out)
    }

    /// Set progress replaces the current live operation, or removes it on completion.
    pub(crate) fn set_progress(&mut self, label: Option<&str>) -> io::Result<()> {
        self.progress = label.map(|label| Progress {
            label: label.into(),
            started: Instant::now(),
            ticked: Instant::now(),
            frame: 0,
        });
        self.repaint()
    }

    /// Refresh status updates session labels without changing the editable draft.
    pub(crate) fn refresh_status(&mut self, config: &Value, state: &Value) -> io::Result<()> {
        self.status.update(config, state);
        self.repaint()
    }

    fn repaint(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.input(
            &self.buffer.clone(),
            self.caret,
            &self.prefix.clone(),
            self.width,
            self.height,
        )
    }

    /// Tick progress advances only at the display's hundred-millisecond cadence.
    pub(super) fn tick_progress(&mut self) -> bool {
        let Some(progress) = &mut self.progress else {
            return false;
        };
        if progress.ticked.elapsed() < Duration::from_millis(100) {
            return false;
        }
        progress.ticked = Instant::now();
        progress.frame += 1;
        true
    }

    /// Submit commits the user frame above the next input pane.
    pub(super) fn submit(&mut self, message: &str) -> io::Result<()> {
        if message.is_empty() || !self.active {
            return Ok(());
        }
        let mut rows = Vec::new();
        if self.width >= 20 {
            rows.push(self.rule());
        }
        let safe = sanitize(message).replace('\t', "⇥");
        for (index, line) in safe.split('\n').enumerate() {
            rows.extend(crate::render::wrap(
                if index == 0 { "❯ " } else { "  " },
                "  ",
                line,
                self.width.saturating_sub(1),
            ));
        }
        if self.width >= 20 {
            rows.push(self.rule());
        }
        self.draw(rows, 0, 0, self.width, self.height)?;
        emit(&format!(
            "\x1b[{};1H\r\n",
            self.top + self.visible.len() - 1
        ))?;
        self.active = false;
        Ok(())
    }

    /// Collapse removes the pane before exiting or handing the terminal to another program.
    pub(crate) fn collapse(&mut self) -> io::Result<()> {
        if self.active {
            emit(&format!("\x1b[?2026h\x1b[{};1H\x1b[J\x1b[?2026l", self.top))?;
        }
        self.active = false;
        Ok(())
    }

    /// Clear erases terminal history on screen without changing the editor.
    pub(super) fn clear(&mut self) -> io::Result<()> {
        emit("\x1b[?2026h\x1b[H\x1b[2J\x1b[?2026l")?;
        self.top = 1;
        Ok(())
    }

    fn insert(&mut self, line: &str) -> io::Result<()> {
        if !self.active {
            return emit(&format!("{line}\n"));
        }
        let available = self
            .height
            .saturating_sub(self.top + self.visible.len() - 1);
        if available > 0 {
            let needed = width(line).div_ceil(self.width).max(1);
            let movement = available.min(needed);
            let old_top = self.top;
            emit(&format!("\x1b[?2026h\x1b[{old_top};1H\x1b[J"))?;
            self.top += movement;
            self.draw(
                self.visible.clone(),
                self.caret_row,
                self.caret_col,
                self.width,
                self.height,
            )?;
            if movement == needed {
                return emit(&format!(
                    "\x1b[?2026h\x1b[{old_top};1H{line}\x1b[{};{}H\x1b[?2026l",
                    self.top + self.caret_row,
                    self.caret_col + 1
                ));
            }
        }
        let above = self.top.saturating_sub(1).max(1);
        emit(&format!("\x1b[?2026h\x1b[1;{above}r\x1b[{above};1H\r\n\x1b[{above};1H{line}\x1b[r\x1b[{};{}H\x1b[?2026l", self.top + self.caret_row, self.caret_col + 1))
    }

    fn write_text(&mut self, text: &str) -> io::Result<()> {
        if !self.active {
            return emit(text);
        }
        self.pending.push_str(text);
        while let Some(end) = self.pending.find('\n') {
            let line: String = self.pending.drain(..end).collect();
            self.pending.drain(..1);
            self.insert(&line)?;
        }
        Ok(())
    }
}

fn same_terminal() -> bool {
    unsafe {
        if libc::isatty(1) != 1 || libc::isatty(2) != 1 {
            return false;
        }
        let mut stdout: libc::stat = std::mem::zeroed();
        let mut stderr: libc::stat = std::mem::zeroed();
        libc::fstat(1, &mut stdout) == 0
            && libc::fstat(2, &mut stderr) == 0
            && stdout.st_dev == stderr.st_dev
            && stdout.st_ino == stderr.st_ino
    }
}

/// Output inserts complete terminal lines above the pane while leaving redirected stdout literal.
pub(crate) fn output(region: Option<&RegionHandle>, fd: i32, text: &str) {
    let result = if let Some(region) = region.filter(|_| fd == 2 || same_terminal()) {
        region.lock().unwrap().write_text(text)
    } else if fd == 1 {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(text.as_bytes())
            .and_then(|_| stdout.flush())
    } else {
        emit(text)
    };
    let _ = result;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_reserves_caret_cell_and_uses_elapsed_only_after_one_second() {
        let now = Instant::now();
        let mut progress = Progress {
            label: "waiting for model · step 1".into(),
            started: now,
            ticked: now,
            frame: 0,
        };
        assert_eq!(progress.text(80, false), "⠋ waiting for model · step 1");
        assert_eq!(progress.text(10, false), "⠋ waitin…");
        progress.started = now - Duration::from_millis(1500);
        assert!(progress.text(80, true).ends_with(" · 1.5s\x1b[0m"));
        progress.started = now - Duration::from_secs(65);
        assert!(progress.text(80, false).ends_with(" · 1m05s"));
    }

    #[test]
    fn status_preserves_auth_and_window_labels_before_width_omission() {
        let mut status = Status::default();
        status.update(
            &serde_json::json!({"model":"gpt-6-luna", "reasoning_effort":"low", "auth_mode":"key"}),
            &Value::Null,
        );
        assert_eq!(status.row(80, false), "  gpt-6-luna / low · API key");
        assert_eq!(status.row(18, false), "  gpt-6-luna / lo…");
        status.update(
            &serde_json::json!({"model":"gpt-6-luna", "auth_mode":"proxy"}),
            &Value::Null,
        );
        assert_eq!(status.row(80, false), "  gpt-6-luna · API proxy");
        status.update(
            &serde_json::json!({"model":"gpt-6-luna", "reasoning_effort":"low", "context_window_tokens":1_000_000}),
            &Value::Null,
        );
        assert_eq!(
            status.row(80, false),
            "  gpt-6-luna / low · window 1M override"
        );
    }

    #[test]
    fn status_omits_rightmost_labels_and_keeps_controls_on_one_row() {
        let mut status = Status::default();
        status.update(
            &serde_json::json!({"model":"model\nname", "reasoning_effort":"high\t", "context_window_tokens":100}),
            &serde_json::json!({"plan":true,"auto_enabled":true,"last_request":{"known":true,"input_tokens":25}}),
        );
        assert_eq!(
            status.row(80, false),
            "  plan mode · model↵name / high⇥ · window 100 override · auto · 25% of context"
        );
        assert_eq!(status.row(12, false), "  plan mode");
        assert_eq!(status.row(5, false), "  pl…");
        status.blocked = true;
        assert_eq!(status.prompt(false), "(blocked) ❯ ");
        assert_eq!(status.prompt(true), format!("(blocked) {TEAL}❯{RESET} "));
    }
}
