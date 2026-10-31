use crate::render::{columns, display, sanitize, styled, truncate, width};
use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::os::fd::AsRawFd;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};
use std::time::{Duration, Instant};
mod editor;
#[cfg(test)]
mod panic_tests;
mod readiness;
mod region;
use crate::tools::{WorkspaceConsent, WorkspaceConsentError, WorkspaceDestination};
use editor::{Decoder, Editor as IdleEditor, Key, TurnInput};
pub(crate) use readiness::poll_read;
pub(crate) use region::output;
pub use region::{Region, RegionHandle};
use serde_json::Value;

#[derive(Default)]
struct Consent {
    running: bool,
    request: Option<(Value, WorkspaceDestination)>,
    answer: Option<Result<bool, WorkspaceConsentError>>,
}

static PANIC_TERMINAL: Mutex<Option<(i32, libc::termios)>> = Mutex::new(None);

/// Install panic hook restores raw input before terminating the whole process.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let saved = PANIC_TERMINAL.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((fd, previous)) = *saved {
            unsafe {
                libc::tcsetattr(fd, libc::TCSANOW, &previous);
                // Do not wait for a terminal renderer's stderr lock during a crash.
                let disable = b"\x1b[?2004l";
                libc::write(2, disable.as_ptr().cast(), disable.len());
            }
        }
        let message = format!("{info}\n");
        unsafe {
            libc::write(2, message.as_ptr().cast(), message.len());
        }
        // Test and debug builds unwind, but a worker panic must never strand a wait.
        std::process::abort();
    }));
}

struct Raw {
    fd: i32,
    previous: libc::termios,
}
impl Raw {
    fn enter() -> io::Result<Self> {
        Self::enter_fd(0)
    }
    fn enter_fd(fd: i32) -> io::Result<Self> {
        unsafe {
            let mut old = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut old) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut saved = PANIC_TERMINAL.lock().unwrap_or_else(|e| e.into_inner());
            let mut raw = old;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
            *saved = Some((fd, old));
            drop(saved);
            let guard = Self { fd, previous: old };
            let _ = io::stderr().write_all(b"\x1b[?2004h");
            Ok(guard)
        }
    }
}
impl Drop for Raw {
    fn drop(&mut self) {
        let _ = io::stderr().write_all(b"\x1b[?2004l");
        let mut saved = PANIC_TERMINAL.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.previous);
        }
        *saved = None;
    }
}

/// Read supplies one complete submission from a pipe or an editable terminal.
pub struct Input {
    terminal: bool,
    history: Vec<String>,
    reader: io::BufReader<io::Stdin>,
    tty: Option<File>,
    raw: Option<Raw>,
    region: Option<RegionHandle>,
    decoder: Decoder,
    next: TurnInput,
    consent: Arc<(Mutex<Consent>, Condvar)>,
}
/// Submission distinguishes text, interruption, and end of input.
pub enum Submission {
    Text(String),
    Bytes(Vec<u8>),
    Interrupted,
    Eof,
}
impl Input {
    /// New selects raw input only for a terminal stdin.
    pub fn new() -> Self {
        let terminal = unsafe { libc::isatty(0) == 1 };
        let tty = if terminal
            && unsafe { libc::isatty(2) == 1 }
            && std::env::var("TERM").unwrap_or_default() != "dumb"
        {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .ok()
        } else {
            None
        };
        let region = tty.as_ref().map(|_| Arc::new(Mutex::new(Region::new())));
        Self {
            terminal,
            history: Vec::new(),
            reader: io::BufReader::new(io::stdin()),
            tty,
            raw: None,
            region,
            decoder: Decoder::default(),
            next: TurnInput::default(),
            consent: Arc::new((Mutex::new(Consent::default()), Condvar::new())),
        }
    }
    /// Workspace consent delegates a tool's prompt to the sole turn-input reader.
    pub(crate) fn workspace_consent(&self) -> Box<WorkspaceConsent> {
        let interactive = self.terminal;
        let shared = self.consent.clone();
        shared.0.lock().unwrap().running = interactive;
        Box::new(move |config, destination| {
            if !interactive {
                return Err(WorkspaceConsentError::NotInteractive);
            }
            let (lock, ready) = &*shared;
            let mut state = lock.lock().unwrap();
            if !state.running {
                return Err(WorkspaceConsentError::Unavailable);
            }
            state.answer = None;
            state.request = Some((config.clone(), destination.clone()));
            ready.notify_all();
            loop {
                if let Some(answer) = state.answer.take() {
                    return answer;
                }
                if !state.running || crate::cancellation::cancelled(config) {
                    state.request = None;
                    return Err(WorkspaceConsentError::Unavailable);
                }
                state = ready
                    .wait_timeout(state, Duration::from_millis(50))
                    .unwrap()
                    .0;
            }
        })
    }
    fn service_consent(
        &mut self,
        next: &mut TurnInput,
        cancelled: &AtomicBool,
    ) -> io::Result<bool> {
        let request = self.consent.0.lock().unwrap().request.take();
        let Some((config, destination)) = request else {
            return Ok(false);
        };
        *next = TurnInput::default();
        let result = (|| {
            self.discard_input()?;
            let mut disclosure = format!(
                "Allow this location as a workspace?\nLocation: {}\n",
                crate::errors::quote(&destination.path.to_string_lossy())
            );
            if destination.missing {
                disclosure.push_str("Destination does not exist; only this exact location will be approved, not its parent.\n");
            }
            disclosure.push_str(&format!("Authority: {}\n", crate::render::mode(&config)));
            if config["plan"].as_bool().unwrap_or(false) {
                disclosure.push_str("Plan mode remains on; edits and commands are refused.\n");
            }
            disclosure.push_str("Duration: until this chat exits, including /reset, /model, and compaction.\nFiles read here may be sent to the provider and recorded in traces.\n");
            output(self.region.as_ref(), 2, &disclosure);
            let options = [
                Choice {
                    label: "Allow for this session".into(),
                    detail: String::new(),
                    note: String::new(),
                    disabled: false,
                },
                Choice {
                    label: "Deny".into(),
                    detail: String::new(),
                    note: String::new(),
                    disabled: false,
                },
            ];
            self.choose_options(
                "Workspace consent",
                &options,
                1,
                false,
                "deny",
                true,
                Some(cancelled),
            )
        })();
        let answer = match result {
            Ok(Selection::Chosen(index)) => Ok(index == 0 && !cancelled.load(Ordering::Acquire)),
            Ok(Selection::NotInteractive) => Err(WorkspaceConsentError::NotInteractive),
            _ => Err(WorkspaceConsentError::Unavailable),
        };
        let (lock, ready) = &*self.consent;
        lock.lock().unwrap().answer = Some(answer);
        ready.notify_all();
        if self.region.is_some() {
            self.redraw(&next.editor, &next.prefix)?;
        }
        Ok(true)
    }
    fn choose_region(
        &mut self,
        title: &str,
        options: &[Choice],
        current: usize,
        shortcuts: bool,
        cancel_label: &str,
        fresh: bool,
        cancelled: Option<&AtomicBool>,
    ) -> io::Result<Selection> {
        let (width, height) = self.size()?;
        if width < 2
            || height < options.len() + if width >= 20 { 5 } else { 2 }
            || options.is_empty()
        {
            return Ok(Selection::NotInteractive);
        }
        let mut selected = if current < options.len() && !options[current].disabled {
            current
        } else if let Some(index) = options.iter().position(|option| !option.disabled) {
            index
        } else {
            return Ok(Selection::NotInteractive);
        };
        let mut header = if cancel_label.is_empty() {
            format!("{}  ↑↓ move · enter · esc", sanitize(title))
        } else {
            format!(
                "{}   ↑↓ move · enter choose · esc {}",
                sanitize(title),
                sanitize(cancel_label)
            )
        };
        if crate::render::width(&header) > width - 1 {
            header = if cancel_label.is_empty() {
                format!("↑↓ move · enter · esc  {}", sanitize(title))
            } else {
                format!("esc {}  {}", sanitize(cancel_label), sanitize(title))
            };
        }
        let header = truncate(&header, width - 1);
        let region = self.region.as_ref().unwrap().clone();
        let draw = |selected| {
            let rows: Vec<String> = options
                .iter()
                .enumerate()
                .map(|(index, option)| {
                    let row = format!(
                        "{} {}  {:<17} {:<10} {}",
                        if index == selected { "❯" } else { " " },
                        index + 1,
                        sanitize(&option.label),
                        sanitize(&option.detail),
                        sanitize(&option.note)
                    );
                    let row = truncate(row.trim_end(), width - 1);
                    if !styled(2) {
                        row
                    } else if option.disabled {
                        format!("\x1b[2m{row}\x1b[0m")
                    } else if index == selected {
                        format!("\x1b[36m\x1b[1m{row}\x1b[0m")
                    } else {
                        row
                    }
                })
                .collect();
            region.lock().unwrap().picker(&header, rows, width, height)
        };
        draw(selected)?;
        if fresh {
            self.discard_input()?;
        }
        let result = (|| loop {
            if cancelled.is_some_and(|token| token.load(Ordering::Acquire)) {
                return Ok(Selection::Cancelled);
            }
            let key = if let Some(key) = self.decoder.next() {
                Some(key)
            } else {
                let fd = self.tty.as_ref().unwrap().as_raw_fd();
                let ready = match poll_read(fd, 50) {
                    Ok(ready) => ready,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                };
                if ready == 0 {
                    self.decoder.escape()
                } else {
                    let mut bytes = [0; 256];
                    let count = self.tty.as_mut().unwrap().read(&mut bytes)?;
                    if count == 0 {
                        return Ok(Selection::Cancelled);
                    }
                    self.decoder.feed(&bytes[..count]);
                    continue;
                }
            };
            let Some(key) = key else {
                continue;
            };
            match key {
                Key::Name("up") | Key::Text('k') if shortcuts || key == Key::Name("up") => {
                    if let Some(index) = (0..selected).rev().find(|index| !options[*index].disabled)
                    {
                        selected = index;
                        draw(selected)?;
                    }
                }
                Key::Name("down") | Key::Text('j') if shortcuts || key == Key::Name("down") => {
                    if let Some(index) =
                        (selected + 1..options.len()).find(|index| !options[*index].disabled)
                    {
                        selected = index;
                        draw(selected)?;
                    }
                }
                Key::Name("enter") => return Ok(Selection::Chosen(selected)),
                Key::Name("escape" | "interrupt" | "eof") => return Ok(Selection::Cancelled),
                Key::Text('q') if shortcuts => return Ok(Selection::Cancelled),
                Key::Text(c @ '1'..='9') if shortcuts => {
                    let index = c as usize - '1' as usize;
                    if index < options.len() && !options[index].disabled {
                        return Ok(Selection::Chosen(index));
                    }
                }
                _ if !shortcuts => return Ok(Selection::Cancelled),
                _ => {}
            }
        })();
        self.decoder = Decoder::default();
        if cancel_label == "keep planning" {
            region.lock().unwrap().collapse()?;
        } else {
            let next = std::mem::take(&mut self.next);
            let restored = self.redraw(&next.editor, &next.prefix);
            self.next = next;
            restored?;
        }
        result
    }
    fn discard_input(&mut self) -> io::Result<()> {
        self.decoder = Decoder::default();
        self.next = TurnInput::default();
        if let Some(region) = &self.region {
            region.lock().unwrap().queued = false;
        }
        let buffered = self.reader.buffer().len();
        self.reader.consume(buffered);
        let fd = self.tty.as_ref().map_or(0, |tty| tty.as_raw_fd());
        if unsafe { libc::tcflush(fd, libc::TCIFLUSH) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    /// Handoff releases raw input and the region before another terminal owner runs.
    pub(crate) fn handoff(&mut self) -> io::Result<()> {
        if let Some(region) = &self.region {
            region.lock().unwrap().collapse()?;
        }
        self.raw.take();
        Ok(())
    }

    /// Restore reacquires raw input and immediately repaints after an editor returns.
    pub(crate) fn restore(&mut self) -> io::Result<()> {
        if self.region.is_none() {
            return Ok(());
        }
        self.raw = Some(Raw::enter()?);
        self.redraw(&IdleEditor::default(), "")
    }

    /// Region returns the pane shared with this chat's presentation.
    pub fn region(&self) -> Option<RegionHandle> {
        self.region.clone()
    }
    /// Set status refreshes the session labels used on the next input redraw.
    pub fn set_status(&mut self, config: &serde_json::Value, state: &serde_json::Value) {
        if let Some(region) = &self.region {
            let _ = region.lock().unwrap().refresh_status(config, state);
        }
    }
    /// Interactive reports whether raw terminal editing is available.
    pub fn interactive(&self) -> bool {
        self.terminal
    }
    /// Choose offers terminal choices without adding selection keys to chat input.
    pub fn choose(
        &mut self,
        title: &str,
        options: &[Choice],
        current: usize,
    ) -> io::Result<Selection> {
        self.choose_options(title, options, current, true, "", false, None)
    }
    /// Choose plan discards turn type-ahead and requires an explicit highlighted choice.
    pub(crate) fn choose_plan(&mut self) -> io::Result<Selection> {
        if let Some(region) = &self.region {
            self.next = TurnInput::default();
            self.decoder = Decoder::default();
            region.lock().unwrap().queued = false;
        }
        let options: Vec<_> = [
            (
                "Implement here",
                "leaves plan mode and asks the model to implement it",
            ),
            (
                "Implement fresh",
                "starts a new session holding only the plan",
            ),
            ("Keep planning", ""),
        ]
        .into_iter()
        .map(|(label, detail)| Choice {
            label: label.into(),
            detail: detail.into(),
            note: String::new(),
            disabled: false,
        })
        .collect();
        self.choose_options(
            "Plan ready",
            &options,
            2,
            false,
            "keep planning",
            false,
            None,
        )
    }
    /// Write user frame records an implementation request before its generated turn.
    pub(crate) fn write_user_frame(&self, message: &str) {
        if let Some(region) = &self.region {
            let columns = region.lock().unwrap().width;
            let rule = "─".repeat(columns);
            let rule = if styled(2) {
                format!("\x1b[2m{rule}\x1b[0m")
            } else {
                rule
            };
            let lines = crate::render::wrap("❯ ", "  ", message, columns.saturating_sub(1));
            let frame = if columns >= 20 {
                format!("{rule}\r\n{}\r\n{rule}\r\n", lines.join("\r\n"))
            } else {
                format!("{}\r\n", lines.join("\r\n"))
            };
            output(Some(region), 2, &frame);
        } else {
            output(None, 2, &format!("> {message}\n"));
        }
    }
    fn choose_options(
        &mut self,
        title: &str,
        options: &[Choice],
        current: usize,
        shortcuts: bool,
        cancel_label: &str,
        fresh: bool,
        cancelled: Option<&AtomicBool>,
    ) -> io::Result<Selection> {
        if fresh {
            self.discard_input()?;
        }
        if self.region.is_some() {
            return self.choose_region(
                title,
                options,
                current,
                shortcuts,
                cancel_label,
                fresh,
                cancelled,
            );
        }
        self.raw.take();
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if !self.terminal
            || unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut size) } != 0
            || size.ws_col < 2
            || (size.ws_row as usize) < options.len() + 2
            || options.is_empty()
        {
            return Ok(Selection::NotInteractive);
        }
        let mut cursor = if current < options.len() && !options[current].disabled {
            current
        } else {
            options
                .iter()
                .position(|c| !c.disabled)
                .unwrap_or(options.len())
        };
        if cursor == options.len() {
            return Ok(Selection::NotInteractive);
        }
        let _raw = Raw::enter()?;
        let columns = size.ws_col as usize;
        let rows = options.len() + 1;
        let mut header = if cancel_label.is_empty() {
            format!("{}  ↑↓ move · enter · esc", sanitize(title))
        } else {
            format!(
                "{}   ↑↓ move · enter choose · esc {}",
                sanitize(title),
                sanitize(cancel_label)
            )
        };
        if width(&header) > columns - 1 {
            header = if cancel_label.is_empty() {
                format!("↑↓ move · enter · esc  {}", sanitize(title))
            } else {
                format!("esc {}  {}", sanitize(cancel_label), sanitize(title))
            };
        }
        header = crate::render::truncate(&header, columns - 1);
        eprint!("\x1b[?25l");
        let result = (|| {
            let mut redraw = false;
            loop {
                if redraw {
                    eprint!("\x1b[{rows}A\r")
                }
                eprint!("\x1b[2K{header}\r\n");
                for (i, option) in options.iter().enumerate() {
                    let row = format!(
                        "{} {}  {:<17} {:<10} {}",
                        if i == cursor { "❯" } else { " " },
                        i + 1,
                        crate::render::sanitize(&option.label),
                        crate::render::sanitize(&option.detail),
                        crate::render::sanitize(&option.note)
                    );
                    let row = crate::render::truncate(row.trim_end_matches(' '), columns - 1);
                    let style = if !crate::render::styled(2) {
                        ""
                    } else if option.disabled {
                        "\x1b[2m"
                    } else if i == cursor {
                        "\x1b[36m\x1b[1m"
                    } else {
                        ""
                    };
                    eprint!(
                        "\x1b[2K{style}{row}{}\r\n",
                        if style.is_empty() { "" } else { "\x1b[0m" }
                    );
                }
                io::stderr().flush()?;
                redraw = true;
                let key = read_key(&mut self.reader, &mut self.decoder)?;
                match key.as_str() {
                    "up" | "text:k" if shortcuts || key == "up" => {
                        if let Some(next) = (0..cursor).rev().find(|i| !options[*i].disabled) {
                            cursor = next
                        }
                    }
                    "down" | "text:j" if shortcuts || key == "down" => {
                        if let Some(next) =
                            (cursor + 1..options.len()).find(|i| !options[*i].disabled)
                        {
                            cursor = next
                        }
                    }
                    "enter" => return Ok(Selection::Chosen(cursor)),
                    "escape" | "interrupt" | "eof" | "text:q" => return Ok(Selection::Cancelled),
                    _ => {
                        if !shortcuts {
                            return Ok(Selection::Cancelled);
                        }
                        if let Some(number) = key
                            .strip_prefix("text:")
                            .and_then(|s| s.parse::<usize>().ok())
                            .filter(|n| *n > 0 && *n <= 9)
                        {
                            if let Some(option) = options.get(number - 1) {
                                if !option.disabled {
                                    return Ok(Selection::Chosen(number - 1));
                                }
                            }
                        }
                    }
                }
            }
        })();
        eprint!("\x1b[{rows}A\r\x1b[J\x1b[?25h");
        result
    }
    /// Read line handles continuation and bracketed pastes as one submission.
    pub fn read_line(&mut self, prompt: &str) -> io::Result<Submission> {
        if !self.terminal {
            let mut line = Vec::new();
            if self.reader.read_until(b'\n', &mut line)? == 0 {
                return Ok(Submission::Eof);
            }
            if line.ends_with(b"\n") {
                line.pop();
                if line.ends_with(b"\r") {
                    line.pop();
                }
            }
            return Ok(match String::from_utf8(line) {
                Ok(text) => Submission::Text(text),
                Err(error) => Submission::Bytes(error.into_bytes()),
            });
        }
        if self.region.is_some() {
            let result = self.regional_line();
            eprint!("\x1b[?2004l");
            io::stderr().flush()?;
            return result;
        }
        let _raw = Raw::enter()?;
        let mut line = String::new();
        let mut current_prompt = prompt;
        loop {
            match self.physical(current_prompt)? {
                Submission::Text(next) => {
                    line.push_str(&next);
                    if line.ends_with('\\') && !line.ends_with("\\\\") {
                        line.pop();
                        line.push('\n');
                        current_prompt = "… ";
                        continue;
                    }
                    return Ok(Submission::Text(line));
                }
                other => return Ok(other),
            }
        }
    }
    fn physical(&mut self, prompt: &str) -> io::Result<Submission> {
        let mut editor = Editor::default();
        let mut history = 0;
        let mut draft = String::new();
        eprint!("{prompt}");
        io::stderr().flush()?;
        loop {
            let key = read_key(&mut self.reader, &mut self.decoder)?;
            match key.as_str() {
                "eof" => {
                    if editor.buffer.is_empty() {
                        eprint!("\r\n");
                        return Ok(Submission::Eof);
                    }
                }
                "interrupt" => {
                    eprint!("^C\r\n");
                    return Ok(Submission::Interrupted);
                }
                "paste-start" | "paste-end" => {}
                "enter" => {
                    let text = editor.value();
                    if !text.is_empty() && self.history.last() != Some(&text) {
                        self.history.push(text.clone());
                        if self.history.len() > 200 {
                            self.history.remove(0);
                        }
                    }
                    eprint!("\r\n");
                    return Ok(Submission::Text(text));
                }
                "clear" => eprint!("\x1b[2J\x1b[H"),
                "up" => {
                    if history < self.history.len() {
                        if history == 0 {
                            draft = editor.value()
                        }
                        history += 1;
                        editor.replace(&self.history[self.history.len() - history])
                    }
                }
                "down" => {
                    if history > 0 {
                        history -= 1;
                        editor.replace(if history == 0 {
                            &draft
                        } else {
                            &self.history[self.history.len() - history]
                        })
                    }
                }
                "tab" => {
                    let line = editor.value();
                    let commands = [
                        "/plan",
                        "/model",
                        "/effort",
                        "/auto",
                        "/status",
                        "/context",
                        "/compact",
                        "/friction",
                        "/review",
                        "/trace",
                        "/reset",
                        "/edit",
                        "/help",
                        "/exit",
                    ];
                    let matches: Vec<_> =
                        commands.iter().filter(|c| c.starts_with(&line)).collect();
                    if matches.len() == 1 {
                        editor.replace(matches[0])
                    }
                }
                _ => editor.apply(&key),
            }
            draw(prompt, &editor)?;
        }
    }
}
impl Input {
    fn size(&self) -> io::Result<(usize, usize)> {
        let fd = self.tty.as_ref().map_or(0, AsRawFd::as_raw_fd);
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((usize::from(size.ws_col), usize::from(size.ws_row)))
    }

    fn cursor_row(&mut self) -> io::Result<usize> {
        let fd = self.tty.as_ref().unwrap().as_raw_fd();
        eprint!("\x1b[6n");
        io::stderr().flush()?;
        let deadline = Instant::now() + Duration::from_millis(100);
        let mut pending = Vec::new();
        let mut row = 0;
        while Instant::now() < deadline && pending.len() < 256 {
            if !matches!(poll_read(fd, 5), Ok(ready) if ready != 0) {
                continue;
            }
            let mut chunk = [0u8; 64];
            let count = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
            if count <= 0 {
                break;
            }
            pending.extend_from_slice(&chunk[..count as usize]);
            let mut response = None;
            for start in 0..pending.len().saturating_sub(4) {
                if !pending[start..].starts_with(b"\x1b[") {
                    continue;
                }
                let Some(end) = pending[start + 2..]
                    .iter()
                    .position(|b| *b == b'R')
                    .map(|end| start + 2 + end)
                else {
                    continue;
                };
                let Ok(position) = std::str::from_utf8(&pending[start + 2..end]) else {
                    continue;
                };
                if let Some((r, c)) = position
                    .split_once(';')
                    .and_then(|(r, c)| Some((r.parse::<usize>().ok()?, c.parse::<usize>().ok()?)))
                {
                    if r > 0 && c > 0 {
                        response = Some((start, end + 1, r));
                        break;
                    }
                }
            }
            if let Some((start, end, position)) = response {
                pending.drain(start..end);
                row = position;
                break;
            }
        }
        self.decoder.feed(&pending);
        Ok(row)
    }

    fn redraw(&mut self, editor: &IdleEditor, prefix: &str) -> io::Result<()> {
        let handle = self.region.clone().unwrap();
        if !handle.lock().unwrap().active {
            handle.lock().unwrap().initial_row = self.cursor_row()?;
        }
        let (columns, height) = self.size()?;
        let result =
            handle
                .lock()
                .unwrap()
                .input(&editor.buffer, editor.caret, prefix, columns, height);
        result
    }

    /// Poll turn edits type-ahead while the main thread runs the model and tools.
    pub(crate) fn poll_turn(
        &mut self,
        stop: &AtomicBool,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        let mut next = std::mem::take(&mut self.next);
        let result = if self.region.is_some() {
            self.read_turn(&mut next, stop, cancelled)
        } else {
            while self.terminal
                && !stop.load(Ordering::Acquire)
                && !cancelled.load(Ordering::Acquire)
            {
                if let Err(error) = self.service_consent(&mut next, cancelled) {
                    self.consent.0.lock().unwrap().running = false;
                    self.consent.1.notify_all();
                    return Err(error);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        };
        self.consent.0.lock().unwrap().running = false;
        self.consent.1.notify_all();
        if self.region.is_none() {
            return result;
        }
        if cancelled.load(Ordering::Acquire) {
            next = TurnInput::default();
            self.decoder = Decoder::default();
        }
        // Keep the last turn's pane until its result and trace have been printed.
        self.next = next;
        result
    }

    fn read_turn(
        &mut self,
        next: &mut TurnInput,
        stop: &AtomicBool,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        self.redraw(&next.editor, &next.prefix)?;
        while !stop.load(Ordering::Acquire) && !cancelled.load(Ordering::Acquire) {
            if self.service_consent(next, cancelled)? {
                continue;
            }
            let key = if let Some(key) = self.decoder.next() {
                Some(key)
            } else {
                let fd = self.tty.as_ref().unwrap().as_raw_fd();
                let ready = match poll_read(fd, 50) {
                    Ok(ready) => ready,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                };
                if ready > 0 {
                    let mut chunk = [0; 256];
                    let count = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
                    if count == 0 {
                        next.exit = true;
                        return Ok(());
                    }
                    if count < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    self.decoder.feed(&chunk[..count as usize]);
                    continue;
                }
                self.decoder.escape()
            };
            let handle = self.region.as_ref().unwrap().clone();
            let (columns, height) = self.size()?;
            let mut region = handle.lock().unwrap();
            // A key already taken from the decoder when the turn ends belongs to the next
            // prompt's draft; only Ctrl-C and Ctrl-L would act on the finished turn.
            let ended = stop.load(Ordering::Acquire) || cancelled.load(Ordering::Acquire);
            let mut changed = !ended && region.tick_progress();
            if let Some(key) = key {
                let prompt = region.prompt();
                match next.apply(&key, &mut self.history, columns, &prompt) {
                    Some("interrupt") if !ended => cancelled.store(true, Ordering::Release),
                    Some("clear") if !ended => region.clear()?,
                    _ => {}
                }
                region.queued = next.queued.is_some();
                changed = true;
            }
            if ended {
                break;
            }
            if changed || columns != region.width || height != region.height {
                region.input(
                    &next.editor.buffer,
                    next.editor.caret,
                    &next.prefix,
                    columns,
                    height,
                )?;
            }
        }
        Ok(())
    }

    fn regional_line(&mut self) -> io::Result<Submission> {
        if self.raw.is_none() {
            self.raw = Some(Raw::enter_fd(self.tty.as_ref().unwrap().as_raw_fd())?);
        } else {
            eprint!("\x1b[?2004h");
        }
        self.region.as_ref().unwrap().lock().unwrap().queued = self.next.queued.is_some();
        if self.next.exit {
            self.next = TurnInput::default();
            self.region.as_ref().unwrap().lock().unwrap().collapse()?;
            return Ok(Submission::Eof);
        }
        if let Some(line) = self.next.queued.take() {
            self.next = TurnInput::default();
            self.region.as_ref().unwrap().lock().unwrap().queued = false;
            self.region
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .submit(&line)?;
            self.redraw(&IdleEditor::default(), "")?;
            return Ok(Submission::Text(line));
        }
        let mut editor = std::mem::take(&mut self.next.editor);
        let mut prefix = std::mem::take(&mut self.next.prefix);
        self.redraw(&editor, &prefix)?;
        loop {
            let key = if let Some(key) = self.decoder.next() {
                key
            } else {
                let fd = self.tty.as_ref().unwrap().as_raw_fd();
                let ready = match poll_read(fd, 50) {
                    Ok(ready) => ready,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                };
                if ready == 0 {
                    if let Some(key) = self.decoder.escape() {
                        key
                    } else {
                        let (columns, height) = self.size()?;
                        let handle = self.region.as_ref().unwrap().lock().unwrap();
                        let resized = columns != handle.width || height != handle.height;
                        drop(handle);
                        if resized {
                            self.redraw(&editor, &prefix)?;
                        }
                        continue;
                    }
                } else {
                    let mut chunk = [0u8; 256];
                    let count = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
                    if count == 0 {
                        self.region.as_ref().unwrap().lock().unwrap().collapse()?;
                        return Ok(Submission::Eof);
                    }
                    if count < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    self.decoder.feed(&chunk[..count as usize]);
                    continue;
                }
            };
            if matches!(key, Key::Name("tab")) {
                let line = editor.value();
                if editor.caret == editor.buffer.len() {
                    let commands = [
                        "/plan",
                        "/model",
                        "/effort",
                        "/auto",
                        "/status",
                        "/context",
                        "/compact",
                        "/friction",
                        "/review",
                        "/trace",
                        "/reset",
                        "/edit",
                        "/help",
                        "/exit",
                    ];
                    let matches: Vec<_> = commands
                        .iter()
                        .filter(|command| command.starts_with(&line))
                        .collect();
                    if matches.len() == 1 {
                        let command = *matches[0];
                        let suffix = if matches!(
                            command,
                            "/plan" | "/model" | "/effort" | "/auto" | "/compact" | "/review"
                        ) {
                            " "
                        } else {
                            ""
                        };
                        editor.replace(&format!("{command}{suffix}"));
                    }
                }
            } else {
                let columns = self.size()?.0;
                let prompt = self.region.as_ref().unwrap().lock().unwrap().prompt();
                match editor.apply(&key, &self.history, columns, &prompt) {
                    Some("enter") => {
                        let line = editor.value();
                        if !line.trim().is_empty() && self.history.last() != Some(&line) {
                            self.history.push(line.clone());
                            if self.history.len() > 500 {
                                self.history.remove(0);
                            }
                        }
                        if line.ends_with('\\') && !line.ends_with("\\\\") {
                            prefix.push_str(&line[..line.len() - 1]);
                            prefix.push('\n');
                            editor.replace("");
                            self.redraw(&editor, &prefix)?;
                            continue;
                        }
                        let line = prefix + &line;
                        self.region
                            .as_ref()
                            .unwrap()
                            .lock()
                            .unwrap()
                            .submit(&line)?;
                        editor.replace("");
                        self.redraw(&editor, "")?;
                        return Ok(Submission::Text(line));
                    }
                    Some("interrupt" | "eof") => {
                        let interrupt = matches!(key, Key::Name("interrupt"));
                        self.region.as_ref().unwrap().lock().unwrap().collapse()?;
                        self.raw.take();
                        self.decoder = Decoder::default();
                        return Ok(if interrupt {
                            Submission::Interrupted
                        } else {
                            Submission::Eof
                        });
                    }
                    Some("clear") => self.region.as_ref().unwrap().lock().unwrap().clear()?,
                    _ => {}
                }
            }
            self.redraw(&editor, &prefix)?;
        }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        if let Some(region) = &self.region {
            let _ = region.lock().unwrap().collapse();
        }
        self.raw.take();
    }
}
fn draw(prompt: &str, editor: &Editor) -> io::Result<()> {
    let text = display(&editor.value(), false, 0)
        .replace('\n', "↵")
        .replace('\t', "⇥");
    let before: String = editor.buffer[..editor.caret].iter().collect();
    let cursor = width(prompt) + width(&before.replace('\n', "↵").replace('\t', "⇥"));
    let _ = columns(2);
    eprint!("\r\x1b[2K{prompt}{text}\r");
    if cursor > 0 {
        eprint!("\x1b[{cursor}C")
    }
    io::stderr().flush()
}
fn read_key(reader: &mut io::BufReader<io::Stdin>, decoder: &mut Decoder) -> io::Result<String> {
    loop {
        if let Some(key) = decoder.next() {
            return Ok(match key {
                Key::Name(name) => name.into(),
                Key::Text(c) => format!("text:{c}"),
            });
        }
        if reader.buffer().is_empty() {
            let ready = match poll_read(0, 20) {
                Ok(ready) => ready,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if ready == 0 {
                if let Some(Key::Name(name)) = decoder.escape() {
                    return Ok(name.into());
                }
                continue;
            }
        }
        let mut bytes = [0; 256];
        let count = reader.read(&mut bytes)?;
        if count == 0 {
            return Ok("eof".into());
        }
        decoder.feed(&bytes[..count]);
    }
}
#[derive(Default)]
struct Editor {
    buffer: Vec<char>,
    caret: usize,
    kill: Vec<char>,
}
impl Editor {
    fn value(&self) -> String {
        self.buffer.iter().collect()
    }
    fn replace(&mut self, text: &str) {
        self.buffer = text.chars().collect();
        self.caret = self.buffer.len()
    }
    fn insert(&mut self, c: char) {
        self.buffer.insert(self.caret, c);
        self.caret += 1
    }
    fn apply(&mut self, key: &str) {
        if let Some(text) = key.strip_prefix("text:") {
            for c in text.chars() {
                self.insert(c)
            }
            return;
        }
        match key {
            "left" => self.caret = self.caret.saturating_sub(1),
            "right" => self.caret = (self.caret + 1).min(self.buffer.len()),
            "home" => self.caret = 0,
            "end" => self.caret = self.buffer.len(),
            "backspace" => {
                if self.caret > 0 {
                    self.caret -= 1;
                    self.buffer.remove(self.caret);
                }
            }
            "delete" => {
                if self.caret < self.buffer.len() {
                    self.buffer.remove(self.caret);
                }
            }
            "word-left" => {
                while self.caret > 0 && self.buffer[self.caret - 1].is_whitespace() {
                    self.caret -= 1
                }
                while self.caret > 0 && !self.buffer[self.caret - 1].is_whitespace() {
                    self.caret -= 1
                }
            }
            "word-right" => {
                while self.caret < self.buffer.len() && !self.buffer[self.caret].is_whitespace() {
                    self.caret += 1
                }
                while self.caret < self.buffer.len() && self.buffer[self.caret].is_whitespace() {
                    self.caret += 1
                }
            }
            "kill-end" => self.kill = self.buffer.drain(self.caret..).collect(),
            "kill-start" => {
                self.kill = self.buffer.drain(..self.caret).collect();
                self.caret = 0
            }
            "kill-word" => {
                let end = self.caret;
                self.apply("word-left");
                self.kill = self.buffer.drain(self.caret..end).collect()
            }
            "yank" => {
                for c in self.kill.clone() {
                    self.insert(c)
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_guard_restores_termios_when_stderr_has_no_reader() {
        const CHILD: &str = "REAGENT_TEST_RAW_STDERR";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "terminal::tests::raw_guard_restores_termios_when_stderr_has_no_reader",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (mut master, mut slave) = (-1, -1);
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let mut before = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(unsafe { libc::tcgetattr(slave, &mut before) }, 0);
        let (reader, writer) = io::pipe().unwrap();
        drop(reader);
        assert_eq!(unsafe { libc::dup2(writer.as_raw_fd(), 2) }, 2);
        let guard = Raw::enter_fd(slave).unwrap();
        let mut during = before;
        assert_eq!(unsafe { libc::tcgetattr(slave, &mut during) }, 0);
        assert_eq!(during.c_lflag & libc::ICANON, 0);
        drop(guard);
        let mut after = before;
        assert_eq!(unsafe { libc::tcgetattr(slave, &mut after) }, 0);
        // Restoring termios may mark pending input for reprocessing on Darwin.
        assert_eq!(
            after.c_lflag & !libc::PENDIN,
            before.c_lflag & !libc::PENDIN
        );
        assert_eq!(after.c_iflag, before.c_iflag);
        assert_eq!(after.c_oflag, before.c_oflag);
        assert_eq!(after.c_cc, before.c_cc);
        unsafe {
            libc::close(slave);
            libc::close(master);
        }
    }

    #[test]
    fn editing_unicode_and_kill() {
        let mut e = Editor::default();
        e.replace("a 界b");
        e.apply("word-left");
        e.apply("kill-end");
        assert_eq!(e.value(), "a ");
        e.apply("yank");
        assert_eq!(e.value(), "a 界b");
        e.apply("backspace");
        assert_eq!(e.value(), "a 界");
    }
}

/// Choice is one selectable terminal row.
pub struct Choice {
    pub label: String,
    pub detail: String,
    pub note: String,
    pub disabled: bool,
}
/// Selection separates a choice from cancellation and pipe fallback.
pub enum Selection {
    Chosen(usize),
    Cancelled,
    NotInteractive,
}
