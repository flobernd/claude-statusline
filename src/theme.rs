#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub const COMMENT: Rgb = Rgb(0x56, 0x5f, 0x89);
pub const BLUE: Rgb = Rgb(0x7a, 0xa2, 0xf7);
pub const CYAN: Rgb = Rgb(0x7d, 0xcf, 0xff);
pub const GREEN: Rgb = Rgb(0x9e, 0xce, 0x6a);
pub const MAGENTA: Rgb = Rgb(0xbb, 0x9a, 0xf7);
pub const YELLOW: Rgb = Rgb(0xe0, 0xaf, 0x68);
pub const RED: Rgb = Rgb(0xf7, 0x76, 0x8e);

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
        Style { colors, links: colors && clickable_links }
    }

    pub fn paint(&self, text: &str, c: Rgb) -> String {
        if !self.colors || text.is_empty() {
            return text.to_string();
        }
        format!("\x1b[38;2;{};{};{}m{}\x1b[0m", c.0, c.1, c.2, text)
    }

    pub fn paint_bold(&self, text: &str, c: Rgb) -> String {
        if !self.colors || text.is_empty() {
            return text.to_string();
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
        if url.chars().any(|ch| (ch as u32) < 0x20 || ch == '\u{7f}' || ch == '\u{9c}') {
            return text.to_string();
        }
        format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Style = Style { colors: false, links: false };
    const FULL: Style = Style { colors: true, links: true };

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
        assert_eq!(FULL.paint_bold("hi", RED), "\x1b[1m\x1b[38;2;247;118;142mhi\x1b[0m");
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
}
