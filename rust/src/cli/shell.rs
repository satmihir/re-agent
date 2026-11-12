use crate::render;
use crate::session_api::ShellResult;
use crate::terminal::{output, RegionHandle};

/// Live renders bounded process events without treating repaired display text as retained capture.
pub(crate) struct Live {
    region: Option<RegionHandle>,
    pending: Vec<u8>,
    offset: u64,
    gap: bool,
}

impl Live {
    /// new creates a display consumer for one exact human shell control.
    pub(crate) fn new(region: Option<RegionHandle>) -> Self {
        Self {
            region,
            pending: Vec::new(),
            offset: 0,
            gap: false,
        }
    }

    /// omitted marks event loss explicitly and leaves recovery to the final bounded capture.
    pub(crate) fn omitted(&mut self) {
        if !self.gap {
            self.flush(true);
            output(self.region.as_ref(), 2, "shell live output omitted; final bounded capture follows when the command settles\n");
        }
        self.gap = true;
    }

    /// push consumes an offset-addressed chunk, retaining incomplete UTF-8 only until the next chunk.
    pub(crate) fn push(&mut self, offset: u64, bytes: &[u8]) {
        if offset != self.offset {
            self.omitted();
        }
        self.offset = offset.saturating_add(bytes.len() as u64);
        if self.gap {
            return;
        }
        self.pending.extend_from_slice(bytes);
        self.flush(false);
    }

    fn flush(&mut self, final_chunk: bool) {
        loop {
            let end = self
                .pending
                .iter()
                .position(|b| *b == b'\n')
                .map(|n| (n + 1).min(4096))
                .or_else(|| (self.pending.len() >= 4096).then_some(4096))
                .or_else(|| {
                    ((final_chunk || self.region.is_none()) && !self.pending.is_empty())
                        .then_some(self.pending.len())
                });
            let Some(mut end) = end else { return };
            if !final_chunk {
                let mut start = 0;
                while let Err(error) = std::str::from_utf8(&self.pending[start..end]) {
                    start += error.valid_up_to();
                    match error.error_len() {
                        Some(invalid) => start += invalid,
                        None => {
                            end = start;
                            break;
                        }
                    }
                }
            }
            if end == 0 {
                return;
            }
            let bytes: Vec<_> = self.pending.drain(..end).collect();
            let text = render::sanitize(&String::from_utf8_lossy(&bytes));
            if self.region.is_some() && !final_chunk && !bytes.ends_with(b"\n") {
                output(
                    self.region.as_ref(),
                    1,
                    &format!("{text} ⟪shell line continues⟫\n"),
                );
            } else {
                output(self.region.as_ref(), 1, &text);
            }
        }
    }

    /// finish prints a footer and, only after a gap, an explicitly labelled final capture.
    pub(crate) fn finish(&mut self, result: &ShellResult) {
        if result.effect == "none" {
            output(
                self.region.as_ref(),
                2,
                if result.status == "persistence_error" {
                    "shell not started; no live Entry added; checkpoint state may have changed\n"
                } else {
                    "shell not started; no conversation entry was added\n"
                },
            );
            return;
        }
        if self.offset < result.output_bytes_seen {
            self.omitted();
        }
        self.flush(true);
        if self.gap {
            output(
                self.region.as_ref(),
                2,
                "final bounded shell capture (may repeat earlier live output):\n",
            );
            output(
                self.region.as_ref(),
                1,
                &render::sanitize(&String::from_utf8_lossy(&result.output)),
            );
        }
        if !result.output.is_empty() && !result.output.ends_with(b"\n") {
            output(self.region.as_ref(), 1, "\n");
        }
        let duration = if result.duration_ms < 60000 {
            format!("{:.1}s", result.duration_ms as f64 / 1000.)
        } else {
            format!(
                "{}m{:02}s",
                result.duration_ms / 60000,
                result.duration_ms / 1000 % 60
            )
        };
        let mut footer = if result.interrupted {
            format!("interrupted after {duration}")
        } else if let Some(signal) = &result.signal {
            format!("killed by {signal} after {duration}")
        } else {
            format!(
                "exit {} · {duration}",
                result
                    .exit_code
                    .map_or("unknown".into(), |code| code.to_string())
            )
        };
        footer.push_str(if result.status == "persistence_error" {
            " · retained in memory; checkpoint failed"
        } else {
            " · added to the conversation"
        });
        if result.output_truncated {
            footer.push_str(" · output truncated");
        }
        if result.effect == "unknown" {
            footer.push_str(" · effects unknown; inspect before continuing");
        }
        let footer = if render::styled(2) {
            format!(
                "{}{footer}{}\n",
                render::palette::DIM,
                render::palette::RESET
            )
        } else {
            footer
        };
        output(self.region.as_ref(), 2, &format!("{footer}\n"));
    }
}
