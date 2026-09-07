use unicode_width::UnicodeWidthStr;

/// Width in terminal cells after stripping SGR and OSC 8 escapes. Payload
/// text (task names, branches, emails) is not limited to single-width
/// glyphs, so the visible runs are measured per UAX #11, each run on its
/// own: an escape ends a sequence, so an emoji split around one is two
/// glyphs. The fixed glyphs the statusline emits are ambiguous-width and
/// count one, as they did before.
pub fn visible_width(s: &str) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut width = 0;
    let mut run = String::new();
    let flush = |run: &mut String, width: &mut usize| {
        *width += run.width();
        run.clear();
    };
    while i < chars.len() {
        if chars[i] == '\x1b' && i + 1 < chars.len() && chars[i + 1] == '[' {
            flush(&mut run, &mut width);
            i += 2;
            while i < chars.len() && chars[i] != 'm' {
                i += 1;
            }
            i += 1;
        } else if chars[i] == '\x1b' && i + 1 < chars.len() && chars[i + 1] == ']' {
            flush(&mut run, &mut width);
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
            run.push(chars[i]);
            i += 1;
        }
    }
    flush(&mut run, &mut width);
    width
}

/// The longest prefix of `text` that fits in `cells`, measured as a string
/// after each character so a cut never lands inside a wide character or
/// a joined sequence. A dangling joiner or selector measures like the
/// sequence it starts, so it is dropped together with what follows it.
pub fn take_cells(text: &str, cells: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        out.push(c);
        if out.width() > cells {
            out.pop();
            break;
        }
    }
    out
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
        assert_eq!(
            visible_width("\x1b]8;;https://e.com\x1b\\text\x1b]8;;\x1b\\"),
            4
        );
        assert_eq!(
            visible_width("\x1b]8;;https://e.com\x07text\x1b]8;;\x07"),
            4
        );
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

    #[test]
    fn wide_and_combining_characters_measure_in_cells() {
        assert_eq!(visible_width("\u{754c}\u{754c}"), 4);
        assert_eq!(visible_width("e\u{0301}"), 1);
        assert_eq!(visible_width("\x1b[38;2;1;2;3m\u{754c}\x1b[0m"), 2);
        assert_eq!(
            visible_width("\u{2502}\u{2B06}\u{2387}\u{2338}\u{2302}\u{2301}"),
            6
        );
        // Sequences: a variation selector makes the heart emoji-wide, and a ZWJ family is
        // one two-cell glyph, not four characters.
        assert_eq!(visible_width("\u{2764}\u{FE0F}"), 2);
        assert_eq!(visible_width("\u{1F468}\u{200D}\u{1F469}"), 2);
        // An escape between the two halves must not merge them into one sequence.
        assert_eq!(visible_width("\u{1F468}\x1b[0m\u{200D}\u{1F469}"), 4);
    }

    #[test]
    fn take_cells_never_splits_a_wide_character_or_a_sequence() {
        assert_eq!(take_cells("abc", 5), "abc");
        assert_eq!(take_cells("abc", 2), "ab");
        assert_eq!(
            take_cells("\u{754c}\u{754c}\u{754c}", 5),
            "\u{754c}\u{754c}"
        );
        assert_eq!(take_cells("a\u{754c}", 2), "a");
        assert_eq!(take_cells("e\u{0301}x", 1), "e\u{0301}");
        assert_eq!(
            take_cells("\u{1F468}\u{200D}\u{1F469}x", 2),
            "\u{1F468}\u{200D}\u{1F469}"
        );
        assert_eq!(take_cells("\u{1F468}\u{200D}\u{1F469}", 1), "");
        assert_eq!(take_cells("abc", 0), "");
    }
}
