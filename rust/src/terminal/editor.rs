use crate::render::{rune_width, sanitize, width};
use std::collections::VecDeque;

/// Key preserves literal input separately from terminal editing commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Key {
    Name(&'static str),
    Text(char),
}

/// Decoder retains escape and UTF-8 fragments between terminal reads.
#[derive(Default)]
pub(super) struct Decoder {
    pending: VecDeque<u8>,
    paste: bool,
    last_cr: bool,
}

impl Decoder {
    /// Feed appends bytes without discarding an incomplete key.
    pub(super) fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend(bytes);
    }

    /// Escape resolves only a lone ESC after the caller's idle timeout.
    pub(super) fn escape(&mut self) -> Option<Key> {
        if self.pending.len() == 1 && self.pending.front() == Some(&27) {
            self.pending.pop_front();
            self.last_cr = false;
            return Some(Key::Name("escape"));
        }
        None
    }

    /// Next returns one complete key, retaining partial CSI and UTF-8 input.
    pub(super) fn next(&mut self) -> Option<Key> {
        loop {
            let first = *self.pending.front()?;
            if first == 27 {
                let second = *self.pending.get(1)?;
                let (count, name) = if matches!(second, b'[' | b'O') {
                    let end = (2..self.pending.len())
                        .find(|i| (0x40..=0x7e).contains(&self.pending[*i]))?;
                    let seq = &self.pending.make_contiguous()[..=end];
                    let name = match seq {
                        b"\x1b[A" | b"\x1bOA" => "up",
                        b"\x1b[B" | b"\x1bOB" => "down",
                        b"\x1b[C" | b"\x1bOC" => "right",
                        b"\x1b[D" | b"\x1bOD" => "left",
                        b"\x1b[H" | b"\x1bOH" | b"\x1b[1~" => "home",
                        b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" => "end",
                        b"\x1b[3~" => "delete",
                        b"\x1b[1;3D" => "word-left",
                        b"\x1b[1;3C" => "word-right",
                        b"\x1b[200~" => "paste-start",
                        b"\x1b[201~" => "paste-end",
                        _ => "unknown",
                    };
                    (end + 1, name)
                } else {
                    match second {
                        b'b' | b'B' => (2, "word-left"),
                        b'f' | b'F' => (2, "word-right"),
                        _ => (1, "escape"),
                    }
                };
                self.pending.drain(..count);
                self.last_cr = false;
                match name {
                    "paste-start" => self.paste = true,
                    "paste-end" => self.paste = false,
                    _ => {}
                }
                return Some(Key::Name(name));
            }

            let count = match first {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 1,
            };
            let available = count.min(self.pending.len());
            let (c, consumed) =
                match std::str::from_utf8(&self.pending.make_contiguous()[..available]) {
                    Ok(text) => (text.chars().next()?, count),
                    Err(error) if error.error_len().is_none() => return None,
                    // Match Go's DecodeRune: each invalid byte produces one RuneError.
                    Err(_) => ('\u{fffd}', 1),
                };
            self.pending.drain(..consumed);
            let skip_lf = c == '\n' && self.last_cr;
            self.last_cr = c == '\r';
            if skip_lf {
                continue;
            }
            if self.paste && matches!(c, '\r' | '\n' | '\t') {
                return Some(Key::Text(if c == '\r' { '\n' } else { c }));
            }
            let name = match c {
                '\r' | '\n' => "enter",
                '\t' => "tab",
                '\x03' => "interrupt",
                '\x04' => "eof",
                '\x08' | '\x7f' => "backspace",
                '\x01' => "home",
                '\x05' => "end",
                '\x0b' => "kill-end",
                '\x15' => "kill-start",
                '\x17' => "kill-word",
                '\x19' => "yank",
                '\x0c' => "clear",
                _ => "",
            };
            if !name.is_empty() {
                return Some(Key::Name(name));
            }
            if c >= ' ' && c != '\x7f' {
                return Some(Key::Text(c));
            }
        }
    }
}

/// Editor owns one physical submission; the input reader owns completed history.
#[derive(Default)]
pub(super) struct Editor {
    pub(super) buffer: Vec<char>,
    pub(super) caret: usize,
    kill: Vec<char>,
    history_index: usize,
    draft: Vec<char>,
    preferred_col: Option<usize>,
}

impl Editor {
    /// Value returns the literal submission rather than its display markers.
    pub(super) fn value(&self) -> String {
        self.buffer.iter().collect()
    }

    /// Replace sets the text and places the caret at its end.
    pub(super) fn replace(&mut self, text: &str) {
        self.buffer = text.chars().collect();
        self.caret = self.buffer.len();
    }

    /// Apply edits one key, returning actions that belong to the reader.
    pub(super) fn apply(
        &mut self,
        key: &Key,
        history: &[String],
        width: usize,
        prompt: &str,
    ) -> Option<&'static str> {
        if !matches!(key, Key::Name("up" | "down")) {
            self.preferred_col = None;
        }
        let name = match *key {
            Key::Text(c) => {
                self.buffer.insert(self.caret, c);
                self.caret += 1;
                return None;
            }
            Key::Name(name) => name,
        };
        match name {
            "enter" | "interrupt" | "clear" => return Some(name),
            "eof" if self.buffer.is_empty() => return Some("eof"),
            "backspace" if self.caret > 0 => {
                self.caret -= 1;
                self.buffer.remove(self.caret);
            }
            "delete" if self.caret < self.buffer.len() => {
                self.buffer.remove(self.caret);
            }
            "left" => self.caret = self.caret.saturating_sub(1),
            "right" => self.caret = (self.caret + 1).min(self.buffer.len()),
            "home" => self.caret = 0,
            "end" => self.caret = self.buffer.len(),
            "word-left" => {
                while self.caret > 0 && self.buffer[self.caret - 1].is_whitespace() {
                    self.caret -= 1;
                }
                while self.caret > 0 && !self.buffer[self.caret - 1].is_whitespace() {
                    self.caret -= 1;
                }
            }
            "word-right" => {
                while self.caret < self.buffer.len() && !self.buffer[self.caret].is_whitespace() {
                    self.caret += 1;
                }
                while self.caret < self.buffer.len() && self.buffer[self.caret].is_whitespace() {
                    self.caret += 1;
                }
            }
            "kill-end" => self.kill = self.buffer.drain(self.caret..).collect(),
            "kill-start" => {
                self.kill = self.buffer.drain(..self.caret).collect();
                self.caret = 0;
            }
            "kill-word" => {
                let end = self.caret;
                self.apply(&Key::Name("word-left"), history, width, prompt);
                self.kill = self.buffer.drain(self.caret..end).collect();
            }
            "yank" => {
                self.buffer
                    .splice(self.caret..self.caret, self.kill.iter().copied());
                self.caret += self.kill.len();
            }
            "up" => {
                if self.move_row(-1, width, prompt) {
                    return None;
                }
                if self.history_index < history.len() {
                    if self.history_index == 0 {
                        self.draft = self.buffer.clone();
                    }
                    self.history_index += 1;
                    self.replace(&history[history.len() - self.history_index]);
                }
            }
            "down" => {
                if self.move_row(1, width, prompt) {
                    return None;
                }
                if self.history_index > 0 {
                    self.history_index -= 1;
                    if self.history_index == 0 {
                        self.buffer = self.draft.clone();
                        self.caret = self.buffer.len();
                    } else {
                        self.replace(&history[history.len() - self.history_index]);
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn move_row(&mut self, delta: isize, width: usize, prompt: &str) -> bool {
        if width == 0 {
            return false;
        }
        let (rows, row, col) = layout(&self.buffer, self.caret, prompt, width);
        let target = row as isize + delta;
        if target < 0 || target as usize >= rows.len() {
            return false;
        }
        let preferred = *self.preferred_col.get_or_insert(col);
        let mut best = self.caret;
        let mut distance = usize::MAX;
        for i in 0..=self.buffer.len() {
            let (_, row, col) = layout(&self.buffer, i, prompt, width);
            let d = col.abs_diff(preferred);
            if row == target as usize && d < distance {
                best = i;
                distance = d;
            }
        }
        self.caret = best;
        true
    }
}

/// TurnInput retains an editable draft and at most one submission during a run.
#[derive(Default)]
pub(super) struct TurnInput {
    pub(super) editor: Editor,
    pub(super) prefix: String,
    pub(super) queued: Option<String>,
    pub(super) exit: bool,
}

impl TurnInput {
    /// Apply freezes queued text while still accepting cancellation.
    pub(super) fn apply(
        &mut self,
        key: &Key,
        history: &mut Vec<String>,
        columns: usize,
        prompt: &str,
    ) -> Option<&'static str> {
        if *key == Key::Name("interrupt") {
            return Some("interrupt");
        }
        if self.queued.is_some() || self.exit {
            return None;
        }
        match self.editor.apply(key, history, columns, prompt) {
            Some("enter") => {
                let line = self.editor.value();
                if !line.trim().is_empty() && history.last() != Some(&line) {
                    history.push(line.clone());
                    if history.len() > 500 {
                        history.remove(0);
                    }
                }
                if line.ends_with('\\') && !line.ends_with("\\\\") {
                    self.prefix.push_str(&line[..line.len() - 1]);
                    self.prefix.push('\n');
                    self.editor.replace("");
                } else if !(self.prefix.is_empty() && line.is_empty()) {
                    let line = format!("{}{line}", self.prefix);
                    self.editor.replace(&line);
                    self.queued = Some(line);
                }
                None
            }
            Some("eof") => {
                self.exit = true;
                None
            }
            action => action,
        }
    }
}

fn visible(text: &str) -> String {
    sanitize(text).replace('\n', "↵").replace('\t', "⇥")
}

/// Layout reserves the last cell to avoid the terminal's implicit line wrap.
pub(super) fn layout(
    buffer: &[char],
    caret: usize,
    prompt: &str,
    columns: usize,
) -> (Vec<String>, usize, usize) {
    let columns = columns.max(2);
    let mut col = width(prompt);
    let mut rows = vec![prompt.to_string()];
    let mut row = 0;
    let hang = if columns <= 4 { "" } else { "  " };
    if col >= columns {
        rows.push(hang.into());
        row += 1;
        col = hang.len();
    }
    let (mut caret_row, mut caret_col) = (row, col);
    for (i, c) in buffer.iter().enumerate() {
        if i == caret {
            (caret_row, caret_col) = (row, col);
        }
        for c in visible(&c.to_string()).chars() {
            let cells = rune_width(c);
            if col + cells >= columns {
                rows.push(hang.into());
                row += 1;
                col = hang.len();
            }
            rows[row].push(c);
            col += cells;
        }
    }
    if caret == buffer.len() {
        (caret_row, caret_col) = (row, col);
    }
    (rows, caret_row, caret_col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(decoder: &mut Decoder) -> Vec<Key> {
        std::iter::from_fn(|| decoder.next()).collect()
    }

    #[test]
    fn decoder_retains_every_split_sequence() {
        for (input, expected) in [
            ("\x1b[1;3D", Key::Name("word-left")),
            ("\x1bOA", Key::Name("up")),
            ("\x1b[200~", Key::Name("paste-start")),
            ("\x1b[99~", Key::Name("unknown")),
            ("\x1bb", Key::Name("word-left")),
            ("界", Key::Text('界')),
            ("🙂", Key::Text('🙂')),
        ] {
            for split in 1..input.len() {
                let mut decoder = Decoder::default();
                decoder.feed(&input.as_bytes()[..split]);
                assert_eq!(decoder.next(), None, "{input:?} at {split}");
                if split != 1 || input.as_bytes()[0] != 27 {
                    assert_eq!(decoder.escape(), None);
                }
                decoder.feed(&input.as_bytes()[split..]);
                assert_eq!(drain(&mut decoder), vec![expected]);
            }
        }
    }

    #[test]
    fn decoder_lone_escape_does_not_discard_partial_csi_or_utf8() {
        let mut decoder = Decoder::default();
        decoder.feed(b"\x1b");
        assert_eq!(decoder.next(), None);
        assert_eq!(decoder.escape(), Some(Key::Name("escape")));
        decoder.feed(b"\x1b[1;");
        assert_eq!(decoder.next(), None);
        assert_eq!(decoder.escape(), None);
        decoder.feed(b"3C\xe7");
        assert_eq!(decoder.next(), Some(Key::Name("word-right")));
        assert_eq!(decoder.next(), None);
        assert_eq!(decoder.escape(), None);
        decoder.feed(b"\x95\x8c");
        assert_eq!(decoder.next(), Some(Key::Text('界')));
    }

    #[test]
    fn decoder_normalizes_split_crlf_inside_and_outside_paste() {
        let mut decoder = Decoder::default();
        let mut keys = Vec::new();
        for byte in b"a\r\nb\n\x1b[200~c\r\nd\re\nf\t\x1b[201~\r\n" {
            decoder.feed(&[*byte]);
            keys.extend(drain(&mut decoder));
        }
        assert_eq!(
            keys,
            vec![
                Key::Text('a'),
                Key::Name("enter"),
                Key::Text('b'),
                Key::Name("enter"),
                Key::Name("paste-start"),
                Key::Text('c'),
                Key::Text('\n'),
                Key::Text('d'),
                Key::Text('\n'),
                Key::Text('e'),
                Key::Text('\n'),
                Key::Text('f'),
                Key::Text('\t'),
                Key::Name("paste-end"),
                Key::Name("enter"),
            ]
        );
    }

    #[test]
    fn decoder_invalid_bytes_do_not_swallow_following_input() {
        let mut decoder = Decoder::default();
        decoder.feed(b"\xff\xc0\xaf\xe2\x82!");
        assert_eq!(
            drain(&mut decoder),
            vec![
                Key::Text('\u{fffd}'),
                Key::Text('\u{fffd}'),
                Key::Text('\u{fffd}'),
                Key::Text('\u{fffd}'),
                Key::Text('\u{fffd}'),
                Key::Text('!'),
            ]
        );
        decoder.feed(b"\xf0\x9f");
        assert_eq!(decoder.next(), None);
        decoder.feed(b"\x99\x82");
        assert_eq!(decoder.next(), Some(Key::Text('🙂')));
    }

    #[test]
    fn editor_keys_preserve_unicode_and_kill_buffer() {
        for (input, expected, caret) in [
            ("ab\x1b[D中", "a中b", 2),
            ("abc\x1b[D\x1b[3~", "ab", 2),
            ("abc\x01Z\x05!", "Zabc!", 5),
            ("one two\x1bbZ\x1bf!", "one Ztwo!", 9),
            ("abc\x01\x1b[C\x0b\x19", "abc", 3),
            ("abc\x15\x19", "abc", 3),
            ("one 界\x17\x19", "one 界", 5),
            ("a界b\x7f", "a界", 2),
            ("ab\x1b[D\x0c", "ab", 1),
        ] {
            let mut decoder = Decoder::default();
            decoder.feed(input.as_bytes());
            let mut editor = Editor::default();
            for key in drain(&mut decoder) {
                editor.apply(&key, &[], 80, "> ");
            }
            assert_eq!((editor.value(), editor.caret), (expected.into(), caret));
        }
    }

    #[test]
    fn turn_queues_only_once_but_always_accepts_interrupt() {
        let mut turn = TurnInput::default();
        let mut history = Vec::new();
        let mut decoder = Decoder::default();
        decoder.feed(b"draft\x15second\\\rline\rignored\x04\x03");
        let mut actions = Vec::new();
        for key in drain(&mut decoder) {
            if let Some(action) = turn.apply(&key, &mut history, 80, "❯ ") {
                actions.push(action);
            }
        }
        assert_eq!(turn.queued.as_deref(), Some("second\nline"));
        assert_eq!(turn.editor.value(), "second\nline");
        assert_eq!(history, vec!["second\\", "line"]);
        assert_eq!(actions, vec!["interrupt"]);
        assert!(!turn.exit);
    }

    #[test]
    fn turn_preserves_pasted_draft_and_eof_does_not_submit_it() {
        let mut turn = TurnInput::default();
        let mut history = Vec::new();
        let mut decoder = Decoder::default();
        decoder.feed(b"\x1b[200~line one\nline two\t\x1b[201~\x04");
        for key in drain(&mut decoder) {
            turn.apply(&key, &mut history, 20, "❯ ");
        }
        assert_eq!(turn.editor.value(), "line one\nline two\t");
        assert!(turn.queued.is_none());
        assert!(!turn.exit);
        turn.editor.replace("");
        turn.apply(&Key::Name("eof"), &mut history, 80, "❯ ");
        assert!(turn.exit);
    }

    #[test]
    fn editor_returns_reader_actions_without_mutating_text() {
        let mut editor = Editor::default();
        assert_eq!(editor.apply(&Key::Name("eof"), &[], 80, "> "), Some("eof"));
        editor.replace("draft");
        assert_eq!(editor.apply(&Key::Name("eof"), &[], 80, "> "), None);
        for name in ["enter", "interrupt", "clear"] {
            assert_eq!(editor.apply(&Key::Name(name), &[], 80, "> "), Some(name));
            assert_eq!(editor.value(), "draft");
        }
    }

    #[test]
    fn editor_history_restores_draft_and_moves_rows_first() {
        let history = vec!["oldest".into(), "older".into()];
        let mut editor = Editor::default();
        editor.replace("abcdefghij");
        editor.caret = 9;
        editor.apply(&Key::Name("up"), &history, 8, "> ");
        assert_eq!((editor.caret, editor.history_index), (4, 0));
        editor.apply(&Key::Name("down"), &history, 8, "> ");
        assert_eq!((editor.caret, editor.history_index), (9, 0));
        editor.apply(&Key::Name("up"), &history, 8, "> ");
        editor.apply(&Key::Name("up"), &history, 8, "> ");
        assert_eq!((editor.value(), editor.history_index), ("older".into(), 1));
        editor.apply(&Key::Name("up"), &history, 80, "> ");
        assert_eq!(editor.value(), "oldest");
        editor.apply(&Key::Name("down"), &history, 80, "> ");
        assert_eq!(editor.value(), "older");
        editor.apply(&Key::Name("down"), &history, 80, "> ");
        assert_eq!(editor.value(), "abcdefghij");
    }

    #[test]
    fn layout_wraps_by_cells_and_keeps_controls_visible() {
        for (input, prompt, columns, caret, expected, row, col) in [
            ("abc", "> ", 10, 2, vec!["> abc"], 0, 4),
            (
                "a",
                "\x1b[1;38;5;86m❯\x1b[0m ",
                20,
                1,
                vec!["\x1b[1;38;5;86m❯\x1b[0m a"],
                0,
                3,
            ),
            ("abcd", "> ", 6, 4, vec!["> abc", "  d"], 1, 3),
            ("中🙂e\u{301}", "> ", 8, 2, vec!["> 中🙂e\u{301}"], 0, 6),
            (
                "aaaaaaaaaa",
                "> ",
                6,
                10,
                vec!["> aaa", "  aaa", "  aaa", "  a"],
                3,
                3,
            ),
            ("a\nb\t", "> ", 8, 4, vec!["> a↵b⇥"], 0, 6),
            ("\x1b[31m", "> ", 20, 5, vec!["> \\x1b[31m"], 0, 10),
        ] {
            let buffer: Vec<_> = input.chars().collect();
            let (rows, actual_row, actual_col) = layout(&buffer, caret, prompt, columns);
            assert_eq!(rows, expected);
            assert_eq!((actual_row, actual_col), (row, col));
        }
    }
}
