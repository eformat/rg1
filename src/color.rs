//! ANSI coloring for grep-style output (no external deps).
//!
//! Conventions follow grep: magenta filenames, green line numbers, red
//! matches. The probability column is graded (strong matches brighter) so a
//! wall of results is scannable.

use regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorWhen {
    /// Color when stdout is a terminal and NO_COLOR is not set (default).
    Auto,
    /// Always color (even piped).
    Always,
    /// Never color.
    Never,
}

const RESET: &str = "\x1b[0m";
const MAGENTA: &str = "\x1b[35m";
const GREEN: &str = "\x1b[32m";
const GREEN_BOLD: &str = "\x1b[1;32m";
const YELLOW: &str = "\x1b[33m";
const RED_BOLD: &str = "\x1b[1;31m";

pub struct Painter {
    enabled: bool,
}

impl Painter {
    pub fn new(when: ColorWhen) -> Self {
        use std::io::IsTerminal;
        let is_tty = std::io::stdout().is_terminal();
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let enabled = match when {
            ColorWhen::Always => true,
            ColorWhen::Never => false,
            ColorWhen::Auto => is_tty && !no_color,
        };
        Painter { enabled }
    }

    pub fn disabled() -> Self {
        Painter { enabled: false }
    }

    fn wrap(&self, code: &str, s: &str) -> String {
        if self.enabled {
            format!("{code}{s}{RESET}")
        } else {
            s.to_string()
        }
    }

    /// Filename prefix (magenta, like grep).
    pub fn path(&self, s: &str) -> String {
        self.wrap(MAGENTA, s)
    }

    /// Line number (green, like grep).
    pub fn line_no(&self, s: &str) -> String {
        self.wrap(GREEN, s)
    }

    /// Probability, graded by strength: >= 0.9 bold green, >= 0.7 green,
    /// otherwise yellow.
    pub fn prob(&self, p: f64) -> String {
        let s = format!("{p:.3}");
        if !self.enabled {
            return s;
        }
        if p >= 0.9 {
            format!("{GREEN_BOLD}{s}{RESET}")
        } else if p >= 0.7 {
            format!("{GREEN}{s}{RESET}")
        } else {
            format!("{YELLOW}{s}{RESET}")
        }
    }

    /// Wrap keyword occurrences in `text` with red-bold. Boundary-aware:
    /// separators are preserved and words inside longer identifiers are not
    /// highlighted. Multi-line text is handled (newlines are separators).
    pub fn highlight(&self, text: &str, words: Option<&Regex>) -> String {
        if !self.enabled {
            return text.to_string();
        }
        let Some(re) = words else {
            return text.to_string();
        };
        let mut out = String::with_capacity(text.len());
        let mut last = 0usize;
        for m in re.find_iter(text) {
            let before_ok = text[..m.start()]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_ascii_alphanumeric());
            let after_ok = text[m.end()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphanumeric());
            if before_ok && after_ok {
                out.push_str(&text[last..m.start()]);
                out.push_str(RED_BOLD);
                out.push_str(&text[m.start()..m.end()]);
                out.push_str(RESET);
                last = m.end();
            }
        }
        out.push_str(&text[last..]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_painter_is_passthrough() {
        let p = Painter::disabled();
        assert_eq!(p.path("a.txt"), "a.txt");
        assert_eq!(p.prob(0.9), "0.900");
        assert_eq!(p.highlight("login here", None), "login here");
    }

    #[test]
    fn highlight_wraps_whole_words_only() {
        let p = Painter::new(ColorWhen::Always);
        let re = crate::prefilter::highlight_regex(&["login bug".to_string()].to_vec()).unwrap();
        let out = p.highlight("Handle_User_Login(): fix the login bug", Some(&re));
        assert!(out.contains("\x1b[1;31mLogin\x1b[0m"), "out: {out}");
        assert!(out.contains("\x1b[1;31mbug\x1b[0m"), "out: {out}");
        // "username" must not be highlighted
        let out2 = p.highlight("the username field", Some(&re));
        assert!(!out2.contains("\x1b["), "out2: {out2}");
    }

    #[test]
    fn graded_probability() {
        let p = Painter::new(ColorWhen::Always);
        assert!(p.prob(0.95).contains("\x1b[1;32m"));
        assert!(p.prob(0.75).contains("\x1b[32m"));
        assert!(p.prob(0.55).contains("\x1b[33m"));
    }
}
