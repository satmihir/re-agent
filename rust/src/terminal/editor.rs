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
                        b"\x1b[1;3D" | b"\x1b[1;5D" => "word-left",
                        b"\x1b[1;3C" | b"\x1b[1;5C" => "word-right",
                        b"\x1b[200~" => "paste-start",
                        b"\x1b[201~" => "paste-end",
                        _ => "unknown",
                    };
                    (end + 1, name)
                } else {
                    match second {
                        b'b' | b'B' => (2, "word-left"),
                        b'f' | b'F' => (2, "word-right"),
                        b'd' | b'D' => (2, "kill-word-forward"),
                        8 | 127 => (2, "kill-word"),
                        b'\r' => (2, "newline"),
                        _ => (1, "escape"),
                    }
                };
                self.pending.drain(..count);
                self.last_cr = name == "newline";
                match name {
                    "paste-start" => self.paste = true,
                    "paste-end" => self.paste = false,
                    _ => {}
                }
                return Some(if name == "newline" {
                    Key::Text('\n')
                } else {
                    Key::Name(name)
                });
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
            if c == '\n' || self.paste && matches!(c, '\r' | '\t') {
                return Some(Key::Text(if c == '\r' { '\n' } else { c }));
            }
            let name = match c {
                '\r' => "enter",
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

    /// Forget history leaves the draft intact when the workspace's history changes.
    pub(super) fn forget_history(&mut self) {
        self.history_index = 0;
        self.draft.clear();
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
            "delete" | "eof" if self.caret < self.buffer.len() => {
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
            "kill-word-forward" => {
                let start = self.caret;
                let mut end = start;
                while end < self.buffer.len() && self.buffer[end].is_whitespace() {
                    end += 1;
                }
                while end < self.buffer.len() && !self.buffer[end].is_whitespace() {
                    end += 1;
                }
                self.kill = self.buffer.drain(start..end).collect();
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
    /// Apply permits queued recall and cancellation without recording undispatched text.
    pub(super) fn apply(
        &mut self,
        key: &Key,
        history: &[String],
        columns: usize,
        prompt: &str,
    ) -> Option<&'static str> {
        if matches!(key, Key::Name("interrupt" | "escape")) {
            return Some("interrupt");
        }
        if self.exit {
            return None;
        }
        if self.queued.is_some() {
            if matches!(key, Key::Name("up" | "backspace")) {
                self.editor.replace(&self.queued.take().unwrap());
                self.editor.preferred_col = None;
            }
            return None;
        }
        match self.editor.apply(key, history, columns, prompt) {
            Some("enter") => {
                let line = self.editor.value();
                if line.ends_with('\\') && !line.ends_with("\\\\") {
                    self.prefix.push_str(&line[..line.len() - 1]);
                    self.prefix.push('\n');
                    self.editor.replace("");
                } else if !(self.prefix.is_empty() && line.is_empty()) {
                    let line = format!("{}{line}", self.prefix);
                    self.prefix.clear();
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
    sanitize(text).replace('\t', "⇥")
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
        if *c == '\n' {
            rows.push(hang.into());
            row += 1;
            col = hang.len();
            continue;
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
            ("\x1b[1;3C", Key::Name("word-right")),
            ("\x1b[1;5D", Key::Name("word-left")),
            ("\x1b[1;5C", Key::Name("word-right")),
            ("\x1b\x7f", Key::Name("kill-word")),
            ("\x1b\x08", Key::Name("kill-word")),
            ("\x1bd", Key::Name("kill-word-forward")),
            ("\x1b\r", Key::Text('\n')),
            ("\n", Key::Text('\n')),
            ("\x04", Key::Name("eof")),
            ("\x1bOA", Key::Name("up")),
            ("\x1b[200~", Key::Name("paste-start")),
            ("\x1b[99~", Key::Name("unknown")),
            ("\x1bb", Key::Name("word-left")),
            ("界", Key::Text('界')),
            ("🙂", Key::Text('🙂')),
        ] {
            for split in 0..input.len() {
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
                Key::Text('\n'),
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
    fn replacing_workspace_history_preserves_the_draft_without_stale_indices() {
        let mut editor = Editor::default();
        let history = vec!["oldest".into(), "latest".into()];
        editor.apply(&Key::Name("up"), &history, 80, "> ");
        editor.apply(&Key::Name("up"), &history, 80, "> ");
        assert_eq!(editor.value(), "oldest");
        editor.forget_history();
        editor.apply(&Key::Name("down"), &[], 80, "> ");
        assert_eq!(editor.value(), "oldest");
        let history = vec!["other workspace".into()];
        editor.apply(&Key::Name("up"), &history, 80, "> ");
        assert_eq!(editor.value(), "other workspace");
        editor.apply(&Key::Name("down"), &history, 80, "> ");
        assert_eq!(editor.value(), "oldest");
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
    fn word_editing_and_forward_delete_preserve_unicode_at_boundaries() {
        for (input, expected, caret) in [
            ("one 界🙂\x1b[1;5DZ\x1b[1;5C!", "one Z界🙂!", 8),
            ("one 界🙂  \x1b\x7f", "one ", 4),
            ("one 界🙂\x1b\x08\x19", "one 界🙂", 6),
            ("\x1b\x7f\x1bd\x04", "", 0),
            ("界🙂\x01\x04", "🙂", 0),
            ("界🙂\x04", "界🙂", 2),
            ("界🙂\x01\x1bd\x19", "界🙂", 2),
            ("one 界🙂\x01\x1bd", " 界🙂", 0),
            ("  界🙂 next\x01\x1bd", " next", 0),
            ("界🙂\x1bd", "界🙂", 2),
            ("界🙂\x01\x1b\x7f", "界🙂", 0),
            ("one\n界🙂\x1b\x7f\x19", "one\n界🙂", 6),
        ] {
            for split in 0..=input.len() {
                let mut decoder = Decoder::default();
                let mut editor = Editor::default();
                for bytes in [&input.as_bytes()[..split], &input.as_bytes()[split..]] {
                    decoder.feed(bytes);
                    for key in drain(&mut decoder) {
                        editor.apply(&key, &[], 80, "> ");
                    }
                }
                assert_eq!(
                    (editor.value(), editor.caret),
                    (expected.into(), caret),
                    "{input:?} at {split}"
                );
            }
        }
    }

    #[test]
    fn literal_newlines_can_be_edited_on_the_previous_row_before_submission() {
        for newline in ["\x1b\r", "\n", "\x1b[200~\r\n\x1b[201~"] {
            let input = format!("one{newline}two\x1b[A\x7f\r\n");
            for split in 0..=input.len() {
                let mut decoder = Decoder::default();
                let mut turn = TurnInput::default();
                let history = vec!["not this history entry".into()];
                for bytes in [&input.as_bytes()[..split], &input.as_bytes()[split..]] {
                    decoder.feed(bytes);
                    for key in drain(&mut decoder) {
                        assert_eq!(turn.apply(&key, &history, 80, "> "), None);
                    }
                }
                assert_eq!(turn.queued.as_deref(), Some("on\ntwo"));
                assert_eq!(turn.editor.value(), "on\ntwo");
                assert_eq!(turn.editor.history_index, 0);
            }
        }
    }

    #[test]
    fn alt_enter_collapses_a_following_lf_without_submitting() {
        let mut decoder = Decoder::default();
        let mut keys = Vec::new();
        for byte in b"\x1b\r\n\n\r\n" {
            decoder.feed(&[*byte]);
            keys.extend(drain(&mut decoder));
        }
        assert_eq!(
            keys,
            vec![Key::Text('\n'), Key::Text('\n'), Key::Name("enter")]
        );
    }

    #[test]
    fn queued_recall_restores_the_whole_message_without_applying_the_recall_key() {
        for recall in [Key::Name("up"), Key::Name("backspace")] {
            let history = vec!["older".into()];
            let mut turn = TurnInput::default();
            let mut decoder = Decoder::default();
            decoder.feed("one\\\r界 two\r".as_bytes());
            for key in drain(&mut decoder) {
                turn.apply(&key, &history, 80, "> ");
            }
            assert_eq!(turn.queued.as_deref(), Some("one\n界 two"));
            assert!(turn.prefix.is_empty());
            for key in [
                Key::Text('!'),
                Key::Name("delete"),
                Key::Name("left"),
                Key::Name("down"),
                Key::Name("eof"),
                Key::Name("enter"),
            ] {
                assert_eq!(turn.apply(&key, &history, 80, "> "), None);
                assert_eq!(turn.queued.as_deref(), Some("one\n界 two"));
                assert_eq!(turn.editor.value(), "one\n界 two");
            }
            for _ in 0..2 {
                assert_eq!(turn.apply(&recall, &history, 80, "> "), None);
                assert!(turn.queued.is_none());
                assert_eq!(turn.editor.value(), "one\n界 two");
                assert_eq!(turn.editor.caret, turn.editor.buffer.len());
                assert_eq!(turn.editor.history_index, 0);
                assert_eq!(turn.apply(&Key::Name("enter"), &history, 80, "> "), None);
                assert_eq!(turn.queued.as_deref(), Some("one\n界 two"));
                assert!(turn.prefix.is_empty());
            }
            turn.apply(&recall, &history, 80, "> ");
            turn.apply(&Key::Name("backspace"), &history, 80, "> ");
            turn.apply(&Key::Text('!'), &history, 80, "> ");
            turn.apply(&Key::Name("enter"), &history, 80, "> ");
            assert_eq!(turn.queued.as_deref(), Some("one\n界 tw!"));
            assert_eq!(history, vec!["older"]);
            assert!(!turn.exit);
        }
    }

    #[test]
    fn turn_queues_only_once_but_always_accepts_interrupt() {
        let mut turn = TurnInput::default();
        let history = Vec::new();
        let mut decoder = Decoder::default();
        decoder.feed(b"draft\x15second\\\rline\rignored\x04\x03");
        let mut actions = Vec::new();
        for key in drain(&mut decoder) {
            if let Some(action) = turn.apply(&key, &history, 80, "❯ ") {
                actions.push(action);
            }
        }
        assert_eq!(turn.queued.as_deref(), Some("second\nline"));
        assert_eq!(turn.editor.value(), "second\nline");
        assert!(turn.prefix.is_empty());
        assert!(history.is_empty());
        assert_eq!(actions, vec!["interrupt"]);
        assert!(!turn.exit);
    }

    #[test]
    fn escape_and_ctrl_c_interrupt_running_turns_without_losing_drafts_or_queued_text() {
        for queued in [false, true] {
            for bytes in [b"\x03".as_slice(), b"\x1b".as_slice()] {
                let mut turn = TurnInput::default();
                turn.editor.replace("draft");
                let history = Vec::new();
                if queued {
                    turn.apply(&Key::Name("enter"), &history, 80, "❯ ");
                }
                let mut decoder = Decoder::default();
                decoder.feed(bytes);
                let key = decoder.next().or_else(|| decoder.escape()).unwrap();
                assert_eq!(turn.apply(&key, &history, 80, "❯ "), Some("interrupt"));
                assert_eq!(turn.editor.value(), "draft");
                assert_eq!(turn.queued.as_deref(), queued.then_some("draft"));
                assert!(!turn.exit);
            }
        }
    }

    #[test]
    fn turn_preserves_pasted_draft_and_eof_does_not_submit_it() {
        let mut turn = TurnInput::default();
        let history = Vec::new();
        let mut decoder = Decoder::default();
        decoder.feed(b"\x1b[200~line one\nline two\t\x1b[201~\x04");
        for key in drain(&mut decoder) {
            turn.apply(&key, &history, 20, "❯ ");
        }
        assert_eq!(turn.editor.value(), "line one\nline two\t");
        assert!(turn.queued.is_none());
        assert!(!turn.exit);
        turn.editor.replace("");
        turn.apply(&Key::Name("eof"), &history, 80, "❯ ");
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
    fn physical_line_movement_keeps_the_preferred_cell_column_across_short_rows() {
        for (text, expected) in [
            ("abcd\nx\nabcd", [6, 4, 6, 11]),
            ("a界b\nx\na界b", [5, 3, 5, 9]),
            ("a\tb\nx\na\tb", [5, 3, 5, 9]),
        ] {
            let mut editor = Editor::default();
            editor.replace(text);
            for (key, caret) in ["up", "up", "down", "down"].into_iter().zip(expected) {
                editor.apply(&Key::Name(key), &["older".into()], 80, "> ");
                assert_eq!(editor.caret, caret, "{text:?} with {key}");
                assert_eq!(editor.history_index, 0);
                assert_eq!(editor.value(), text);
            }
        }
    }

    #[test]
    fn layout_wraps_by_cells_and_keeps_tabs_and_escapes_visible() {
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
            ("a\nb\t", "> ", 8, 4, vec!["> a", "  b⇥"], 1, 4),
            ("a\nb", "> ", 8, 1, vec!["> a", "  b"], 0, 3),
            ("a\nb", "> ", 8, 2, vec!["> a", "  b"], 1, 2),
            ("a\n\n", "> ", 8, 3, vec!["> a", "  ", "  "], 2, 2),
            ("\n", "> ", 4, 1, vec!["> ", ""], 1, 0),
            ("ab\n界\tz", "> ", 8, 4, vec!["> ab", "  界⇥z"], 1, 4),
            ("abcd\nxy", "> ", 6, 7, vec!["> abc", "  d", "  xy"], 2, 4),
            ("\x1b[31m", "> ", 20, 5, vec!["> \\x1b[31m"], 0, 10),
        ] {
            let buffer: Vec<_> = input.chars().collect();
            let (rows, actual_row, actual_col) = layout(&buffer, caret, prompt, columns);
            assert_eq!(rows, expected);
            assert_eq!((actual_row, actual_col), (row, col));
        }
    }
}
