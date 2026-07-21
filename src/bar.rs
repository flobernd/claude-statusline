use crate::theme::{COMMENT, GREEN, RED, Rgb, Style, YELLOW};

/// Green at 60 or below, yellow below 85, red at 85 or above. Float
/// comparisons on purpose: 60.5 is yellow, 84.9 is yellow, 85.0 is red.
pub fn bar_color(pct: f64) -> Rgb {
    if pct <= 60.0 {
        GREEN
    } else if pct < 85.0 {
        YELLOW
    } else {
        RED
    }
}

pub fn render_bar(pct: f64, width: usize, style: &Style) -> String {
    let clamped = pct.clamp(0.0, 100.0);
    let color = bar_color(clamped);
    let filled = (width as f64 * clamped / 100.0) as usize;
    let empty = width - filled;
    format!(
        "[{}{}]",
        style.paint(&"\u{2588}".repeat(filled), color),
        style.paint(&"\u{2591}".repeat(empty), COMMENT),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Style = Style { colors: false, links: false };

    #[test]
    fn color_bands() {
        assert_eq!(bar_color(0.0), GREEN);
        assert_eq!(bar_color(60.0), GREEN);
        assert_eq!(bar_color(60.5), YELLOW);
        assert_eq!(bar_color(84.9), YELLOW);
        assert_eq!(bar_color(85.0), RED);
        assert_eq!(bar_color(100.0), RED);
    }

    #[test]
    fn fill_proportional_to_percentage() {
        assert_eq!(render_bar(42.0, 20, &PLAIN), format!("[{}{}]", "\u{2588}".repeat(8), "\u{2591}".repeat(12)));
        assert_eq!(render_bar(0.0, 20, &PLAIN), format!("[{}]", "\u{2591}".repeat(20)));
        assert_eq!(render_bar(100.0, 20, &PLAIN), format!("[{}]", "\u{2588}".repeat(20)));
    }

    #[test]
    fn out_of_range_percentages_clamp() {
        assert_eq!(render_bar(-5.0, 10, &PLAIN), format!("[{}]", "\u{2591}".repeat(10)));
        assert_eq!(render_bar(400.0, 10, &PLAIN), format!("[{}]", "\u{2588}".repeat(10)));
    }

    #[test]
    fn colored_bar_uses_band_color_for_fill_and_comment_for_empty() {
        let full = Style { colors: true, links: false };
        let bar = render_bar(90.0, 10, &full);
        assert!(bar.contains("\x1b[38;2;247;118;142m")); // red fill
        assert!(bar.contains("\x1b[38;2;86;95;137m")); // comment empty
        assert!(bar.starts_with('['));
        assert!(bar.ends_with(']'));
    }

    #[test]
    fn fill_scales_before_truncation_for_non_divisor_widths() {
        assert_eq!(render_bar(3.5, 30, &PLAIN), format!("[{}{}]", "\u{2588}", "\u{2591}".repeat(29)));
    }
}
