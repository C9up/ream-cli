//! Terminal output for the binary.
//!
//! DELIBERATE DUPLICATION — this is the Rust half of `@c9up/lumen`, and it is
//! a second implementation of the same contract rather than a shared library:
//! every package here lives in its own repository, so a `path` dependency on
//! `packages/lumen` would build in the monorepo and break for anyone who
//! cloned this one alone. What must stay in step is the VOCABULARY, not the
//! code: the same labels, the same colours, the same environment rules. Change
//! one side and change the other.
//!
//! The `--ansi` / `--no-ansi` flags are already exported as `FORCE_COLOR` /
//! `NO_COLOR` before anything here runs, so both halves of a command — this
//! binary and the Node process it spawns — reach the same decision.

use std::io::IsTerminal;

/// Open and close codes of a style.
///
/// Both halves matter: closing with a blanket reset drops the styles an outer
/// call was still holding, so a dim word inside a red line would end the red.
#[derive(Clone, Copy)]
pub struct Style(u8, u8);

pub const BOLD: Style = Style(1, 22);
pub const DIM: Style = Style(2, 22);
pub const RED: Style = Style(31, 39);
pub const GREEN: Style = Style(32, 39);
pub const YELLOW: Style = Style(33, 39);
pub const BLUE: Style = Style(34, 39);
pub const CYAN: Style = Style(36, 39);
pub const WHITE: Style = Style(37, 39);
pub const BG_RED: Style = Style(41, 49);

/// CI services that render escape codes in their log viewer. A CI that is not
/// on this list gets none: `[32m` on every line is worse than a plain
/// transcript, and `FORCE_COLOR` is there to say otherwise.
const COLOUR_CAPABLE_CI: [&str; 7] = [
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "CIRCLECI",
    "TRAVIS",
    "BUILDKITE",
    "DRONE",
    "APPVEYOR",
];

fn env_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.is_empty())
}

/// Should this process emit escape codes? Same order as the TypeScript side:
/// an explicit `FORCE_COLOR` wins, then `NO_COLOR`, then the terminal.
pub fn supports_color() -> bool {
    if let Ok(force) = std::env::var("FORCE_COLOR") {
        if !force.is_empty() {
            return force != "0" && force != "false";
        }
    }
    if env_set("NO_COLOR") {
        return false;
    }
    if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
        return false;
    }
    if env_set("CI") {
        return COLOUR_CAPABLE_CI.iter().any(|name| env_set(name));
    }
    std::io::stdout().is_terminal()
}

/// Wrap text in one style, or hand it back untouched when colour is off.
pub fn paint(text: &str, style: Style) -> String {
    if !supports_color() {
        return text.to_string();
    }
    format!("\x1b[{}m{}\x1b[{}m", style.0, text, style.1)
}

/// Wrap text in several styles, opened outside-in and closed inside-out.
pub fn paint_all(text: &str, styles: &[Style]) -> String {
    if !supports_color() || styles.is_empty() {
        return text.to_string();
    }
    let open: String = styles.iter().map(|s| format!("\x1b[{}m", s.0)).collect();
    let close: String = styles
        .iter()
        .rev()
        .map(|s| format!("\x1b[{}m", s.1))
        .collect();
    format!("{open}{text}{close}")
}

/// The levels, with the label and colour they carry everywhere.
#[derive(Clone, Copy)]
pub enum Level {
    Success,
    Info,
    Warning,
    Error,
}

impl Level {
    fn parts(self) -> (&'static str, Style) {
        match self {
            Level::Success => ("success", GREEN),
            Level::Info => ("info", BLUE),
            Level::Warning => ("warn", YELLOW),
            Level::Error => ("error", RED),
        }
    }
}

/// `[ info ] the message`, the shape every Ream package prints.
pub fn message(level: Level, text: &str) -> String {
    let (label, style) = level.parts();
    format!("[ {} ] {}", paint(label, style), text)
}

pub fn success(text: &str) {
    println!("{}", message(Level::Success, text));
}

pub fn info(text: &str) {
    println!("{}", message(Level::Info, text));
}

/// Alerts go to stderr, so a command's data output stays pipeable.
pub fn warning(text: &str) {
    eprintln!("{}", message(Level::Warning, text));
}

pub fn error(text: &str) {
    eprintln!("{}", message(Level::Error, text));
}

/// A heading above a block — the yellow `Usage:` and section titles.
pub fn heading(text: &str) -> String {
    paint(text, YELLOW)
}

/// The badge an unhandled failure carries, so it cannot be missed in a scroll.
pub fn error_badge(text: &str) -> String {
    format!("{} {}", paint_all("  ERROR  ", &[BG_RED, WHITE]), text)
}

/// Visible width: escape codes draw nothing, and a CJK glyph or an emoji takes
/// two columns. Measured with `.len()`, one such glyph makes every row under
/// it ragged.
pub fn display_width(text: &str) -> usize {
    let mut width = 0;
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\x1b' {
            // Skip the whole SGR sequence, up to and including the `m`.
            for escaped in chars.by_ref() {
                if escaped == 'm' {
                    break;
                }
            }
            continue;
        }
        width += char_width(character);
    }
    width
}

fn char_width(character: char) -> usize {
    let code = character as u32;
    if code < 0x20 || (0x7f..0xa0).contains(&code) {
        return 0;
    }
    const ZERO: [(u32, u32); 6] = [
        (0x0300, 0x036f),
        (0x1ab0, 0x1aff),
        (0x1dc0, 0x1dff),
        (0x200b, 0x200f),
        (0x20d0, 0x20ff),
        (0xfe00, 0xfe0f),
    ];
    if ZERO
        .iter()
        .any(|(start, end)| code >= *start && code <= *end)
    {
        return 0;
    }
    const WIDE: [(u32, u32); 10] = [
        (0x1100, 0x115f),
        (0x2e80, 0x303e),
        (0x3041, 0x33ff),
        (0x3400, 0x4dbf),
        (0x4e00, 0x9fff),
        (0xac00, 0xd7a3),
        (0xf900, 0xfaff),
        (0xff00, 0xff60),
        (0x1f300, 0x1f64f),
        (0x1f680, 0x1f9ff),
    ];
    if WIDE
        .iter()
        .any(|(start, end)| code >= *start && code <= *end)
    {
        return 2;
    }
    1
}

/// Pad to `width` on the VISIBLE width, so colour never shifts a column.
pub fn pad_end(text: &str, width: usize) -> String {
    let visible = display_width(text);
    if visible >= width {
        return text.to_string();
    }
    format!("{}{}", text, " ".repeat(width - visible))
}

/// A bordered box, for what must not scroll past unread.
///
/// `painter` colours the border, which is how the same box is dim for a notice
/// and red for a failure.
pub fn sticker(lines: &[String], painter: fn(&str) -> String) -> Vec<String> {
    if lines.is_empty() {
        return Vec::new();
    }
    const LEFT: usize = 4;
    const RIGHT: usize = 8;
    let inner = lines
        .iter()
        .map(|line| display_width(line))
        .max()
        .unwrap_or(0);
    let span = inner + LEFT + RIGHT;

    let edge = |left: &str, right: &str| {
        format!(
            "{}{}{}",
            painter(left),
            painter("─").repeat(span),
            painter(right)
        )
    };
    let row = |text: &str| {
        format!(
            "{}{}{}{}{}",
            painter("│"),
            " ".repeat(LEFT),
            text,
            " ".repeat(inner - display_width(text) + RIGHT),
            painter("│")
        )
    };

    let mut out = vec![edge("╭", "╮"), row("")];
    for line in lines {
        out.push(row(line));
    }
    out.push(row(""));
    out.push(edge("╰", "╯"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_ignores_escape_codes() {
        assert_eq!(display_width("\x1b[32mDONE\x1b[39m"), 4);
    }

    #[test]
    fn width_counts_a_wide_glyph_as_two_columns() {
        assert_eq!(display_width("日本語"), 6);
        assert_eq!(display_width("🚀"), 2);
    }

    #[test]
    fn width_gives_a_combining_accent_no_column() {
        assert_eq!(display_width("e\u{0301}"), 1);
    }

    #[test]
    fn padding_is_measured_on_the_visible_text() {
        // The escape codes take no column, so the padded string must still
        // occupy exactly four.
        let padded = pad_end("\x1b[32mab\x1b[39m", 4);
        assert_eq!(display_width(&padded), 4);
        // Already wider: left alone rather than truncated.
        assert_eq!(pad_end("abcdef", 3), "abcdef");
    }

    #[test]
    fn every_line_of_a_box_is_the_same_width() {
        let lines = sticker(
            &["short".to_string(), "a much longer line".to_string()],
            |c| c.to_string(),
        );
        let widths: Vec<usize> = lines.iter().map(|line| display_width(line)).collect();
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "{widths:?}"
        );
        assert!(lines[0].starts_with('╭'));
    }

    #[test]
    fn an_empty_box_draws_nothing() {
        assert!(sticker(&[], |c| c.to_string()).is_empty());
    }

    #[test]
    fn the_label_vocabulary_matches_the_typescript_side() {
        // Colour off here, so this asserts the words and the shape — the part
        // that has to stay identical across the two implementations.
        temp_env("NO_COLOR", Some("1"), || {
            assert_eq!(message(Level::Success, "x"), "[ success ] x");
            assert_eq!(message(Level::Info, "x"), "[ info ] x");
            assert_eq!(message(Level::Warning, "x"), "[ warn ] x");
            assert_eq!(message(Level::Error, "x"), "[ error ] x");
        });
    }

    #[test]
    fn force_color_wins_over_no_color_both_ways() {
        temp_env("NO_COLOR", Some("1"), || {
            temp_env("FORCE_COLOR", Some("1"), || {
                assert!(supports_color());
            });
            // Set to a falsy value it means OFF — existing is not the same as on.
            temp_env("FORCE_COLOR", Some("0"), || {
                assert!(!supports_color());
            });
            temp_env("FORCE_COLOR", None, || {
                assert!(!supports_color());
            });
        });
    }

    /// Set an env var for the duration of a closure, restoring it after.
    ///
    /// Rust runs tests in threads of one process, so this is not isolation —
    /// the colour tests are the only ones touching these variables, and they
    /// each restore what they found.
    fn temp_env(name: &str, value: Option<&str>, body: impl FnOnce()) {
        let previous = std::env::var(name).ok();
        match value {
            Some(v) => unsafe { std::env::set_var(name, v) },
            None => unsafe { std::env::remove_var(name) },
        }
        body();
        match previous {
            Some(v) => unsafe { std::env::set_var(name, v) },
            None => unsafe { std::env::remove_var(name) },
        }
    }
}
