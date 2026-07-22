#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub const COMMENT: Rgb = Rgb(0x56, 0x5f, 0x89);
pub const BLUE: Rgb = Rgb(0x7a, 0xa2, 0xf7);
pub const CYAN: Rgb = Rgb(0x7d, 0xcf, 0xff);
pub const GREEN: Rgb = Rgb(0x9e, 0xce, 0x6a);
pub const MAGENTA: Rgb = Rgb(0xbb, 0x9a, 0xf7);
pub const YELLOW: Rgb = Rgb(0xe0, 0xaf, 0x68);
pub const RED: Rgb = Rgb(0xf7, 0x76, 0x8e);

/// Payload text reaches the terminal verbatim, so control characters
/// (C0, DEL, C1) are stripped here at the single chokepoint every chip
/// string flows through; otherwise a hostile display_name or directory
/// name could inject escape sequences or break the line contract.
///
/// `char::is_control` alone covers C0, DEL, and C1 (U+0080-U+009F): all
/// three ranges are the Unicode `Cc` category, verified against rustc's
/// char tables, so no separate C1 range check is needed.
pub(crate) fn sanitize(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub colors: bool,
    pub links: bool,
}

impl Style {
    /// NO_COLOR (any non-empty value) suppresses color and links;
    /// FORCE_COLOR (non-empty) overrides NO_COLOR. https://no-color.org/
    pub fn from_env(clickable_links: bool) -> Self {
        let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        let colors = set("FORCE_COLOR") || !set("NO_COLOR");
        Style {
            colors,
            links: colors && clickable_links,
        }
    }

    pub fn paint(&self, text: &str, c: Rgb) -> String {
        let text = sanitize(text);
        if !self.colors || text.is_empty() {
            return text;
        }
        format!("\x1b[38;2;{};{};{}m{}\x1b[0m", c.0, c.1, c.2, text)
    }

    pub fn paint_bold(&self, text: &str, c: Rgb) -> String {
        let text = sanitize(text);
        if !self.colors || text.is_empty() {
            return text;
        }
        format!("\x1b[1m\x1b[38;2;{};{};{}m{}\x1b[0m", c.0, c.1, c.2, text)
    }

    /// Wrap text in an OSC 8 hyperlink. URLs containing control bytes are
    /// rejected: an attacker-controlled URL from stdin could otherwise
    /// break out of the escape envelope and inject terminal sequences.
    pub fn link(&self, url: &str, text: &str) -> String {
        if !self.links || url.is_empty() {
            return text.to_string();
        }
        if url
            .chars()
            .any(|ch| (ch as u32) < 0x20 || ch == '\u{7f}' || ch == '\u{9c}')
        {
            return text.to_string();
        }
        format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Style = Style {
        colors: false,
        links: false,
    };
    const FULL: Style = Style {
        colors: true,
        links: true,
    };

    #[test]
    fn paint_disabled_returns_plain_text() {
        assert_eq!(PLAIN.paint("hi", GREEN), "hi");
        assert_eq!(PLAIN.paint_bold("hi", GREEN), "hi");
    }

    #[test]
    fn paint_emits_truecolor_sgr() {
        assert_eq!(FULL.paint("hi", GREEN), "\x1b[38;2;158;206;106mhi\x1b[0m");
    }

    #[test]
    fn paint_bold_prefixes_bold_sgr() {
        assert_eq!(
            FULL.paint_bold("hi", RED),
            "\x1b[1m\x1b[38;2;247;118;142mhi\x1b[0m"
        );
    }

    #[test]
    fn paint_empty_text_stays_empty() {
        assert_eq!(FULL.paint("", GREEN), "");
    }

    #[test]
    fn link_wraps_in_osc8() {
        assert_eq!(
            FULL.link("https://example.com", "text"),
            "\x1b]8;;https://example.com\x1b\\text\x1b]8;;\x1b\\"
        );
    }

    #[test]
    fn link_disabled_returns_text() {
        assert_eq!(PLAIN.link("https://example.com", "text"), "text");
    }

    #[test]
    fn link_rejects_control_bytes_in_url() {
        assert_eq!(FULL.link("https://e.com/\x07evil", "text"), "text");
        assert_eq!(FULL.link("https://e.com/\x1b[31m", "text"), "text");
        assert_eq!(FULL.link("", "text"), "text");
    }

    #[test]
    fn paint_strips_control_characters_in_both_modes() {
        assert_eq!(
            FULL.paint("evil\x1b[2Jwiped", GREEN),
            FULL.paint("evil[2Jwiped", GREEN)
        );
        assert_eq!(PLAIN.paint("evil\x1b[2Jwiped", GREEN), "evil[2Jwiped");
        assert_eq!(PLAIN.paint("line1\nline2", GREEN), "line1line2");
        assert_eq!(PLAIN.paint_bold("a\u{9c}b\u{7f}c", RED), "abc");
    }
}
