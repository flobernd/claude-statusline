//! Wall-clock access, overridable for reproducible preview captures.

/// Epoch milliseconds. `CLAUDE_STATUSLINE_NOW_MS` pins the value so
/// tools/preview can capture byte-identical output on any machine; unset
/// or unparsable falls back to the real clock.
pub fn now_ms() -> u64 {
    parse_override(std::env::var("CLAUDE_STATUSLINE_NOW_MS").ok().as_deref())
        .unwrap_or_else(real_now_ms)
}

fn parse_override(raw: Option<&str>) -> Option<u64> {
    raw?.trim().parse().ok()
}

fn real_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_parses_plain_millis() {
        assert_eq!(
            parse_override(Some("1784851200000")),
            Some(1_784_851_200_000)
        );
        assert_eq!(parse_override(Some(" 42 ")), Some(42));
    }

    #[test]
    fn garbage_or_missing_override_is_ignored() {
        assert_eq!(parse_override(None), None);
        assert_eq!(parse_override(Some("")), None);
        assert_eq!(parse_override(Some("soon")), None);
        assert_eq!(parse_override(Some("-5")), None);
        assert_eq!(parse_override(Some("1.5e3")), None);
    }

    #[test]
    fn real_clock_advances() {
        assert!(real_now_ms() > 1_700_000_000_000);
    }
}
