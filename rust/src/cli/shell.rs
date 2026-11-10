use crate::cli::text;
use crate::runtime::Session;
use crate::{render, user_shell};
use serde_json::json;
use std::io::{self, Write};

fn elapsed(ms: u64) -> String {
    if ms < 60000 {
        format!("{:.1}s", ms as f64 / 1000.)
    } else {
        format!("{}m{:02}s", ms / 60000, ms / 1000 % 60)
    }
}
struct Live<'a> {
    region: Option<&'a crate::terminal::RegionHandle>,
    pending: Vec<u8>,
}
impl Write for Live<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.region.is_none() {
            return io::stdout().write(bytes);
        }
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.pending.drain(..=end).collect();
            self.region
                .unwrap()
                .lock()
                .unwrap()
                .insert_bytes(&line[..line.len() - 1])?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}

/// Execute streams a user command and records its outcome without a model turn.
pub(crate) fn execute(session: &mut Session, command: &str) {
    let region = session.region.clone();
    macro_rules! eprintln {
        ($($args:tt)*) => { crate::terminal::output(region.as_ref(), 2, &format!("{}\n", format_args!($($args)*))) };
    }
    let mut live = Live {
        region: region.as_ref(),
        pending: Vec::new(),
    };
    if command.is_empty() {
        eprintln!("!COMMAND runs COMMAND in the workspace; its output joins the conversation");
        return;
    }
    match user_shell::run(
        text(&session.config, "workspace"),
        command.as_bytes(),
        &mut live,
        &crate::cancellation::token(&session.config),
    ) {
        Err(error) => eprintln!("error: {}", render::sanitize(&error)),
        Ok(outcome) => {
            if outcome.open {
                let _ = live.write_all(b"\n");
            }
            if let Some(error) = outcome.reason {
                eprintln!("error: {}", render::sanitize(&error));
            }
            let record = outcome.record;
            let duration = elapsed(record["duration_ms"].as_u64().unwrap_or(0));
            let mut footer = if record["interrupted"] == true {
                format!("interrupted after {duration}")
            } else if let Some(signal) = record["signal"].as_str() {
                format!("killed by {signal} after {duration}")
            } else {
                format!("exit {} · {duration}", record["exit_code"])
            };
            footer.push_str(" · added to the conversation");
            if record["output_truncated"] == true {
                footer.push_str(" · output truncated");
            }
            session.history.push(json!({"kind":"shell","shell":record}));
            if render::styled(2) {
                eprintln!("\x1b[2m{footer}\x1b[0m\n");
            } else {
                eprintln!("{footer}");
            }
        }
    }
}
