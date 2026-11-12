//! Shared terminal colours and text styles; callers decide whether styling is enabled.

/// Reset restores the terminal's default style.
pub(crate) const RESET: &str = "\x1b[0m";
/// Bold emphasizes text without changing its colour.
pub(crate) const BOLD: &str = "\x1b[1m";
/// Dim de-emphasizes secondary text and structural marks.
pub(crate) const DIM: &str = "\x1b[2m";
/// Italic emphasizes inline Markdown text.
pub(crate) const ITALIC: &str = "\x1b[3m";
/// Underline identifies terminal hyperlinks.
pub(crate) const UNDERLINE: &str = "\x1b[4m";
/// Underline off ends link emphasis without resetting surrounding styles.
pub(crate) const UNDERLINE_OFF: &str = "\x1b[24m";
/// Accent ties headings, writes and selections to the flask's teal prompt.
pub(crate) const ACCENT: &str = "\x1b[38;5;86m";
/// Success marks completed work and added lines.
pub(crate) const SUCCESS: &str = "\x1b[38;5;114m";
/// Warning marks uncertain effects and other cautions.
pub(crate) const WARNING: &str = "\x1b[38;5;179m";
/// Error marks failures and removed lines.
pub(crate) const ERROR: &str = "\x1b[38;5;174m";
/// Muted identifies read-only tools without competing with writes.
pub(crate) const MUTED: &str = "\x1b[38;5;245m";
/// Exec distinguishes commands and the flask's bubbles from file tools.
pub(crate) const EXEC: &str = "\x1b[38;5;99m";
/// Diff added pairs readable text with a soft green background.
pub(crate) const DIFF_ADDED: &str = "\x1b[38;5;252;48;5;22m";
/// Diff removed pairs readable text with a soft red background.
pub(crate) const DIFF_REMOVED: &str = "\x1b[38;5;252;48;5;52m";
/// Submitted pairs a neutral background with readable text on light and dark terminals.
pub(crate) const SUBMITTED: &str = "\x1b[38;5;252;48;5;236m";

/// Flask liquid preserves the welcome illustration's decorative gradient.
pub(crate) const FLASK_LIQUID: [&str; 13] = [
    "",
    "",
    "\x1b[48;5;17m",
    "\x1b[48;5;18m",
    "\x1b[48;5;19m",
    "\x1b[48;5;20m",
    "\x1b[48;5;56m",
    "\x1b[48;5;57m",
    "\x1b[48;5;57m",
    "\x1b[48;5;93m",
    "\x1b[48;5;93m",
    "",
    "",
];
