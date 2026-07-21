use crate::bar::render_bar;
use crate::format::{fmt_duration, fmt_tokens};
use crate::git::GitInfo;
use crate::schema::Payload;
use crate::theme::{BLUE, COMMENT, GREEN, MAGENTA, Style, YELLOW};

/// Display heuristic: past five minutes the prompt cache is likely cold.
pub const CACHE_AGE_WARN_MS: i64 = 5 * 60 * 1000;

pub struct Ctx<'a> {
    pub payload: &'a Payload,
    pub git: &'a GitInfo,
    pub cache_age_ms: Option<i64>,
    pub style: &'a Style,
}

pub fn line1(c: &Ctx) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let s = c.style;
    let cw = c.payload.context_window.as_ref();
    let pct = cw.and_then(|w| w.used_percentage);
    let window = cw.and_then(|w| w.context_window_size).filter(|w| *w > 0.0);
    let usage = cw.and_then(|w| w.current_usage.as_ref());

    if let Some(p) = pct {
        out.push(("bar", render_bar(p, 20, s)));
    }

    if let (Some(p), Some(win)) = (pct, window) {
        let p = p.clamp(0.0, 100.0);
        // Derived from the percentage so the chip always agrees with the
        // bar beside it; the token fields are not the documented fill.
        let used = (win * p / 100.0).round() as u64;
        out.push((
            "context_tokens",
            s.paint(&format!("ctx:{}/{}", fmt_tokens(used), fmt_tokens(win as u64)), COMMENT),
        ));
    }

    let input = usage.and_then(|u| u.input_tokens);
    let output = usage.and_then(|u| u.output_tokens);
    if input.is_some() || output.is_some() {
        let fmt = |v: Option<f64>| v.map_or("?".to_string(), |n| fmt_tokens(n.max(0.0) as u64));
        out.push((
            "tokens",
            format!(
                "{}{} {}{}",
                s.paint("in:", COMMENT),
                s.paint(&fmt(input), BLUE),
                s.paint("out:", COMMENT),
                s.paint(&fmt(output), BLUE),
            ),
        ));
    }

    if let Some(u) = usage {
        let read = u.cache_read_input_tokens.unwrap_or(0.0).max(0.0);
        let denom = read
            + u.input_tokens.unwrap_or(0.0).max(0.0)
            + u.cache_creation_input_tokens.unwrap_or(0.0).max(0.0);
        if read > 0.0 && denom > 0.0 {
            let ratio = (read / denom * 100.0) as u64;
            if ratio > 0 {
                out.push((
                    "cache",
                    format!("{}{}", s.paint("cache:", COMMENT), s.paint(&format!("{ratio}%"), GREEN)),
                ));
            }
        }
    }

    if let Some(age) = c.cache_age_ms.filter(|a| *a >= 0) {
        let color = if age >= CACHE_AGE_WARN_MS { YELLOW } else { COMMENT };
        out.push((
            "cache_age",
            format!(
                "{}{}",
                s.paint("cache_age:", COMMENT),
                s.paint(&fmt_duration(age as u64), color)
            ),
        ));
    }

    if let Some(name) = c.payload.model.as_ref().and_then(|m| m.display_name.as_deref()) {
        out.push(("model", s.paint(name, MAGENTA)));
    }

    if let Some(level) = c.payload.effort.as_ref().and_then(|e| e.level.as_deref()) {
        let color = match level {
            "high" | "xhigh" | "max" => Some(MAGENTA),
            "medium" | "low" => Some(COMMENT),
            _ => None, // outside the documented enum: hide
        };
        if let Some(color) = color {
            out.push(("effort", s.paint_bold(&format!("effort:{level}"), color)));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parse_payload;

    pub(crate) const PLAIN: Style = Style { colors: false, links: false };

    pub(crate) fn ctx_of<'a>(payload: &'a Payload, git: &'a GitInfo) -> Ctx<'a> {
        Ctx { payload, git, cache_age_ms: None, style: &PLAIN }
    }

    fn names(chips: &[(&'static str, String)]) -> Vec<&'static str> {
        chips.iter().map(|(n, _)| *n).collect()
    }

    fn text_of<'a>(chips: &'a [(&'static str, String)], name: &str) -> &'a str {
        &chips.iter().find(|(n, _)| *n == name).unwrap().1
    }

    #[test]
    fn full_line1_renders_all_chips_in_order() {
        let payload = parse_payload(
            r#"{
            "model": {"display_name": "Sonnet 5"},
            "effort": {"level": "xhigh"},
            "context_window": {
                "used_percentage": 42, "context_window_size": 1000000,
                "current_usage": {"input_tokens": 412000, "output_tokens": 18500,
                    "cache_creation_input_tokens": 12000, "cache_read_input_tokens": 365000}
            }
        }"#,
        )
        .unwrap();
        let git = GitInfo::default();
        let mut c = ctx_of(&payload, &git);
        c.cache_age_ms = Some(72_000);
        let chips = line1(&c);
        assert_eq!(
            names(&chips),
            vec!["bar", "context_tokens", "tokens", "cache", "cache_age", "model", "effort"]
        );
        assert_eq!(text_of(&chips, "context_tokens"), "ctx:420K/1M");
        assert_eq!(text_of(&chips, "tokens"), "in:412K out:18K");
        assert_eq!(text_of(&chips, "cache"), "cache:46%"); // 365000 / 789000
        assert_eq!(text_of(&chips, "cache_age"), "cache_age:1m12s");
        assert_eq!(text_of(&chips, "model"), "Sonnet 5");
        assert_eq!(text_of(&chips, "effort"), "effort:xhigh");
    }

    #[test]
    fn empty_payload_renders_nothing() {
        let payload = parse_payload("{}").unwrap();
        let git = GitInfo::default();
        assert!(line1(&ctx_of(&payload, &git)).is_empty());
    }

    #[test]
    fn medium_and_low_effort_render_dim_but_visible() {
        for level in ["medium", "low"] {
            let payload = parse_payload(&format!(r#"{{"effort": {{"level": "{level}"}}}}"#)).unwrap();
            let git = GitInfo::default();
            let chips = line1(&ctx_of(&payload, &git));
            assert_eq!(text_of(&chips, "effort"), &format!("effort:{level}"));
        }
    }

    #[test]
    fn unknown_effort_level_hides_chip() {
        let payload = parse_payload(r#"{"effort": {"level": "ultrathink"}}"#).unwrap();
        let git = GitInfo::default();
        assert!(line1(&ctx_of(&payload, &git)).is_empty());
    }

    #[test]
    fn missing_output_tokens_renders_question_mark() {
        let payload = parse_payload(
            r#"{"context_window": {"current_usage": {"input_tokens": 5000}}}"#,
        )
        .unwrap();
        let git = GitInfo::default();
        let chips = line1(&ctx_of(&payload, &git));
        assert_eq!(text_of(&chips, "tokens"), "in:5K out:?");
    }

    #[test]
    fn zero_cache_read_hides_cache_chip() {
        let payload = parse_payload(
            r#"{"context_window": {"current_usage": {"input_tokens": 5000, "output_tokens": 1, "cache_read_input_tokens": 0}}}"#,
        )
        .unwrap();
        let git = GitInfo::default();
        assert!(!names(&line1(&ctx_of(&payload, &git))).contains(&"cache"));
    }

    #[test]
    fn negative_cache_age_is_hidden_and_warn_color_kicks_in_at_5m() {
        let payload = parse_payload("{}").unwrap();
        let git = GitInfo::default();
        let mut c = ctx_of(&payload, &git);
        c.cache_age_ms = Some(-3_000);
        assert!(line1(&c).is_empty());

        let colored = Style { colors: true, links: false };
        let mut c = ctx_of(&payload, &git);
        c.style = &colored;
        c.cache_age_ms = Some(CACHE_AGE_WARN_MS);
        let chips = line1(&c);
        assert!(text_of(&chips, "cache_age").contains("\x1b[38;2;224;175;104m")); // yellow
    }

    #[test]
    fn percentage_clamped_before_ctx_derivation() {
        let payload = parse_payload(
            r#"{"context_window": {"used_percentage": 400, "context_window_size": 1000000}}"#,
        )
        .unwrap();
        let git = GitInfo::default();
        let chips = line1(&ctx_of(&payload, &git));
        assert_eq!(text_of(&chips, "context_tokens"), "ctx:1M/1M");
    }
}
