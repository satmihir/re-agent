use super::observations::{entry, fingerprint, MAX_PAGE_BYTES, PAGE_HEADROOM};
use super::*;
use serde::{Deserialize, Serialize};
use std::time::UNIX_EPOCH;

/// SavedSession describes a Session in the local store, read without opening or locking it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedSession {
    pub session_id: String,
    /// origin labels the host that opened it, empty for the CLI and plain library Sessions.
    pub origin: String,
    /// workspace is where the Session was launched.
    pub workspace: String,
    pub model: String,
    pub reasoning_effort: String,
    /// modified_at_ms is when its checkpoint was last saved, in Unix milliseconds.
    pub modified_at_ms: u64,
    /// first_message is a bounded preview of the first thing it was asked.
    pub first_message: String,
    /// open is true while a live process, this one included, owns the Session.
    pub open: bool,
    /// phase is `idle` or `terminal` between Runs; `model`, `accepted` and `tool` are mid-Run.
    pub phase: String,
    /// blocked names the status that stops further messages until a reset, if any.
    pub blocked: String,
    /// last_status and last_reason describe how the latest Run ended, empty before the first.
    pub last_status: String,
    pub last_reason: String,
    /// usage totals the Session's model usage so far.
    pub usage: Usage,
    /// entries counts the Entries in its retained history.
    pub entries: u64,
}

/// SavedEntryPage is a bounded run of a saved Session's visible Entries.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedEntryPage {
    pub session_id: String,
    /// entries are consecutive, in history order, ending just before the requested end.
    pub entries: Vec<EntryView>,
    /// start is the first returned Entry's position; ask for `end: start` to page further back.
    pub start: u64,
    /// total counts the Entries in the retained history.
    pub total: u64,
}

/// Saved_sessions lists every Session in the local store, newest first, without opening any.
pub fn saved_sessions() -> Result<Vec<SavedSession>, ApiError> {
    let scanned = crate::checkpoint::scan().map_err(|e| error("unavailable", e))?;
    Ok(scanned
        .into_iter()
        .map(|(id, modified, cp)| {
            let text = |key: &str| cp[key].as_str().unwrap_or("").to_owned();
            let history = cp["history"].as_array().map_or(&[][..], Vec::as_slice);
            SavedSession {
                open: crate::checkpoint::owned(&id),
                session_id: id,
                origin: text("origin"),
                workspace: text("launch"),
                model: text("model"),
                reasoning_effort: text("effort"),
                modified_at_ms: modified
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64),
                first_message: crate::checkpoint::first_message(&cp, history),
                phase: text("phase"),
                blocked: text("blocked"),
                last_status: text("last_status"),
                last_reason: text("last_reason"),
                usage: Usage::from_value(&cp["usage"]),
                entries: history.len() as u64,
            }
        })
        .collect())
}

/// Read_saved_entries returns up to `limit` visible Entries of a saved Session that end just
/// before `end` (the newest when absent), without opening it. Content is projected as
/// `read_entries` projects it: no reasoning or provider-native items, and bounded bytes.
pub fn read_saved_entries(
    session_id: &str,
    end: Option<u64>,
    limit: usize,
) -> Result<SavedEntryPage, ApiError> {
    if !(1..=100).contains(&limit) {
        return Err(error(
            "invalid_argument",
            "page limit must be between 1 and 100",
        ));
    }
    let (_, saved) = crate::checkpoint::read(session_id)
        .ok_or_else(|| error("not_found", format!("no saved Session {session_id}")))?;
    let history = saved["history"].as_array().map_or(&[][..], Vec::as_slice);
    let total = history.len();
    let end = end.map_or(total, |end| (end as usize).min(total));
    let observed = ObservationStamp {
        revision: 0,
        event_cursor: EventCursor {
            session: SessionRef {
                session_id: session_id.into(),
                instance_id: String::new(),
            },
            sequence: 0,
        },
        observed_at_ms: 0,
    };
    // The newest Entries are kept when the page's bytes run out.
    let mut bytes = PAGE_HEADROOM as u64;
    let mut entries = Vec::new();
    for index in (end.saturating_sub(limit)..end).rev() {
        let value = &history[index];
        let view = entry(
            value,
            index,
            fingerprint(value).0,
            observed.clone(),
            None,
            None,
        );
        let size = fingerprint(&view).0 + 1;
        if bytes + size > MAX_PAGE_BYTES as u64 {
            break;
        }
        bytes += size;
        entries.push(view);
    }
    entries.reverse();
    Ok(SavedEntryPage {
        session_id: session_id.into(),
        start: (end - entries.len()) as u64,
        entries,
        total: total as u64,
    })
}
