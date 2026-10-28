use crate::render::{columns, display, width};
use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::os::fd::AsRawFd;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
mod editor;
mod region;
use editor::{Decoder, Editor as IdleEditor, Key, TurnInput};
pub(crate) use region::output;
pub use region::{Region, RegionHandle};

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
            let mut raw = old;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
            eprint!("\x1b[?2004h");
            Ok(Self { fd, previous: old })
        }
    }
}
impl Drop for Raw {
    fn drop(&mut self) {
        eprint!("\x1b[?2004l");
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.previous);
        }
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
        }
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
        if let Some(region) = &self.region {
            region.lock().unwrap().collapse()?;
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
        let mut header = format!("{}  ↑↓ move · enter · esc", crate::render::sanitize(title));
        if width(&header) > columns - 1 {
            header = format!("↑↓ move · enter · esc  {}", crate::render::sanitize(title))
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
                let key = read_key(&mut self.reader)?;
                match key.as_str() {
                    "up" | "text:k" => {
                        if let Some(next) = (0..cursor).rev().find(|i| !options[*i].disabled) {
                            cursor = next
                        }
                    }
                    "down" | "text:j" => {
                        if let Some(next) =
                            (cursor + 1..options.len()).find(|i| !options[*i].disabled)
                        {
                            cursor = next
                        }
                    }
                    "enter" => return Ok(Selection::Chosen(cursor)),
                    "escape" | "interrupt" | "eof" | "text:q" => return Ok(Selection::Cancelled),
                    _ => {
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
        let mut paste = false;
        let mut history = 0;
        let mut draft = String::new();
        eprint!("{prompt}");
        io::stderr().flush()?;
        loop {
            let key = read_key(&mut self.reader)?;
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
                "paste-start" => paste = true,
                "paste-end" => paste = false,
                "enter" if paste => editor.insert('\n'),
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
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 5) } <= 0 {
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
        if self.region.is_none() {
            return Ok(());
        }
        let mut next = std::mem::take(&mut self.next);
        let result = self.read_turn(&mut next, stop, cancelled);
        if cancelled.load(Ordering::Acquire) {
            next = TurnInput::default();
            self.decoder = Decoder::default();
        }
        let handle = self.region.as_ref().unwrap().clone();
        {
            let mut region = handle.lock().unwrap();
            region.queued = next.queued.is_some();
            let (columns, height) = self.size()?;
            region.input(
                &next.editor.buffer,
                next.editor.caret,
                &next.prefix,
                columns,
                height,
            )?;
        }
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
            let key = if let Some(key) = self.decoder.next() {
                Some(key)
            } else {
                let fd = self.tty.as_ref().unwrap().as_raw_fd();
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut poll, 1, 50) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
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
            if stop.load(Ordering::Acquire) || cancelled.load(Ordering::Acquire) {
                break;
            }
            let mut changed = region.tick_progress();
            if let Some(key) = key {
                let prompt = region.prompt();
                match next.apply(&key, &mut self.history, columns, &prompt) {
                    Some("interrupt") => cancelled.store(true, Ordering::Release),
                    Some("clear") => region.clear()?,
                    _ => {}
                }
                region.queued = next.queued.is_some();
                changed = true;
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
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut poll, 1, 50) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
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
fn read_key(reader: &mut io::BufReader<io::Stdin>) -> io::Result<String> {
    let mut byte = [0];
    if reader.read(&mut byte)? == 0 {
        return Ok("eof".into());
    }
    let b = byte[0];
    if b == 27 {
        let mut seq = vec![b];
        let mut poll = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        if reader.buffer().is_empty() && unsafe { libc::poll(&mut poll, 1, 20) } <= 0 {
            return Ok("escape".into());
        }
        reader.read_exact(&mut byte)?;
        seq.push(byte[0]);
        if matches!(byte[0], b'[' | b'O') {
            loop {
                reader.read_exact(&mut byte)?;
                seq.push(byte[0]);
                if (0x40..=0x7e).contains(&byte[0]) {
                    break;
                }
            }
        }
        return Ok(match seq.as_slice() {
            b"\x1b[A" | b"\x1bOA" => "up",
            b"\x1b[B" | b"\x1bOB" => "down",
            b"\x1b[C" | b"\x1bOC" => "right",
            b"\x1b[D" | b"\x1bOD" => "left",
            b"\x1b[H" | b"\x1bOH" | b"\x1b[1~" => "home",
            b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" => "end",
            b"\x1b[3~" => "delete",
            b"\x1b[200~" => "paste-start",
            b"\x1b[201~" => "paste-end",
            b"\x1bb" | b"\x1bB" | b"\x1b[1;3D" => "word-left",
            b"\x1bf" | b"\x1bF" | b"\x1b[1;3C" => "word-right",
            _ => "unknown",
        }
        .into());
    }
    let name = match b {
        b'\r' | b'\n' => Some("enter"),
        b'\t' => Some("tab"),
        3 => Some("interrupt"),
        4 => Some("eof"),
        8 | 127 => Some("backspace"),
        1 => Some("home"),
        5 => Some("end"),
        11 => Some("kill-end"),
        21 => Some("kill-start"),
        23 => Some("kill-word"),
        25 => Some("yank"),
        12 => Some("clear"),
        _ => None,
    };
    if let Some(name) = name {
        return Ok(name.into());
    }
    if b < 32 {
        return Ok("unknown".into());
    }
    let mut bytes = vec![b];
    let len = if b < 128 {
        1
    } else if b < 224 {
        2
    } else if b < 240 {
        3
    } else {
        4
    };
    for _ in 1..len {
        reader.read_exact(&mut byte)?;
        bytes.push(byte[0]);
    }
    Ok(format!("text:{}", String::from_utf8_lossy(&bytes)))
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
