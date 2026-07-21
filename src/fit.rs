/// Width in terminal cells after stripping SGR and OSC 8 escapes. Every
/// glyph the statusline emits is single-width, so chars-as-1 is exact.
pub fn visible_width(s: &str) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut width = 0;
    while i < chars.len() {
        if chars[i] == '\x1b' && i + 1 < chars.len() && chars[i + 1] == '[' {
            i += 2;
            while i < chars.len() && chars[i] != 'm' {
                i += 1;
            }
            i += 1;
        } else if chars[i] == '\x1b' && i + 1 < chars.len() && chars[i + 1] == ']' {
            i += 2;
            while i < chars.len() {
                if chars[i] == '\x07' {
                    i += 1;
                    break;
                }
                if chars[i] == '\x1b' && i + 1 < chars.len() && chars[i + 1] == '\\' {
                    i += 2;
                    break;
                }
                i += 1;
            }
        } else {
            width += 1;
            i += 1;
        }
    }
    width
}

/// Drop sections in drop_order until the separator-joined line fits.
/// Names not present in drop_order are never dropped.
pub fn fit_line(
    items: Vec<(&'static str, String)>,
    sep_width: usize,
    max_width: usize,
    drop_order: &[&str],
) -> Vec<(&'static str, String)> {
    let mut items: Vec<(&'static str, String, usize)> = items
        .into_iter()
        .map(|(name, rendered)| {
            let w = visible_width(&rendered);
            (name, rendered, w)
        })
        .collect();
    let total = |v: &[(&'static str, String, usize)]| -> usize {
        v.iter().map(|t| t.2).sum::<usize>() + sep_width * v.len().saturating_sub(1)
    };
    for name in drop_order {
        if total(&items) <= max_width {
            break;
        }
        items.retain(|(n, _, _)| n != name);
    }
    items.into_iter().map(|(n, r, _)| (n, r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_width_is_char_count() {
        assert_eq!(visible_width("hello"), 5);
        assert_eq!(visible_width("\u{2387} main"), 6);
        assert_eq!(visible_width(""), 0);
    }

    #[test]
    fn sgr_escapes_are_invisible() {
        assert_eq!(visible_width("\x1b[38;2;1;2;3mhi\x1b[0m"), 2);
        assert_eq!(visible_width("\x1b[1m\x1b[38;2;1;2;3mhi\x1b[0m"), 2);
    }

    #[test]
    fn osc8_links_are_invisible_with_both_terminators() {
        assert_eq!(visible_width("\x1b]8;;https://e.com\x1b\\text\x1b]8;;\x1b\\"), 4);
        assert_eq!(visible_width("\x1b]8;;https://e.com\x07text\x1b]8;;\x07"), 4);
    }

    #[test]
    fn fitting_line_drops_in_priority_order() {
        let items = vec![
            ("keep", "aaaa".to_string()),
            ("first", "bbbb".to_string()),
            ("second", "cccc".to_string()),
        ];
        // total = 4*3 + 2 seps * 3 = 18; max 12 forces dropping "first"
        // (leaves 4+4+3 = 11), which fits.
        let fitted = fit_line(items, 3, 12, &["first", "second"]);
        let names: Vec<&str> = fitted.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, vec!["keep", "second"]);
    }

    #[test]
    fn sections_not_in_drop_order_survive_overflow() {
        let items = vec![("keep", "x".repeat(50))];
        let fitted = fit_line(items, 3, 10, &["other"]);
        assert_eq!(fitted.len(), 1);
    }

    #[test]
    fn fitting_line_that_already_fits_drops_nothing() {
        let items = vec![("a", "aa".to_string()), ("b", "bb".to_string())];
        let fitted = fit_line(items, 3, 80, &["a", "b"]);
        assert_eq!(fitted.len(), 2);
    }
}
