use super::editor::layout;
use crate::render::{sanitize, styled, truncate, width};
use serde_json::Value;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::render::palette::{ACCENT, BOLD, DIM, RESET, SUBMITTED};

/// RegionHandle shares the active terminal pane between input and presentation.
pub type RegionHandle = Arc<Mutex<Region>>;

/// Status retains the session labels shown below input.
#[derive(Default)]
pub(super) struct Status {
    model: String,
    effort: String,
    plan: bool,
    fast: bool,
    blocked: bool,
    auto: bool,
    agents: usize,
    context: Option<f64>,
    request: Option<String>,
}

impl Status {
    /// Update reads the labels from the active session.
    pub(super) fn update(&mut self, config: &Value, state: &Value) {
        self.model = config["model"].as_str().unwrap_or("").into();
        self.effort = config["reasoning_effort"].as_str().unwrap_or("").into();
        self.plan = state["plan"].as_bool().unwrap_or(false);
        self.fast = config["fast"].as_bool().unwrap_or(false);
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
            format!("{BOLD}{ACCENT}❯{RESET} ")
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
        if self.fast {
            parts.push("fast".into());
        }
        if !self.model.is_empty() {
            let mut model = label(&self.model);
            if !self.effort.is_empty() {
                model.push_str(&format!(" / {}", label(&self.effort)));
            }
            parts.push(model);
        }
        if let Some(request) = &self.request {
            parts.push(request.clone());
        }
        if self.agents > 0 {
            parts.push(crate::render::plural(self.agents as i64, "agent", "agents"));
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
                row.push_str(&format!("{BOLD}{ACCENT}{part}{RESET}"));
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
    pub(super) command_selection: Option<usize>,
    progress: Option<Progress>,
    agents: Vec<crate::agents::AgentRow>,
    exec_lines: Vec<String>,
    stream_text: String,
    stream_rows: Vec<String>,
    stream_columns: usize,
    stream_color: bool,
    #[cfg(test)]
    stream_layouts: usize,
    buffer: Vec<char>,
    caret: usize,
    prefix: String,
    top: usize,
    caret_row: usize,
    caret_col: usize,
    visible: Vec<String>,
    pending: String,
    pub(crate) printed: bool,
    output_update: bool,
    pending_stdout: String,
    color: bool,
}

const THINKING_PHRASES: &[&str] = &[
    "Distilling thoughts…",
    "Attempting to turn tokens into gold…",
    "Checking for impurities…",
    "Waiting for the precipitate…",
    "Reaction proceeding at geological timescales…",
    "Consulting the periodic table of excuses…",
    "Applying heat to a questionable hypothesis…",
    "Separating signal from sediment…",
    "Searching for the philosopher’s stone…",
    "The experiment is under control. Probably.",
];
const WRITING_PHRASES: &[&str] = &[
    "Transmuting caffeine into questionable code…",
    "Pouring unstable compounds into production…",
    "Writing equations the universe hasn't approved…",
    "Adding another ingredient to the primordial soup…",
    "Documenting the explosion as an expected result…",
];

struct Progress {
    label: String,
    started: Instant,
    ticked: Instant,
    frame: usize,
    output_tokens: Option<usize>,
}

impl Progress {
    fn text(&self, columns: usize, color: bool) -> String {
        let frames = ['·', '∘', '°', '∘'];
        let elapsed = self.started.elapsed();
        let words = if self.label.starts_with("writing") {
            WRITING_PHRASES
        } else {
            THINKING_PHRASES
        };
        let label = if self.output_tokens.is_some() {
            words[(elapsed.as_secs() / 4) as usize % words.len()]
        } else {
            &self.label
        };
        let mut text = format!("{}⚗ {label}", frames[self.frame % frames.len()]);
        let elapsed = if elapsed.as_secs() < 60 {
            format!("{:.1}s", elapsed.as_secs_f64())
        } else {
            format!("{}m{:02}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
        };
        text.push_str(&format!(" · {elapsed}"));
        if let Some(tokens) = self.output_tokens {
            text.push_str(&format!(" · ~{tokens} output tokens"));
        }
        text = truncate(&text, columns.saturating_sub(1));
        if color {
            format!("{BOLD}{ACCENT}{text}{RESET}")
        } else {
            text
        }
    }
}

fn emit(text: &str) -> io::Result<()> {
    emit_line("", text.as_bytes(), "")
}

fn emit_line(prefix: &str, line: &[u8], suffix: &str) -> io::Result<()> {
    let mut out = io::stderr().lock();
    out.write_all(prefix.as_bytes())?;
    out.write_all(line)?;
    out.write_all(suffix.as_bytes())?;
    out.flush()
}

impl Region {
    /// New creates an inactive pane for the controlling terminal.
    pub(crate) fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            active: false,
            initial_row: 0,
            status: Status::default(),
            queued: false,
            command_selection: None,
            progress: None,
            agents: Vec::new(),
            exec_lines: Vec::new(),
            stream_text: String::new(),
            stream_rows: Vec::new(),
            stream_columns: 0,
            stream_color: false,
            #[cfg(test)]
            stream_layouts: 0,
            buffer: Vec::new(),
            caret: 0,
            prefix: String::new(),
            top: 0,
            caret_row: 0,
            caret_col: 0,
            visible: Vec::new(),
            pending: String::new(),
            printed: false,
            output_update: false,
            pending_stdout: String::new(),
            color: styled(2),
        }
    }

    /// Begin output keeps a reply and its summary in one visible terminal update.
    pub(crate) fn begin_output(&mut self) -> io::Result<()> {
        self.output_update = true;
        emit("\x1b[?2026h")
    }

    /// End output publishes the terminal frame before releasing any redirected reply.
    pub(crate) fn end_output(&mut self) -> io::Result<()> {
        self.output_update = false;
        emit("\x1b[?2026l")?;
        write_stdout(&std::mem::take(&mut self.pending_stdout))
    }

    fn update_end(&self) -> &str {
        if self.output_update {
            ""
        } else {
            "\x1b[?2026l"
        }
    }

    /// Prompt reflects plan and blocked state without changing editor input.
    pub(super) fn prompt(&self) -> String {
        self.status.prompt(self.color)
    }

    fn rule(&self, columns: usize) -> String {
        let rule = "─".repeat(columns);
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
        if columns < 2 || height < 2 {
            return Ok(());
        }
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
        if let Some(selected) = self.command_selection {
            let choices = super::command_choices(&buffer.iter().collect::<String>());
            let selected = selected.min(choices.len().saturating_sub(1));
            let limit = height.saturating_sub(rows.len() + 6).min(6);
            let low = selected
                .saturating_sub(limit / 2)
                .min(choices.len().saturating_sub(limit));
            rows.extend(
                choices
                    .iter()
                    .enumerate()
                    .skip(low)
                    .take(limit)
                    .map(|(index, choice)| {
                        super::choice_row(choice, index, selected, columns, self.color)
                    }),
            );
        }
        if columns >= 20 {
            rows.insert(0, self.rule(columns));
            rows.push(self.rule(columns));
            rows.push(self.status.row(columns, self.color));
            if self.stream_columns != columns || self.stream_color != self.color {
                #[cfg(test)]
                {
                    self.stream_layouts += 1;
                }
                self.stream_rows.clear();
                if !self.stream_text.is_empty() {
                    let preview = crate::render::display(
                        &self.stream_text,
                        self.color,
                        columns.saturating_sub(1),
                    );
                    for line in preview.split('\n') {
                        self.stream_rows.extend(crate::render::wrap(
                            "",
                            "",
                            line,
                            columns.saturating_sub(1),
                        ));
                    }
                }
                self.stream_columns = columns;
                self.stream_color = self.color;
            }
            // Copy only visible cached rows on draft, spinner, and height-only redraws.
            let limit = height.saturating_sub(rows.len() + 2);
            let agent_rows = self.agent_rows(
                columns,
                (height / 3).min(limit.saturating_sub(usize::from(self.progress.is_some()))),
            );
            let preview_limit =
                limit.saturating_sub(usize::from(self.progress.is_some()) + agent_rows.len());
            let start = self.stream_rows.len().saturating_sub(preview_limit);
            let mut above = self.stream_rows[start..].to_vec();
            if limit > 0 {
                if let Some(progress) = &self.progress {
                    above.push(progress.text(columns, self.color));
                }
            }
            let tail_limit = limit
                .saturating_sub(above.len() + agent_rows.len())
                .min(self.exec_lines.len());
            for line in &self.exec_lines[self.exec_lines.len() - tail_limit..] {
                let line = truncate(line, columns.saturating_sub(5));
                above.push(if self.color {
                    format!("    {DIM}{line}{RESET}")
                } else {
                    format!("    {line}")
                });
            }
            above.extend(agent_rows);
            row += above.len();
            above.extend(rows);
            rows = above;
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
        let resized = self.active && (self.width != columns || self.height != height);
        self.width = columns;
        self.height = height;
        if rows.len() >= height {
            let cut = rows.len() - height + 1;
            rows.drain(..cut);
            row = row.saturating_sub(cut);
        }
        let mut out = String::new();
        // Resize can shift or reflow the old pane; never scroll its transient rows into history.
        let mut top = if resized { 1 } else { self.top };
        if !self.active {
            if self.initial_row > 0
                && self
                    .initial_row
                    .checked_add(rows.len() - 1)
                    .is_some_and(|end| end <= height)
            {
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
            "\x1b[{};{}H{}",
            top + self.caret_row,
            col + 1,
            self.update_end()
        ));
        emit(&out)
    }

    /// Set progress replaces the current live operation, or removes it on completion.
    pub(crate) fn set_progress(&mut self, label: Option<&str>) -> io::Result<()> {
        if label.is_none() && self.progress.is_none() {
            return Ok(());
        }
        self.stream_text.clear();
        self.stream_rows.clear();
        self.stream_columns = 0;
        self.exec_lines.clear();
        self.status.request = label
            .filter(|label| label.starts_with("thinking"))
            .and_then(|label| {
                label
                    .split(" · ")
                    .find(|part| part.starts_with("step ") || part.starts_with("attempt "))
                    .map(str::to_owned)
            });
        self.progress = label.map(|label| Progress {
            label: label.into(),
            started: Instant::now(),
            ticked: Instant::now(),
            frame: 0,
            output_tokens: label.starts_with("thinking").then_some(0),
        });
        self.repaint()
    }

    /// Set exec lines retains a bounded tail for the display owner's next repaint.
    pub(crate) fn set_exec_lines(&mut self, lines: &[String]) {
        self.exec_lines = lines
            .iter()
            .rev()
            .take(5)
            .map(|line| sanitize(line).replace('\n', "↵").replace('\t', "⇥"))
            .collect();
        self.exec_lines.reverse();
    }

    /// Stream progress updates counters and optionally replaces the bounded complete-line preview.
    pub(crate) fn stream_progress(
        &mut self,
        writing: bool,
        tokens: usize,
        text: Option<&str>,
    ) -> io::Result<()> {
        if let Some(progress) = &mut self.progress {
            let suffix = progress
                .label
                .split_once(" · ")
                .map_or("", |(_, suffix)| suffix);
            progress.label = format!(
                "{} · {suffix}",
                if writing { "writing" } else { "thinking" }
            );
            progress.output_tokens = Some(tokens);
        }
        if let Some(text) = text {
            self.stream_text.clear();
            self.stream_text.push_str(text);
            self.stream_columns = 0;
        }
        self.repaint()
    }

    /// Clear stream removes speculative text after completion, failure, or cancellation.
    pub(crate) fn clear_stream(&mut self) -> io::Result<()> {
        if self.stream_text.is_empty() {
            return Ok(());
        }
        self.stream_text.clear();
        self.stream_rows.clear();
        self.stream_columns = 0;
        self.repaint()
    }

    fn agent_rows(&self, columns: usize, limit: usize) -> Vec<String> {
        let count = self.agents.len();
        if count == 0 || limit == 0 {
            return vec![];
        }
        let shown = if count > limit { limit - 1 } else { count };
        let mut rows = Vec::new();
        for agent in self.agents.iter().take(shown) {
            let label = sanitize(&agent.label).replace('\n', "↵").replace('\t', "⇥");
            let activity = sanitize(&agent.activity)
                .replace('\n', "↵")
                .replace('\t', "⇥");
            let elapsed = agent.started.elapsed().as_secs();
            let elapsed = if elapsed < 60 {
                format!("{elapsed}s")
            } else {
                format!("{}m{:02}s", elapsed / 60, elapsed % 60)
            };
            rows.push(format!("  {label} · {activity} · {elapsed}"));
        }
        if shown < count {
            rows.push(format!("  +{} more", count - shown));
        }
        rows.into_iter()
            .map(|row| {
                let row = truncate(&row, columns.saturating_sub(1));
                if self.color {
                    format!("{DIM}{row}{RESET}")
                } else {
                    row
                }
            })
            .collect()
    }

    /// Refresh agents replaces live rows and a wait label without restarting its clock.
    pub(crate) fn refresh_agents(
        &mut self,
        count: usize,
        wait_label: Option<&str>,
        rows: &[crate::agents::AgentRow],
    ) -> io::Result<()> {
        self.status.agents = count;
        self.agents = rows.to_vec();
        if let Some(label) = wait_label {
            if let Some(progress) = &mut self.progress {
                progress.label = label.to_owned();
            } else {
                return self.set_progress(Some(label));
            }
        }
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

    /// Submit leaves the user message above the next input pane without its frame.
    pub(super) fn submit(&mut self, message: &str) -> io::Result<()> {
        if message.is_empty() || !self.active {
            return Ok(());
        }
        let mut rows = Vec::new();
        let safe = sanitize(message).replace('\t', "⇥");
        for (index, line) in safe.split('\n').enumerate() {
            rows.extend(crate::render::wrap(
                if index == 0 { "❯ " } else { "  " },
                "  ",
                line,
                self.width.saturating_sub(1),
            ));
        }
        if self.color {
            for row in &mut rows {
                *row = format!("{SUBMITTED}{row}{RESET}");
            }
        }
        self.draw(rows, 0, 0, self.width, self.height)?;
        emit(&format!(
            "\x1b[{};1H\r\n",
            self.top + self.visible.len() - 1
        ))?;
        self.active = false;
        Ok(())
    }

    /// Picker replaces the input pane without releasing terminal ownership.
    pub(super) fn picker(
        &mut self,
        header: &str,
        options: Vec<String>,
        width: usize,
        height: usize,
    ) -> io::Result<()> {
        let mut rows = vec![header.to_owned()];
        rows.extend(options);
        if width >= 20 {
            rows.insert(0, self.rule(width));
            rows.push(self.rule(width));
            rows.push(self.status.row(width, self.color));
            if let Some(progress) = &self.progress {
                rows.insert(0, progress.text(width, self.color));
            }
            if self.queued {
                rows.push("  queued".into());
            }
        }
        self.draw(
            rows,
            1 + usize::from(self.progress.is_some() && width >= 20),
            0,
            width,
            height,
        )
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
        self.insert_bytes(line.as_bytes())
    }

    /// Insert bytes preserves raw shell output while retaining the input caret.
    pub(crate) fn insert_bytes(&mut self, line: &[u8]) -> io::Result<()> {
        self.printed = true;
        if !self.active {
            return emit_line("", line, "\n");
        }
        let available = self
            .height
            .saturating_sub(self.top + self.visible.len() - 1);
        if available > 0 {
            let needed = width(&String::from_utf8_lossy(line))
                .div_ceil(self.width)
                .max(1);
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
                return emit_line(
                    &format!("\x1b[?2026h\x1b[{old_top};1H"),
                    line,
                    &format!(
                        "\x1b[{};{}H{}",
                        self.top + self.caret_row,
                        self.caret_col + 1,
                        self.update_end()
                    ),
                );
            }
        }
        let above = self.top.saturating_sub(1).max(1);
        emit_line(
            &format!("\x1b[?2026h\x1b[1;{above}r\x1b[{above};1H\r\n\x1b[{above};1H"),
            line,
            &format!(
                "\x1b[r\x1b[{};{}H{}",
                self.top + self.caret_row,
                self.caret_col + 1,
                self.update_end()
            ),
        )
    }

    fn write_text(&mut self, text: &str) -> io::Result<()> {
        if !self.active {
            return emit(&text.replace("\r\n", "\n").replace('\n', "\r\n"));
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

fn write_stdout(text: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()
}

/// Output inserts complete terminal lines above the pane while leaving redirected stdout literal.
pub(crate) fn output(region: Option<&RegionHandle>, fd: i32, text: &str) {
    if let Some(region) = region {
        let shared = fd == 2 || same_terminal();
        let mut region = region.lock().unwrap();
        if shared {
            let _ = region.write_text(text);
            return;
        }
        if region.output_update {
            // A pipe reader can observe a reply before the PTY consumes its summary.
            region.pending_stdout.push_str(text);
            return;
        }
    }
    let _ = if fd == 1 {
        write_stdout(text)
    } else {
        emit(text)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unusable_resize_keeps_output_safe_and_the_next_submission_works() {
        let mut region = Region::new();
        region.initial_row = 1;
        region.input(&['a'], 1, "", 80, 24).unwrap();
        for (columns, height) in [(0, 24), (1, 24), (80, 0)] {
            region.input(&['b'], 1, "", columns, height).unwrap();
            assert_eq!((region.width, region.height), (80, 24));
            region.insert_bytes(b"output after resize").unwrap();
        }
        region.input(&['c'], 1, "", 60, 24).unwrap();
        region.submit("next submission").unwrap();
        assert!(!region.active);
    }

    #[test]
    fn resize_reanchors_live_rows_before_repainting_and_the_next_submission_works() {
        for color in [false, true] {
            let mut region = Region::new();
            region.color = color;
            let draft: Vec<_> = "next question".chars().collect();
            region.input(&draft, 3, "", 80, 24).unwrap();
            let agents = [crate::agents::AgentRow {
                label: "t1 research".into(),
                activity: "thinking".into(),
                started: Instant::now(),
            }];
            region
                .refresh_agents(1, Some("waiting on t1 research (step 1)"), &agents)
                .unwrap();
            assert!(region.top > 1);
            for (columns, height) in [(64, 12), (80, 24), (60, 24), (60, 30)] {
                let printed = region.printed;
                region.input(&draft, 3, "", columns, height).unwrap();
                assert_eq!(
                    region.top, 1,
                    "resize must erase the old pane, not scroll it"
                );
                assert!(region.visible[1].contains("t1 research · thinking ·"));
                assert_eq!(region.buffer, draft);
                assert_eq!(region.caret, 3);
                assert_eq!(region.printed, printed);
                region.insert_bytes(b"ordinary output").unwrap();
                assert!(region.top > 1);
            }
            region
                .picker("Pick one", vec!["choice".into()], 50, 20)
                .unwrap();
            assert_eq!(region.top, 1);
            region.input(&draft, 3, "", 50, 20).unwrap();
            region.refresh_agents(0, None, &[]).unwrap();
            region.set_progress(None).unwrap();
            assert!(region
                .visible
                .iter()
                .all(|row| !row.contains("t1 research")));
            region.insert_bytes(b"completed reply").unwrap();
            region.submit("next submission").unwrap();
            assert!(!region.active);
            assert!(region.visible[0].contains("next submission"));
        }
    }

    #[test]
    fn oversized_cursor_report_falls_back_to_bottom_placement() {
        let mut region = Region::new();
        region.initial_row = usize::MAX;
        region.input(&[], 0, "", 80, 24).unwrap();
        assert!(region.top <= 24);
        region.insert_bytes(b"ordinary output").unwrap();
        region.submit("ordinary submission").unwrap();
    }

    #[test]
    fn streamed_code_and_links_keep_their_context_until_the_fence_closes() {
        let mut region = Region::new();
        region.color = true;
        let draft: Vec<_> = "next question".chars().collect();
        region.input(&draft, 3, "", 48, 24).unwrap();
        for text in ["```diff\n", "```diff\n-old\n", "```diff\n-old\n+new\n"] {
            region.stream_progress(true, 10, Some(text)).unwrap();
            assert!(region.stream_rows[0].contains("│ diff"));
            assert!(!region.stream_rows.iter().any(|row| row.contains("```")));
            assert_eq!(region.buffer, draft);
            assert_eq!(region.caret, 3);
        }
        assert!(region.stream_rows[1].contains(crate::render::palette::ERROR));
        assert!(region.stream_rows[2].contains(crate::render::palette::SUCCESS));
        region
            .stream_progress(
                true,
                20,
                Some("```diff\n-old\n+new\n```\n[Docs](https://example.invalid/docs)"),
            )
            .unwrap();
        assert!(region
            .stream_rows
            .last()
            .unwrap()
            .contains("\x1b]8;;https://example.invalid/docs"));
        region.input(&draft, 3, "", 30, 24).unwrap();
        assert!(region
            .stream_rows
            .iter()
            .all(|row| crate::render::width(row) <= 29));
        region.clear_stream().unwrap();
        assert!(region.stream_rows.is_empty());
        assert_eq!(region.buffer, draft);
    }

    #[test]
    fn preview_layout_is_reused_for_drafts_ticks_and_height_only_resizes() {
        let mut region = Region::new();
        region.color = true;
        let draft: Vec<_> = "next question".chars().collect();
        region.input(&draft, 3, "", 80, 24).unwrap();
        region.set_progress(Some("thinking · model")).unwrap();
        let text = format!(
            "```\n{}\n```\n# outside",
            "**literal** code line\n".repeat(128)
        );
        region.stream_progress(true, 1000, Some(&text)).unwrap();
        let layouts = region.stream_layouts;
        assert!(region.stream_rows[1].contains(&format!("{ACCENT}**literal** code line")));
        assert!(region
            .stream_rows
            .last()
            .unwrap()
            .contains(&format!("{BOLD}{ACCENT}outside")));
        assert!(region.visible.len() < 24);
        region.stream_progress(true, 1001, None).unwrap();
        region.progress.as_mut().unwrap().ticked = Instant::now() - Duration::from_millis(101);
        assert!(region.tick_progress());
        region.repaint().unwrap();
        region.input(&draft, 7, "", 80, 30).unwrap();
        assert_eq!(region.stream_layouts, layouts);
        assert_eq!(region.buffer, draft);
        assert_eq!(region.caret, 7);
        assert_eq!(region.stream_text, text);
        region.input(&draft, 7, "", 30, 20).unwrap();
        assert_eq!(region.stream_layouts, layouts + 1);
        assert_eq!(region.stream_columns, 30);
        assert_eq!(region.buffer, draft);
        assert_eq!(region.caret, 7);
        assert!(region.visible.len() < 20);
        region.clear_stream().unwrap();
        assert!(region.stream_rows.is_empty());
        assert_eq!(region.buffer, draft);
        assert_eq!(region.caret, 7);
    }

    #[test]
    fn submitted_multiline_messages_lose_the_frame_but_the_next_prompt_keeps_it() {
        for color in [false, true] {
            let mut region = Region::new();
            region.color = color;
            region.input(&[], 0, "", 40, 24).unwrap();
            region.submit("hello\nworld\t\x1b[2J").unwrap();
            let expected = ["❯ hello", "  world⇥\\x1b[2J"];
            assert_eq!(region.visible.len(), expected.len());
            for (row, expected) in region.visible.iter().zip(expected) {
                assert_eq!(
                    row,
                    &if color {
                        format!("{SUBMITTED}{expected}{RESET}")
                    } else {
                        expected.into()
                    }
                );
            }
            assert!(!region.active);
            region.input(&['n'], 1, "", 40, 24).unwrap();
            assert!(region.visible[0].contains('─'));
            assert!(region.visible[1].contains("❯ n"));
            assert!(region.visible[2].contains('─'));
        }
    }

    #[test]
    fn streamed_lines_remain_speculative_above_the_frame_and_preserve_the_draft() {
        let mut region = Region::new();
        region.color = false;
        let draft: Vec<_> = "next question".chars().collect();
        region.input(&draft, 3, "", 80, 24).unwrap();
        region
            .set_progress(Some("thinking · model · step 1"))
            .unwrap();
        let started = region.progress.as_ref().unwrap().started;
        region
            .stream_progress(true, 12, Some("First live line.\nSecond live line."))
            .unwrap();
        assert_eq!(region.visible[0], "First live line.");
        assert_eq!(region.visible[1], "Second live line.");
        assert!(region.visible[2].contains(WRITING_PHRASES[0]));
        assert!(region.visible[2].contains("~12 output tokens"));
        assert!(!region.visible[2].contains("model"));
        assert!(region.visible.last().unwrap().contains("step 1"));
        assert!(region.visible[3].starts_with('─'));
        assert_eq!(region.progress.as_ref().unwrap().started, started);
        assert_eq!(region.buffer, draft);
        assert_eq!(region.caret, 3);
        assert!(region.pending.is_empty());
        assert!(!region.printed);
        region.clear_stream().unwrap();
        assert!(!region.visible.iter().any(|line| line.contains("live line")));
        region
            .set_progress(Some("thinking · model · attempt 2"))
            .unwrap();
        assert_eq!(region.progress.as_ref().unwrap().output_tokens, Some(0));
        region.stream_progress(false, 5, None).unwrap();
        assert!(region.visible[0].contains(THINKING_PHRASES[0]));
        assert!(!region.visible[0].contains(WRITING_PHRASES[0]));
        region.set_progress(None).unwrap();
        assert!(region.visible[0].starts_with('─'));
    }

    #[test]
    fn exec_tail_is_bounded_safe_and_removed_without_touching_the_draft() {
        let mut region = Region::new();
        region.color = true;
        let draft: Vec<_> = "next question".chars().collect();
        region.input(&draft, 3, "", 40, 24).unwrap();
        region.set_progress(Some("running first")).unwrap();
        let started = region.progress.as_ref().unwrap().started;
        let mut lines: Vec<_> = (0..7).map(|i| format!("output {i}")).collect();
        lines.push("\x1b[2J\r\t**literal**".into());
        lines.push("界".repeat(100));
        let visible = region.visible.clone();
        region.set_exec_lines(&lines);
        assert_eq!(region.visible, visible, "publishing must not repaint");
        region.input(&draft, 3, "", 40, 24).unwrap();
        assert_eq!(region.exec_lines.len(), 5);
        assert_eq!(region.progress.as_ref().unwrap().started, started);
        assert!(region.visible[1].contains("output 4"));
        assert!(region.visible[4].contains("\\x1b[2J\\x0d⇥**literal**"));
        assert!(region.visible[4].contains(DIM));
        assert!(region.visible[1..6].iter().all(|line| width(line) < 40));
        assert!(region.visible[6].starts_with("\x1b[2m─"));
        region.input(&draft, 3, "", 20, 8).unwrap();
        assert!(region.visible.len() < 8);
        assert!(region.visible.iter().all(|line| width(line) <= 20));
        region.set_progress(Some("running second")).unwrap();
        assert!(region.exec_lines.is_empty());
        region.set_progress(None).unwrap();
        assert!(region
            .visible
            .iter()
            .all(|line| !line.contains("output") && !line.contains('⚗')));
        assert_eq!(region.buffer, draft);
        assert_eq!(region.caret, 3);
        assert!(!region.printed);
    }

    #[test]
    fn browsing_progress_is_transient_and_accepts_multi_path_searches() {
        let region = Arc::new(Mutex::new(Region::new()));
        for (name, arguments, expected) in [
            (
                "read_file",
                serde_json::json!({"path":"src/a.rs"}),
                "reading src/a.rs",
            ),
            (
                "list_files",
                serde_json::json!({"path":"src"}),
                "listing src",
            ),
            (
                "search_text",
                serde_json::json!({"paths":["src", "tests"], "query":"needle"}),
                "searching \"needle\" in src, tests",
            ),
            (
                "exec",
                serde_json::json!({"argv":["cargo", "test"]}),
                "running cargo test",
            ),
        ] {
            crate::render::tool_started(
                Some(&region),
                &serde_json::json!({"name":name,"arguments":arguments.to_string()}),
            );
            let pane = region.lock().unwrap();
            assert_eq!(pane.progress.as_ref().unwrap().label, expected);
            assert!(!pane.printed);
        }
        crate::render::model_finished(Some(&region));
        assert!(region.lock().unwrap().progress.is_none());
    }

    #[test]
    fn progress_animates_rotates_words_and_reserves_the_last_cell() {
        let now = Instant::now();
        let mut progress = Progress {
            label: "thinking · model · step 1".into(),
            started: now,
            ticked: now,
            frame: 0,
            output_tokens: Some(12),
        };
        let first = progress.text(80, false);
        assert!(first.starts_with("·⚗ Distilling thoughts… · 0.0s"));
        assert!(!first.contains("model"));
        assert!(!first.contains("step"));
        assert_eq!(width(&progress.text(10, false)), 9);
        progress.frame = 1;
        assert!(progress.text(80, false).starts_with("∘⚗"));
        progress.started = now - Duration::from_millis(1500);
        assert!(progress
            .text(80, true)
            .starts_with(&format!("{BOLD}{ACCENT}")));
        assert!(progress
            .text(80, true)
            .ends_with(" · 1.5s · ~12 output tokens\x1b[0m"));
        progress.started = now - Duration::from_secs(4);
        assert!(progress.text(80, false).contains(THINKING_PHRASES[1]));
        progress.started = now - Duration::from_secs(65);
        assert!(progress.text(80, false).contains(" · 1m05s"));
    }

    #[test]
    fn model_progress_keeps_phrase_lists_separate_and_truncates_every_phrase_cleanly() {
        for (label, phrases) in [
            ("thinking", THINKING_PHRASES),
            ("writing reply", WRITING_PHRASES),
        ] {
            for index in 0..=phrases.len() {
                let now = Instant::now();
                let progress = Progress {
                    label: label.into(),
                    started: now - Duration::from_secs(4 * index as u64 + 1),
                    ticked: now,
                    frame: 0,
                    output_tokens: Some(12),
                };
                let full = progress.text(120, false);
                assert!(full.starts_with(&format!("·⚗ {} · ", phrases[index % phrases.len()])));
                assert!(full.ends_with(" · ~12 output tokens"));
                for columns in [2_usize, 10, 20, 40, 80] {
                    for color in [false, true] {
                        let row = progress.text(columns, color);
                        let plain = if color {
                            row.strip_prefix(&format!("{BOLD}{ACCENT}"))
                                .unwrap()
                                .strip_suffix(RESET)
                                .unwrap()
                        } else {
                            &row
                        };
                        let available = columns.saturating_sub(1);
                        assert!(
                            width(plain) <= available,
                            "{label}, {index}, {columns}: {row:?}"
                        );
                        if width(&full) > available {
                            assert!(plain.ends_with('…'), "{row:?}");
                        }
                    }
                }
            }
        }
        let now = Instant::now();
        let tool = Progress {
            label: "searching for a file".into(),
            started: now,
            ticked: now,
            frame: 0,
            output_tokens: None,
        };
        assert!(tool
            .text(80, false)
            .starts_with("·⚗ searching for a file · "));
    }

    #[test]
    fn working_agent_rows_are_bounded_safe_live_and_removed() {
        let mut region = Region::new();
        region.color = false;
        region.input(&[], 0, "", 100, 24).unwrap();
        let started = Instant::now() - Duration::from_secs(65);
        let mut rows: Vec<_> = (1..=10)
            .map(|id| crate::agents::AgentRow {
                label: format!("t{id} worker"),
                activity: "thinking".into(),
                started,
            })
            .collect();
        rows[0].label = "t1 worker\n\t\x1b[2J\r".into();
        region.refresh_agents(10, None, &rows).unwrap();
        assert_eq!(
            region
                .visible
                .iter()
                .filter(|row| row.contains(" · thinking · "))
                .count(),
            7
        );
        assert!(region.visible[0].contains("t1 worker↵⇥\\x1b[2J\\x0d"));
        assert!(region.visible[0].contains("1m05s"));
        assert!(region.visible[7].contains("+3 more"));
        assert!(!region.visible[0].contains('\x1b'));
        assert!(!region.printed);
        rows.truncate(1);
        rows[0].activity = "exec".into();
        region.refresh_agents(1, None, &rows).unwrap();
        assert!(region.visible[0].contains(" · exec · "));
        assert!(!region
            .visible
            .iter()
            .any(|row| row.contains("thinking") || row.contains("more")));
        assert_eq!(region.agents[0].started, started);
        region.color = true;
        region.input(&[], 0, "", 30, 9).unwrap();
        assert!(region.visible[0].contains(DIM));
        assert!(width(&region.visible[0]) < 30);
        assert!(region.visible.iter().all(|row| width(row) <= 30));
        region.refresh_agents(0, None, &[]).unwrap();
        assert!(region.agents.is_empty());
        assert!(region.visible[0].contains('─'));
        assert!(!region.printed);
    }

    #[test]
    fn agent_refresh_preserves_clocks_drafts_and_progress_when_no_wait_label_is_supplied() {
        let mut region = Region::new();
        region.color = false;
        region.status.model = "model".into();
        region.status.auto = true;
        region
            .set_progress(Some("waiting on t1 worker (step 1)"))
            .unwrap();
        let started = Instant::now() - Duration::from_secs(5);
        let ticked = Instant::now() - Duration::from_millis(50);
        let progress = region.progress.as_mut().unwrap();
        progress.started = started;
        progress.ticked = ticked;
        progress.frame = 4;
        region.buffer = "draft text".chars().collect();
        region.caret = 3;
        let handle = Arc::new(Mutex::new(region));
        crate::render::agents_refreshed(
            Some(&handle),
            2,
            Some("waiting on t1 worker (step 2)"),
            &[],
            &[],
        );
        {
            let region = handle.lock().unwrap();
            let progress = region.progress.as_ref().unwrap();
            assert_eq!(progress.started, started);
            assert_eq!(progress.ticked, ticked);
            assert_eq!(progress.frame, 4);
            assert_eq!(progress.label, "waiting on t1 worker (step 2)");
            assert_eq!(region.status.row(80, false), "  model · 2 agents · auto");
            assert_eq!(region.status.row(20, false), "  model · 2 agents");
            assert_eq!(region.buffer.iter().collect::<String>(), "draft text");
            assert_eq!(region.caret, 3);
        }
        crate::render::agents_refreshed(Some(&handle), 1, None, &[], &[]);
        {
            let mut region = handle.lock().unwrap();
            assert_eq!(region.status.row(80, false), "  model · 1 agent · auto");
            assert_eq!(
                region.progress.as_ref().unwrap().label,
                "waiting on t1 worker (step 2)"
            );
            region
                .status
                .update(&serde_json::json!({"model":"next"}), &Value::Null);
            assert_eq!(region.status.row(80, false), "  next · 1 agent");
        }
        crate::render::agents_refreshed(Some(&handle), 0, None, &[], &[]);
        let region = handle.lock().unwrap();
        assert_eq!(region.status.row(80, false), "  next");
        assert_eq!(region.progress.as_ref().unwrap().started, started);
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
    fn fast_status_is_enabled_only_and_survives_model_width_omission() {
        let mut status = Status::default();
        for fast in [false, true, false] {
            status.update(
                &serde_json::json!({"model":"gpt-6.1-sol", "reasoning_effort":"medium", "fast":fast}),
                &Value::Null,
            );
            assert_eq!(
                status.row(80, false),
                if fast {
                    "  fast · gpt-6.1-sol / medium"
                } else {
                    "  gpt-6.1-sol / medium"
                }
            );
            if fast {
                assert_eq!(status.row(8, false), "  fast");
            }
        }
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
        assert_eq!(
            status.prompt(true),
            format!("(blocked) {BOLD}{ACCENT}❯{RESET} ")
        );
    }
}
