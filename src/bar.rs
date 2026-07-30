use crate::theme::{AMBER, GREEN, RED, Rgb};

/// Green at 60 or below, amber below 85, red at 85 or above. Float
/// comparisons on purpose: 60.5 is amber, 84.9 is amber, 85.0 is red.
pub fn bar_color(pct: f64) -> Rgb {
    if pct <= 60.0 {
        GREEN
    } else if pct < 85.0 {
        AMBER
    } else {
        RED
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_bands() {
        assert_eq!(bar_color(0.0), GREEN);
        assert_eq!(bar_color(60.0), GREEN);
        assert_eq!(bar_color(60.5), AMBER);
        assert_eq!(bar_color(84.9), AMBER);
        assert_eq!(bar_color(85.0), RED);
        assert_eq!(bar_color(100.0), RED);
    }
}
