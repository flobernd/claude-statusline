use serde::Deserialize;

use crate::bar::bar_color;
use crate::format::{fmt_duration, fmt_tokens};
use crate::schema::Config;
use crate::schema::lenient;
use crate::theme::{BLUE, COMMENT, CYAN, MAGENTA, Style};

#[derive(Debug, Default, Deserialize)]
pub struct SubagentPayload {
    #[serde(default, deserialize_with = "lenient")]
    pub columns: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    tasks: Option<Vec<serde_json::Value>>,
}

/// Task fields arrive camelCase, unlike the snake_case main payload.
#[derive(Debug, Default, Deserialize)]
pub struct Task {
    #[serde(default, deserialize_with = "lenient")]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub name: Option<String>,
    #[serde(default, rename = "type", deserialize_with = "lenient")]
    pub task_type: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub label: Option<String>,
    #[serde(default, rename = "startTime", deserialize_with = "lenient")]
    pub start_time: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub model: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub effort: Option<String>,
    #[serde(default, rename = "contextWindowSize", deserialize_with = "lenient")]
    pub context_window_size: Option<f64>,
    #[serde(default, rename = "tokenCount", deserialize_with = "lenient")]
    pub token_count: Option<f64>,
}

impl SubagentPayload {
    /// Each entry parses on its own: one malformed task must not blank
    /// its neighbors' rows.
    pub fn parsed_tasks(&self) -> Vec<Task> {
        self.tasks
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|v| v.is_object())
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect()
    }
}

pub fn parse_payload(raw: &str) -> Option<SubagentPayload> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    if !value.is_object() {
        return None;
    }
    serde_json::from_value(value).ok()
}

/// Sanitized here (not just in paint) so dedup and width math in the
/// fitting step operate on the exact text that will reach the terminal.
pub fn name_text(task: &Task) -> Option<String> {
    [task.name.as_deref(), task.description.as_deref()]
        .into_iter()
        .flatten()
        .map(crate::theme::sanitize)
        .find(|t| !t.is_empty())
}

pub fn activity_text(task: &Task, name: Option<&str>) -> Option<String> {
    task.label
        .as_deref()
        .map(crate::theme::sanitize)
        .filter(|t| !t.is_empty() && Some(t.as_str()) != name)
}

pub fn row_chips(task: &Task, style: &Style, now_ms: i64) -> Vec<(&'static str, String)> {
    let s = style;
    let mut out: Vec<(&'static str, String)> = Vec::new();

    let name = name_text(task);
    if let Some(n) = name.as_deref() {
        out.push(("name", s.paint(n, CYAN)));
    }

    if let Some(a) = activity_text(task, name.as_deref()) {
        out.push(("activity", s.paint(&a, COMMENT)));
    }

    if let Some(win) = task.context_window_size.filter(|w| *w > 0.0) {
        let used = task.token_count.unwrap_or(0.0).clamp(0.0, win);
        let pct = used / win * 100.0;
        out.push((
            "context_tokens",
            format!(
                "{}{}{}{}{}{}",
                s.paint(&fmt_tokens(used.round() as u64), BLUE),
                s.paint("/", COMMENT),
                s.paint(&fmt_tokens(win as u64), BLUE),
                s.paint(" (", COMMENT),
                s.paint(&format!("{}%", pct.round() as u64), bar_color(pct)),
                s.paint(")", COMMENT),
            ),
        ));
    }

    if let Some(start) = task.start_time {
        let elapsed = now_ms - start as i64;
        if elapsed >= 0 {
            out.push(("elapsed", s.paint(&fmt_duration(elapsed as u64), COMMENT)));
        }
    }

    if let Some(model) = task.model.as_deref().filter(|m| !m.is_empty()) {
        out.push(("model", s.paint(model, MAGENTA)));
    }

    if let Some(level) = task.effort.as_deref() {
        let color = match level {
            "high" | "xhigh" | "max" => Some(MAGENTA),
            "medium" | "low" => Some(COMMENT),
            _ => None, // outside the documented enum: hide
        };
        if let Some(color) = color {
            out.push(("effort", s.paint_bold(level, color)));
        }
    }

    out
}

const SEP: &str = " \u{2502} ";
const DROP: &[&str] = &["effort", "model", "context_tokens", "elapsed"];
/// Below this many kept characters a truncated activity is noise.
const MIN_ACTIVITY: usize = 6;

fn row_width(chips: &[(&'static str, String)], sep_width: usize) -> usize {
    chips
        .iter()
        .map(|(_, r)| crate::fit::visible_width(r))
        .sum::<usize>()
        + sep_width * chips.len().saturating_sub(1)
}

/// Geometry shared by every shrink pass over one row.
struct RowBudget<'a> {
    columns: usize,
    sep_width: usize,
    style: &'a Style,
}

/// Shrink one chip's text to recover the overflow, ellipsis-terminated;
/// the chip is removed instead when it would fall below its minimum.
fn shrink_chip(
    chips: &mut Vec<(&'static str, String)>,
    chip: &'static str,
    raw: &str,
    color: crate::theme::Rgb,
    min_keep: usize,
    drop_when_short: bool,
    b: &RowBudget,
) {
    let over = row_width(chips, b.sep_width).saturating_sub(b.columns);
    if over == 0 {
        return;
    }
    let Some(pos) = chips.iter().position(|(n, _)| *n == chip) else {
        return;
    };
    let width = crate::fit::visible_width(&chips[pos].1);
    let keep = width.saturating_sub(over + 1); // one cell for the ellipsis
    if keep < min_keep && drop_when_short {
        chips.remove(pos);
        return;
    }
    let text: String = raw
        .chars()
        .take(keep.max(min_keep))
        .chain(std::iter::once('\u{2026}'))
        .collect();
    chips[pos].1 = b.style.paint(&text, color);
}

pub fn render_row(
    task: &Task,
    columns: usize,
    style: &Style,
    disabled: &[String],
    now_ms: i64,
) -> Option<String> {
    let chips: Vec<(&'static str, String)> = row_chips(task, style, now_ms)
        .into_iter()
        .filter(|(name, _)| !disabled.iter().any(|d| d == name))
        .collect();
    if chips.is_empty() {
        return None;
    }
    let sep = style.paint(SEP, COMMENT);
    let sep_width = crate::fit::visible_width(&sep);
    let mut chips = crate::fit::fit_line(chips, sep_width, columns, DROP);

    let budget = RowBudget {
        columns,
        sep_width,
        style,
    };
    let name = name_text(task);
    if let Some(a) = activity_text(task, name.as_deref()) {
        shrink_chip(
            &mut chips,
            "activity",
            &a,
            COMMENT,
            MIN_ACTIVITY,
            true,
            &budget,
        );
    }
    if let Some(n) = name.as_deref() {
        shrink_chip(&mut chips, "name", n, CYAN, 1, false, &budget);
    }

    Some(
        chips
            .into_iter()
            .map(|(_, r)| r)
            .collect::<Vec<_>>()
            .join(&sep),
    )
}

pub fn render(raw: &str, config: &Config, style: &Style, fallback_width: usize) -> Option<String> {
    let Some(payload) = parse_payload(raw) else {
        eprintln!("claude-statusline: undecodable subagent payload");
        return None;
    };
    let columns = payload
        .columns
        .filter(|c| (10.0..=4000.0).contains(c))
        .map(|c| c as usize)
        .unwrap_or(fallback_width);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let mut lines: Vec<String> = Vec::new();
    for task in payload.parsed_tasks() {
        // Other task types keep Claude Code's default row.
        if task.task_type.as_deref() != Some("local_agent") {
            continue;
        }
        let Some(id) = task.id.as_deref().filter(|i| !i.is_empty()) else {
            continue;
        };
        let Some(content) = render_row(
            &task,
            columns,
            style,
            &config.subagent_disabled_sections,
            now_ms,
        ) else {
            continue;
        };
        // serde_json encodes the ESC byte as a \u001b escape, which
        // Claude Code's JSON.parse restores verbatim.
        lines.push(serde_json::json!({"id": id, "content": content}).to_string());
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_case_task_fields_parse() {
        let p = parse_payload(
            r#"{
            "columns": 120,
            "tasks": [{
                "id": "t1", "type": "local_agent", "name": "Explore",
                "description": "Find callers", "label": "Searching",
                "startTime": 1737648000000, "model": "claude-sonnet-5",
                "effort": "high", "contextWindowSize": 200000,
                "tokenCount": 82000
            }]
        }"#,
        )
        .unwrap();
        assert_eq!(p.columns, Some(120.0));
        let tasks = p.parsed_tasks();
        assert_eq!(tasks.len(), 1);
        let t = &tasks[0];
        assert_eq!(t.id.as_deref(), Some("t1"));
        assert_eq!(t.task_type.as_deref(), Some("local_agent"));
        assert_eq!(t.start_time, Some(1_737_648_000_000.0));
        assert_eq!(t.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(t.context_window_size, Some(200_000.0));
        assert_eq!(t.token_count, Some(82_000.0));
    }

    #[test]
    fn wrong_typed_task_field_becomes_none_without_killing_neighbors() {
        let p =
            parse_payload(r#"{"tasks": [{"id": "t1", "startTime": "garbage", "tokenCount": 5}]}"#)
                .unwrap();
        let t = &p.parsed_tasks()[0];
        assert_eq!(t.start_time, None);
        assert_eq!(t.token_count, Some(5.0));
    }

    #[test]
    fn non_object_task_entries_are_skipped() {
        let p = parse_payload(r#"{"tasks": [42, {"id": "t2"}, "x"]}"#).unwrap();
        let tasks = p.parsed_tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id.as_deref(), Some("t2"));
    }

    #[test]
    fn wrong_typed_tasks_or_columns_survive_leniently() {
        let p = parse_payload(r#"{"columns": "wide", "tasks": "none"}"#).unwrap();
        assert_eq!(p.columns, None);
        assert!(p.parsed_tasks().is_empty());
    }

    #[test]
    fn undecodable_or_non_object_payload_is_none() {
        assert!(parse_payload("not json").is_none());
        assert!(parse_payload("[1]").is_none());
    }

    use crate::theme::Style;

    const PLAIN: Style = Style {
        colors: false,
        links: false,
    };

    fn task(json: &str) -> Task {
        serde_json::from_str(json).unwrap()
    }

    fn names(chips: &[(&'static str, String)]) -> Vec<&'static str> {
        chips.iter().map(|(n, _)| *n).collect()
    }

    fn text_of<'a>(chips: &'a [(&'static str, String)], name: &str) -> &'a str {
        &chips.iter().find(|(n, _)| *n == name).unwrap().1
    }

    const FULL_TASK: &str = r#"{
    "id": "t1", "type": "local_agent", "name": "Explore",
    "description": "Find callers", "label": "Reading the source tree",
    "startTime": 1000, "model": "claude-sonnet-5", "effort": "high",
    "contextWindowSize": 200000, "tokenCount": 82000
}"#;

    #[test]
    fn full_task_renders_all_chips_in_order() {
        let chips = row_chips(&task(FULL_TASK), &PLAIN, 24_000);
        assert_eq!(
            names(&chips),
            vec![
                "name",
                "activity",
                "context_tokens",
                "elapsed",
                "model",
                "effort"
            ]
        );
        assert_eq!(text_of(&chips, "name"), "Explore");
        assert_eq!(text_of(&chips, "activity"), "Reading the source tree");
        assert_eq!(text_of(&chips, "context_tokens"), "82K/200K (41%)");
        assert_eq!(text_of(&chips, "elapsed"), "23s");
        assert_eq!(text_of(&chips, "model"), "claude-sonnet-5");
        assert_eq!(text_of(&chips, "effort"), "high");
    }

    #[test]
    fn name_falls_back_to_description() {
        let chips = row_chips(&task(r#"{"description": "Find callers"}"#), &PLAIN, 0);
        assert_eq!(text_of(&chips, "name"), "Find callers");
    }

    #[test]
    fn activity_equal_to_name_is_dropped() {
        let chips = row_chips(
            &task(r#"{"name": "Explore", "label": "Explore"}"#),
            &PLAIN,
            0,
        );
        assert_eq!(names(&chips), vec!["name"]);
        // Also when the shown name came from the description.
        let chips = row_chips(
            &task(r#"{"description": "Find callers", "label": "Find callers"}"#),
            &PLAIN,
            0,
        );
        assert_eq!(names(&chips), vec!["name"]);
    }

    #[test]
    fn token_count_clamps_to_window_and_missing_window_hides_chip() {
        let chips = row_chips(
            &task(r#"{"contextWindowSize": 200000, "tokenCount": 900000}"#),
            &PLAIN,
            0,
        );
        assert_eq!(text_of(&chips, "context_tokens"), "200K/200K (100%)");
        let chips = row_chips(&task(r#"{"tokenCount": 5000}"#), &PLAIN, 0);
        assert!(!names(&chips).contains(&"context_tokens"));
        let chips = row_chips(
            &task(r#"{"contextWindowSize": 0, "tokenCount": 5000}"#),
            &PLAIN,
            0,
        );
        assert!(!names(&chips).contains(&"context_tokens"));
    }

    #[test]
    fn missing_start_or_negative_elapsed_hides_elapsed() {
        let chips = row_chips(&task(r#"{"name": "x"}"#), &PLAIN, 50_000);
        assert!(!names(&chips).contains(&"elapsed"));
        let chips = row_chips(&task(r#"{"startTime": 60000}"#), &PLAIN, 50_000);
        assert!(!names(&chips).contains(&"elapsed"));
    }

    #[test]
    fn unknown_effort_hides_and_known_levels_render_bare() {
        let chips = row_chips(&task(r#"{"effort": "ultrathink"}"#), &PLAIN, 0);
        assert!(!names(&chips).contains(&"effort"));
        let chips = row_chips(&task(r#"{"effort": "medium"}"#), &PLAIN, 0);
        assert_eq!(text_of(&chips, "effort"), "medium");
    }

    #[test]
    fn context_percentage_uses_band_colors() {
        let colored = Style {
            colors: true,
            links: false,
        };
        let chips = row_chips(
            &task(r#"{"contextWindowSize": 200000, "tokenCount": 180000}"#),
            &colored,
            0,
        );
        // 90% sits in the red band.
        assert!(text_of(&chips, "context_tokens").contains("\x1b[38;2;247;118;142m90%"));
    }

    #[test]
    fn empty_task_renders_no_chips() {
        assert!(row_chips(&task("{}"), &PLAIN, 0).is_empty());
    }

    fn row_at(columns: usize) -> String {
        render_row(&task(FULL_TASK), columns, &PLAIN, &[], 24_000).unwrap()
    }

    #[test]
    fn wide_row_renders_all_chips_joined() {
        assert_eq!(
            row_at(81),
            "Explore \u{2502} Reading the source tree \u{2502} 82K/200K (41%) \u{2502} 23s \u{2502} claude-sonnet-5 \u{2502} high"
        );
    }

    #[test]
    fn chips_drop_in_spec_order_under_pressure() {
        let r = row_at(80); // forces effort out (74 fits)
        assert!(
            !r.contains("high") && r.contains("claude-sonnet-5"),
            "row: {r}"
        );
        let r = row_at(73); // then model (56 fits)
        assert!(
            !r.contains("claude-sonnet-5") && r.contains("82K/200K"),
            "row: {r}"
        );
        let r = row_at(55); // then context_tokens (39 fits)
        assert!(!r.contains("82K/200K") && r.contains("23s"), "row: {r}");
        let r = row_at(38); // then elapsed (33 fits)
        assert!(
            !r.contains("23s") && r.contains("Reading the source tree"),
            "row: {r}"
        );
    }

    #[test]
    fn activity_truncates_with_ellipsis_then_drops() {
        let r = row_at(30);
        assert_eq!(r, "Explore \u{2502} Reading the source \u{2026}");
        assert_eq!(crate::fit::visible_width(&r), 30);
        // Fewer than 6 chars would remain: the chip goes instead.
        assert_eq!(row_at(12), "Explore");
    }

    #[test]
    fn name_truncates_as_last_resort_but_never_drops() {
        let r = render_row(&task(r#"{"name": "Explore"}"#), 5, &PLAIN, &[], 0).unwrap();
        assert_eq!(r, "Expl\u{2026}");
    }

    #[test]
    fn disabled_sections_filter_chips_and_empty_row_is_none() {
        let disabled = vec!["activity".to_string(), "model".to_string()];
        let r = render_row(&task(FULL_TASK), 200, &PLAIN, &disabled, 24_000).unwrap();
        assert!(!r.contains("Reading") && !r.contains("claude-sonnet-5"));
        assert!(r.contains("Explore") && r.contains("82K/200K"));

        let all: Vec<String> = [
            "name",
            "activity",
            "context_tokens",
            "elapsed",
            "model",
            "effort",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(render_row(&task(FULL_TASK), 200, &PLAIN, &all, 24_000).is_none());
    }
}
