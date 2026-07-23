pub fn fmt_tokens(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    if n < 10_000 {
        return format!("{}K", trim_decimal(n as f64 / 1_000.0));
    }
    if n < 1_000_000 {
        return format!("{}K", n / 1_000);
    }
    if n < 10_000_000 {
        return format!("{}M", trim_decimal(n as f64 / 1_000_000.0));
    }
    format!("{}M", n / 1_000_000)
}

fn trim_decimal(v: f64) -> String {
    let s = format!("{v:.1}");
    match s.strip_suffix(".0") {
        Some(t) => t.to_string(),
        None => s,
    }
}

pub fn fmt_duration(ms: u64) -> String {
    let total = ms / 1_000;
    if total < 60 {
        return format!("{total}s");
    }
    if total < 3_600 {
        return format!("{}m{:02}s", total / 60, total % 60);
    }
    if total < 86_400 {
        return format!("{}h{:02}m", total / 3_600, (total % 3_600) / 60);
    }
    // Bare rounded days: at reset-countdown scale the hours are noise.
    format!("{}d", (total + 43_200) / 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_below_1k_are_exact() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
    }

    #[test]
    fn tokens_below_10k_get_one_decimal_with_trailing_zero_stripped() {
        assert_eq!(fmt_tokens(1_000), "1K");
        assert_eq!(fmt_tokens(1_500), "1.5K");
        assert_eq!(fmt_tokens(9_999), "10K");
    }

    #[test]
    fn tokens_below_1m_are_whole_k() {
        assert_eq!(fmt_tokens(412_000), "412K");
        assert_eq!(fmt_tokens(999_999), "999K");
    }

    #[test]
    fn tokens_in_millions() {
        assert_eq!(fmt_tokens(1_000_000), "1M");
        assert_eq!(fmt_tokens(1_500_000), "1.5M");
        assert_eq!(fmt_tokens(12_000_000), "12M");
    }

    #[test]
    fn duration_seconds() {
        assert_eq!(fmt_duration(0), "0s");
        assert_eq!(fmt_duration(59_999), "59s");
    }

    #[test]
    fn duration_minutes_pad_seconds() {
        assert_eq!(fmt_duration(60_000), "1m00s");
        assert_eq!(fmt_duration(725_000), "12m05s");
    }

    #[test]
    fn duration_hours_pad_minutes() {
        assert_eq!(fmt_duration(3_600_000), "1h00m");
        assert_eq!(fmt_duration(4_530_000), "1h15m");
        assert_eq!(fmt_duration(86_399_000), "23h59m");
    }

    #[test]
    fn duration_days_are_bare_and_rounded() {
        assert_eq!(fmt_duration(86_400_000), "1d");
        assert_eq!(fmt_duration(302_400_000), "4d"); // 3.5d rounds up
        assert_eq!(fmt_duration(690_000_000), "8d");
    }
}
