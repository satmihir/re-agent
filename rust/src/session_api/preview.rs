use super::{error, ApiError, RunKind, RunRef, RunState, SessionApi};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// One attempt's preview stops growing here; the Run's result still carries the whole reply.
const MAX_PREVIEW_BYTES: usize = 1 << 20;

/// PreviewCursor identifies a position in one attempt's preview text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewCursor {
    /// attempt counts the Run's provider streams, starting at one.
    pub attempt: u64,
    /// offset is a byte position in that attempt's preview text.
    pub offset: u64,
}

/// PreviewPage carries speculative reply text published after a cursor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewPage {
    /// attempt identifies the stream this text belongs to; zero before the first stream.
    pub attempt: u64,
    /// restarted means a newer attempt began after the cursor: discard earlier preview text.
    pub restarted: bool,
    /// text is redacted visible model text after the cursor, never reasoning or tool arguments.
    pub text: String,
    /// next continues after the returned text.
    pub next: PreviewCursor,
    /// incomplete means this attempt's preview stopped early; its final reply arrives whole.
    pub incomplete: bool,
    /// finished means the Run is terminal and its result is authoritative.
    pub finished: bool,
}

/// ReplyPreview holds the running conversation Run's speculative reply text for API readers.
#[derive(Default)]
pub(crate) struct ReplyPreview {
    text: Mutex<Text>,
    changed: Condvar,
}

#[derive(Default)]
struct Text {
    attempt: u64,
    /// received is the current attempt's visible text, including a tail not yet published.
    received: String,
    /// published is the redacted text of received[..cut].
    published: String,
    cut: usize,
    incomplete: bool,
    finished: bool,
}

impl ReplyPreview {
    /// Begin starts a new provider stream whose text replaces the previous attempt's.
    pub(crate) fn begin(&self) {
        let mut text = self.text.lock().unwrap();
        let attempt = text.attempt + 1;
        *text = Text {
            attempt,
            ..Text::default()
        };
        self.changed.notify_all();
    }

    /// Append adds visible model text and publishes what can no longer be part of a credential.
    pub(crate) fn append(&self, delta: &str, secrets: &[String], endpoint: &str) {
        let mut text = self.text.lock().unwrap();
        if text.finished || text.incomplete || text.attempt == 0 {
            return;
        }
        if text.received.len().saturating_add(delta.len()) > MAX_PREVIEW_BYTES {
            text.incomplete = true;
            self.changed.notify_all();
            return;
        }
        text.received.push_str(delta);
        let patterns = crate::transport::redaction_patterns(secrets, endpoint);
        let cut = safe_cut(&text.received, &patterns).max(text.cut);
        if cut > text.cut {
            let segment =
                crate::transport::redact(&text.received[text.cut..cut], secrets, endpoint);
            text.published.push_str(&segment);
            text.cut = cut;
            self.changed.notify_all();
        }
    }

    /// Interrupt marks the current attempt as missing text, so nothing more is published for it.
    pub(crate) fn interrupt(&self) {
        self.text.lock().unwrap().incomplete = true;
        self.changed.notify_all();
    }

    /// Finish ends the preview when its Run is terminal and frees its text.
    pub(crate) fn finish(&self) {
        let mut text = self.text.lock().unwrap();
        let attempt = text.attempt;
        *text = Text {
            attempt,
            finished: true,
            ..Text::default()
        };
        self.changed.notify_all();
    }

    /// Page returns text after a cursor, or None when nothing changed since it.
    fn page(&self, cursor: PreviewCursor) -> Result<Option<PreviewPage>, ApiError> {
        let text = self.text.lock().unwrap();
        if cursor.attempt > text.attempt {
            return Err(error(
                "invalid_argument",
                "preview cursor is ahead of this Run",
            ));
        }
        if text.finished {
            return Ok(Some(PreviewPage {
                finished: true,
                ..idle(cursor)
            }));
        }
        let newer = cursor.attempt < text.attempt;
        // A first read has no earlier text to discard.
        let restarted = newer && cursor.attempt > 0;
        let offset = if newer {
            0
        } else {
            usize::try_from(cursor.offset).unwrap_or(usize::MAX)
        };
        if offset > text.published.len() || !text.published.is_char_boundary(offset) {
            return Err(error(
                "invalid_argument",
                "preview cursor is not a text position",
            ));
        }
        if !newer && offset == text.published.len() {
            return Ok(None);
        }
        Ok(Some(PreviewPage {
            attempt: text.attempt,
            restarted,
            text: text.published[offset..].to_owned(),
            next: PreviewCursor {
                attempt: text.attempt,
                offset: text.published.len() as u64,
            },
            incomplete: text.incomplete,
            finished: false,
        }))
    }
}

/// Safe_cut is the longest prefix of received text in which no credential can still be forming:
/// it ends before any tail that could begin a pattern, and never splits a complete occurrence.
fn safe_cut(received: &str, patterns: &[String]) -> usize {
    let bytes = received.as_bytes();
    let mut cut = bytes.len();
    loop {
        let mut next = cut;
        for pattern in patterns.iter().map(String::as_bytes) {
            let start = cut.saturating_sub(pattern.len());
            for at in start..cut {
                if bytes[at] != pattern[0] {
                    continue;
                }
                let rest = &bytes[at..];
                let forming = rest.len() < pattern.len() && pattern.starts_with(rest);
                let spanning = rest.starts_with(pattern) && at + pattern.len() > cut;
                if forming || spanning {
                    next = next.min(at);
                    break;
                }
            }
        }
        while !received.is_char_boundary(next) {
            next -= 1;
        }
        if next == cut {
            return cut;
        }
        cut = next;
    }
}

impl SessionApi {
    /// Read_preview waits up to 30 seconds for speculative reply text of a conversation Run after a cursor.
    pub fn read_preview(
        &self,
        run: &RunRef,
        cursor: Option<PreviewCursor>,
        wait_ms: u64,
        cancellation: Option<&AtomicBool>,
    ) -> Result<PreviewPage, ApiError> {
        if wait_ms > 30_000 {
            return Err(error(
                "invalid_argument",
                "preview wait must not exceed 30000 ms",
            ));
        }
        let cursor = cursor.unwrap_or(PreviewCursor {
            attempt: 0,
            offset: 0,
        });
        let deadline = Instant::now() + Duration::from_millis(wait_ms);
        loop {
            let preview = {
                let state = self.client.shared.state.lock().unwrap();
                let index = state.find_run(run)?;
                let message = state.commands[index].work.message().unwrap();
                if message.run.kind != RunKind::Conversation {
                    return Err(error(
                        "invalid_argument",
                        "only conversation Runs have reply previews",
                    ));
                }
                if message.run.state == RunState::Terminal {
                    return Ok(PreviewPage {
                        attempt: cursor.attempt,
                        restarted: false,
                        text: String::new(),
                        next: cursor,
                        incomplete: false,
                        finished: true,
                    });
                }
                state
                    .preview
                    .as_ref()
                    .filter(|(id, _)| *id == run.run_id)
                    .map(|(_, preview)| preview.clone())
            };
            if cancellation.is_some_and(|token| token.load(Ordering::Acquire)) {
                return Err(error("cancelled", "preview wait cancelled by its caller"));
            }
            let slice = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(20));
            match preview {
                Some(preview) => {
                    if let Some(page) = preview.page(cursor)? {
                        return Ok(page);
                    }
                    if slice.is_zero() {
                        return Ok(idle(cursor));
                    }
                    let text = preview.text.lock().unwrap();
                    drop(preview.changed.wait_timeout(text, slice).unwrap());
                }
                // A queued Run has no preview yet.
                None => {
                    if slice.is_zero() {
                        return Ok(idle(cursor));
                    }
                    std::thread::sleep(slice);
                }
            }
        }
    }
}

fn idle(cursor: PreviewCursor) -> PreviewPage {
    PreviewPage {
        attempt: cursor.attempt,
        restarted: false,
        text: String::new(),
        next: cursor,
        incomplete: false,
        finished: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "sk-live-abcdef";

    fn read(preview: &ReplyPreview) -> String {
        preview
            .page(PreviewCursor {
                attempt: 1,
                offset: 0,
            })
            .unwrap()
            .map(|page| page.text)
            .unwrap_or_default()
    }

    #[test]
    fn text_streams_at_once_when_no_tail_could_begin_a_credential() {
        let preview = ReplyPreview::default();
        preview.begin();
        let secrets = vec![SECRET.to_owned()];
        for delta in ["Hello", ", wor", "ld."] {
            preview.append(delta, &secrets, "");
        }
        assert_eq!(read(&preview), "Hello, world.");
    }

    #[test]
    fn a_credential_split_across_deltas_is_never_published() {
        let preview = ReplyPreview::default();
        preview.begin();
        let secrets = vec![SECRET.to_owned()];
        let mut seen = vec![];
        for delta in ["key: sk-", "live-ab", "cdef", " end", " sk-l"] {
            preview.append(delta, &secrets, "");
            seen.push(read(&preview));
        }
        for text in &seen {
            assert!(
                !text.contains("sk-live") && !text.contains("abcdef"),
                "{text}"
            );
        }
        assert_eq!(seen[0], "key: ");
        assert_eq!(seen[3], "key: [redacted] end");
        assert_eq!(
            seen[4], "key: [redacted] end ",
            "a forming prefix stays held back"
        );
    }

    #[test]
    fn a_new_attempt_restarts_and_a_full_attempt_stops_as_incomplete() {
        let preview = ReplyPreview::default();
        preview.begin();
        preview.append("first try", &[], "");
        preview.begin();
        let page = preview
            .page(PreviewCursor {
                attempt: 1,
                offset: 9,
            })
            .unwrap()
            .unwrap();
        assert!(page.restarted);
        assert_eq!((page.attempt, page.text.as_str()), (2, ""));
        preview.append(&"x".repeat(MAX_PREVIEW_BYTES), &[], "");
        preview.append("more", &[], "");
        let page = preview.page(page.next).unwrap().unwrap();
        assert!(page.incomplete);
        assert_eq!(page.text.len(), MAX_PREVIEW_BYTES);
        assert!(
            preview.page(page.next).unwrap().is_none(),
            "a caught-up reader waits"
        );
        preview.finish();
        assert!(preview.page(page.next).unwrap().unwrap().finished);
    }

    #[test]
    fn safe_cut_never_splits_a_character_or_an_occurrence() {
        let patterns = vec!["ab".to_owned(), "é-x".to_owned()];
        assert_eq!(safe_cut("xxa", &patterns), 2);
        assert_eq!(safe_cut("xxé-", &patterns), 2);
        assert_eq!(safe_cut("xxab", &patterns), 4);
        assert_eq!(safe_cut("", &patterns), 0);
    }
}
