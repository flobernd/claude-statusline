use serde::Deserialize;

use crate::bar::bar_color;
use crate::format::{fmt_duration, fmt_tokens};
use crate::schema::Config;
use crate::schema::lenient;
use crate::theme::{BLUE, COMMENT, CYAN, GREEN, MAGENTA, Style, WHITE};

#[derive(Debug, Default, Deserialize)]
pub struct SubagentPayload {
    #[serde(default, deserialize_with = "lenient")]
    pub columns: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub cwd: Option<String>,
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
    #[serde(default, deserialize_with = "lenient")]
    pub cwd: Option<String>,
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

pub fn row_chips(
    task: &Task,
    style: &Style,
    now_ms: i64,
    location: &TaskLocation,
) -> Vec<(&'static str, String)> {
    let s = style;
    let mut out: Vec<(&'static str, String)> = Vec::new();

    let name = name_text(task);
    if let Some(n) = name.as_deref() {
        out.push(("name", s.paint(n, WHITE)));
    }

    match location {
        TaskLocation::Repo { repo, branch } => {
            let branch_color = if branch == "main" || branch == "master" {
                GREEN
            } else {
                MAGENTA
            };
            out.push((
                "branch",
                format!(
                    "{}{}",
                    s.paint(&format!("\u{2387} {repo}"), CYAN),
                    s.paint(&format!("/{branch}"), branch_color),
                ),
            ));
        }
        TaskLocation::Dir(folder) => {
            out.push(("cwd", s.paint(&format!("\u{2302} {folder}"), CYAN)));
        }
        TaskLocation::Same => {}
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
    location: &TaskLocation,
) -> Option<String> {
    let mut chips: Vec<(&'static str, String)> = row_chips(task, style, now_ms, location)
        .into_iter()
        .filter(|(name, _)| !disabled.iter().any(|d| d == name))
        .collect();
    if chips.is_empty() {
        return None;
    }
    let sep = style.paint(SEP, COMMENT);
    let sep_width = crate::fit::visible_width(&sep);
    let budget = RowBudget {
        columns,
        sep_width,
        style,
    };

    // The activity text is the first thing to give way: it shrinks to its
    // floor and then disappears before any metric chip is touched.
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
    let mut chips = crate::fit::fit_line(chips, sep_width, columns, DROP);

    let location_raw = match location {
        TaskLocation::Repo { repo, branch } => Some(format!("\u{2387} {repo}/{branch}")),
        TaskLocation::Dir(folder) => Some(format!("\u{2302} {folder}")),
        TaskLocation::Same => None,
    };
    if let Some(raw) = location_raw.as_deref() {
        let chip = if matches!(location, TaskLocation::Repo { .. }) {
            "branch"
        } else {
            "cwd"
        };
        // Last-resort truncation repaints single-tone cyan; the two-tone
        // split is not worth preserving at widths this tight.
        shrink_chip(&mut chips, chip, raw, CYAN, 4, false, &budget);
    }

    if let Some(n) = name.as_deref() {
        shrink_chip(&mut chips, "name", n, WHITE, 1, false, &budget);
    }

    Some(
        chips
            .into_iter()
            .map(|(_, r)| r)
            .collect::<Vec<_>>()
            .join(&sep),
    )
}

pub fn sample_task() -> Task {
    serde_json::from_str(
        r#"{
        "id": "t1", "type": "local_agent", "name": "Explore",
        "label": "Searching for callers", "startTime": 0,
        "model": "claude-sonnet-5", "contextWindowSize": 200000,
        "tokenCount": 82000
    }"#,
    )
    .expect("sample task is valid")
}

pub fn preview(style: &Style) -> String {
    // Fixed now_ms so the sample elapsed chip is stable: 1m23s.
    render_row(&sample_task(), 100, style, &[], 83_000, &TaskLocation::Same).unwrap_or_default()
}

#[derive(Debug)]
pub enum TaskLocation {
    /// Same location as the session: no chip.
    Same,
    Repo {
        repo: String,
        branch: String,
    },
    Dir(String), // last path component of the task cwd
}

fn last_component(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|c| !c.is_empty())
        .unwrap_or(path)
        .to_string()
}

pub fn resolve_location(
    task_cwd: Option<&str>,
    session_cwd: Option<&str>,
    lookup: &mut dyn FnMut(&str) -> Option<(String, String)>,
) -> TaskLocation {
    let Some(task_cwd) = task_cwd.filter(|c| !c.is_empty()) else {
        return TaskLocation::Same;
    };
    if Some(task_cwd) == session_cwd {
        return TaskLocation::Same;
    }
    let task_loc = lookup(task_cwd);
    let session_loc = match session_cwd {
        Some(c) => lookup(c),
        None => None,
    };
    match task_loc {
        Some((repo, branch)) => {
            if session_loc.as_ref() == Some(&(repo.clone(), branch.clone())) {
                TaskLocation::Same
            } else {
                TaskLocation::Repo { repo, branch }
            }
        }
        None => TaskLocation::Dir(last_component(task_cwd)),
    }
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

    let session_cwd = payload.cwd.clone().or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    });
    // One cache for the whole invocation: sibling tasks in the same repo or
    // worktree reuse a single git lookup instead of spawning one each.
    let mut cache: std::collections::HashMap<String, Option<(String, String)>> =
        std::collections::HashMap::new();
    let mut lookup = |cwd: &str| {
        cache
            .entry(cwd.to_string())
            .or_insert_with(|| crate::git::branch_location(std::path::Path::new(cwd)))
            .clone()
    };

    let mut lines: Vec<String> = Vec::new();
    for task in payload.parsed_tasks() {
        // Other task types keep Claude Code's default row.
        if task.task_type.as_deref() != Some("local_agent") {
            continue;
        }
        let Some(id) = task.id.as_deref().filter(|i| !i.is_empty()) else {
            continue;
        };
        let location = resolve_location(task.cwd.as_deref(), session_cwd.as_deref(), &mut lookup);
        let Some(content) = render_row(
            &task,
            columns,
            style,
            &config.subagent_disabled_sections,
            now_ms,
            &location,
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
        let chips = row_chips(&task(FULL_TASK), &PLAIN, 24_000, &TaskLocation::Same);
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
        let chips = row_chips(
            &task(r#"{"description": "Find callers"}"#),
            &PLAIN,
            0,
            &TaskLocation::Same,
        );
        assert_eq!(text_of(&chips, "name"), "Find callers");
    }

    #[test]
    fn activity_equal_to_name_is_dropped() {
        let chips = row_chips(
            &task(r#"{"name": "Explore", "label": "Explore"}"#),
            &PLAIN,
            0,
            &TaskLocation::Same,
        );
        assert_eq!(names(&chips), vec!["name"]);
        // Also when the shown name came from the description.
        let chips = row_chips(
            &task(r#"{"description": "Find callers", "label": "Find callers"}"#),
            &PLAIN,
            0,
            &TaskLocation::Same,
        );
        assert_eq!(names(&chips), vec!["name"]);
    }

    #[test]
    fn token_count_clamps_to_window_and_missing_window_hides_chip() {
        let chips = row_chips(
            &task(r#"{"contextWindowSize": 200000, "tokenCount": 900000}"#),
            &PLAIN,
            0,
            &TaskLocation::Same,
        );
        assert_eq!(text_of(&chips, "context_tokens"), "200K/200K (100%)");
        let chips = row_chips(
            &task(r#"{"tokenCount": 5000}"#),
            &PLAIN,
            0,
            &TaskLocation::Same,
        );
        assert!(!names(&chips).contains(&"context_tokens"));
        let chips = row_chips(
            &task(r#"{"contextWindowSize": 0, "tokenCount": 5000}"#),
            &PLAIN,
            0,
            &TaskLocation::Same,
        );
        assert!(!names(&chips).contains(&"context_tokens"));
    }

    #[test]
    fn missing_start_or_negative_elapsed_hides_elapsed() {
        let chips = row_chips(
            &task(r#"{"name": "x"}"#),
            &PLAIN,
            50_000,
            &TaskLocation::Same,
        );
        assert!(!names(&chips).contains(&"elapsed"));
        let chips = row_chips(
            &task(r#"{"startTime": 60000}"#),
            &PLAIN,
            50_000,
            &TaskLocation::Same,
        );
        assert!(!names(&chips).contains(&"elapsed"));
    }

    #[test]
    fn unknown_effort_hides_and_known_levels_render_bare() {
        let chips = row_chips(
            &task(r#"{"effort": "ultrathink"}"#),
            &PLAIN,
            0,
            &TaskLocation::Same,
        );
        assert!(!names(&chips).contains(&"effort"));
        let chips = row_chips(
            &task(r#"{"effort": "medium"}"#),
            &PLAIN,
            0,
            &TaskLocation::Same,
        );
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
            &TaskLocation::Same,
        );
        // 90% sits in the red band.
        assert!(text_of(&chips, "context_tokens").contains("\x1b[38;2;247;118;142m90%"));
    }

    #[test]
    fn empty_task_renders_no_chips() {
        assert!(row_chips(&task("{}"), &PLAIN, 0, &TaskLocation::Same).is_empty());
    }

    #[test]
    fn location_chip_sits_after_name_and_same_hides_it() {
        let loc = TaskLocation::Repo {
            repo: "myrepo".to_string(),
            branch: "fix-1".to_string(),
        };
        let chips = row_chips(&task(FULL_TASK), &PLAIN, 24_000, &loc);
        assert_eq!(names(&chips)[..2], ["name", "branch"]);
        assert_eq!(text_of(&chips, "branch"), "\u{2387} myrepo/fix-1");

        let chips = row_chips(&task(FULL_TASK), &PLAIN, 24_000, &TaskLocation::Same);
        assert!(!names(&chips).contains(&"branch") && !names(&chips).contains(&"cwd"));
    }

    #[test]
    fn dir_location_renders_cwd_chip() {
        let loc = TaskLocation::Dir("scratch-dir".to_string());
        let chips = row_chips(&task(r#"{"name": "builder"}"#), &PLAIN, 0, &loc);
        assert_eq!(names(&chips), vec!["name", "cwd"]);
        assert_eq!(text_of(&chips, "cwd"), "\u{2302} scratch-dir");
    }

    #[test]
    fn branch_chip_color_codes_default_and_feature_branches() {
        let colored = Style {
            colors: true,
            links: false,
        };
        let main_loc = TaskLocation::Repo {
            repo: "r".to_string(),
            branch: "master".to_string(),
        };
        let chips = row_chips(&task(r#"{"name": "x"}"#), &colored, 0, &main_loc);
        assert!(text_of(&chips, "branch").contains("\x1b[38;2;158;206;106m/master")); // green
        let feat_loc = TaskLocation::Repo {
            repo: "r".to_string(),
            branch: "feat/x".to_string(),
        };
        let chips = row_chips(&task(r#"{"name": "x"}"#), &colored, 0, &feat_loc);
        assert!(text_of(&chips, "branch").contains("\x1b[38;2;187;154;247m/feat/x")); // magenta
    }

    #[test]
    fn location_chip_never_drops_and_truncates_before_name() {
        let loc = TaskLocation::Repo {
            repo: "myrepo".to_string(),
            branch: "fix-1".to_string(),
        };
        // name 7 + sep 3 + location 14 = 24; at 20 the location truncates
        // (name intact), at tiny widths both sit at their floors.
        let t = task(r#"{"name": "Explore"}"#);
        let r = render_row(&t, 20, &PLAIN, &[], 0, &loc).unwrap();
        assert_eq!(r, "Explore \u{2502} \u{2387} myrepo/\u{2026}");
        assert_eq!(crate::fit::visible_width(&r), 20);
        let r = render_row(&t, 200, &PLAIN, &[], 0, &loc).unwrap();
        assert_eq!(r, "Explore \u{2502} \u{2387} myrepo/fix-1");
    }

    #[test]
    fn name_chip_renders_white() {
        let colored = Style {
            colors: true,
            links: false,
        };
        let chips = row_chips(
            &task(r#"{"name": "Explore"}"#),
            &colored,
            0,
            &TaskLocation::Same,
        );
        assert!(text_of(&chips, "name").starts_with("\x1b[38;2;255;255;255m"));
    }

    #[test]
    fn preview_renders_the_sample_row() {
        assert_eq!(
            preview(&PLAIN),
            "Explore \u{2502} Searching for callers \u{2502} 82K/200K (41%) \u{2502} 1m23s \u{2502} claude-sonnet-5"
        );
    }

    fn row_at(columns: usize) -> String {
        render_row(
            &task(FULL_TASK),
            columns,
            &PLAIN,
            &[],
            24_000,
            &TaskLocation::Same,
        )
        .unwrap()
    }

    #[test]
    fn wide_row_renders_all_chips_joined() {
        assert_eq!(
            row_at(81),
            "Explore \u{2502} Reading the source tree \u{2502} 82K/200K (41%) \u{2502} 23s \u{2502} claude-sonnet-5 \u{2502} high"
        );
    }

    #[test]
    fn activity_gives_way_before_any_chip() {
        // One cell over: the activity shrinks, every chip stays.
        let r = row_at(80);
        assert!(r.contains('\u{2026}') && r.contains("high"), "row: {r}");
        assert_eq!(crate::fit::visible_width(&r), 80);
        // At its 6-char floor the chips are still intact.
        let r = row_at(65);
        assert_eq!(
            r,
            "Explore \u{2502} Readin\u{2026} \u{2502} 82K/200K (41%) \u{2502} 23s \u{2502} claude-sonnet-5 \u{2502} high"
        );
        // Below the floor the whole activity chip goes before any metric.
        let r = row_at(64);
        assert_eq!(
            r,
            "Explore \u{2502} 82K/200K (41%) \u{2502} 23s \u{2502} claude-sonnet-5 \u{2502} high"
        );
    }

    #[test]
    fn chips_drop_in_spec_order_after_activity() {
        let r = row_at(54); // effort drops once the activity is gone (48 fits)
        assert!(
            !r.contains("high") && r.contains("claude-sonnet-5"),
            "row: {r}"
        );
        let r = row_at(47); // then model (30 fits)
        assert!(
            !r.contains("claude-sonnet-5") && r.contains("82K/200K"),
            "row: {r}"
        );
        let r = row_at(29); // then context_tokens (13 fits)
        assert!(!r.contains("82K/200K") && r.contains("23s"), "row: {r}");
        let r = row_at(12); // then elapsed (7 fits)
        assert_eq!(r, "Explore");
    }

    #[test]
    fn name_truncates_as_last_resort_but_never_drops() {
        let r = render_row(
            &task(r#"{"name": "Explore"}"#),
            5,
            &PLAIN,
            &[],
            0,
            &TaskLocation::Same,
        )
        .unwrap();
        assert_eq!(r, "Expl\u{2026}");
    }

    #[test]
    fn disabled_sections_filter_chips_and_empty_row_is_none() {
        let disabled = vec!["activity".to_string(), "model".to_string()];
        let r = render_row(
            &task(FULL_TASK),
            200,
            &PLAIN,
            &disabled,
            24_000,
            &TaskLocation::Same,
        )
        .unwrap();
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
        assert!(
            render_row(
                &task(FULL_TASK),
                200,
                &PLAIN,
                &all,
                24_000,
                &TaskLocation::Same
            )
            .is_none()
        );
    }

    #[test]
    fn location_same_for_missing_equal_or_same_branch_cwd() {
        let mut repo_everywhere = |_: &str| Some(("myrepo".to_string(), "main".to_string()));
        assert!(matches!(
            resolve_location(None, Some("/a"), &mut repo_everywhere),
            TaskLocation::Same
        ));
        assert!(matches!(
            resolve_location(Some("/a"), Some("/a"), &mut repo_everywhere),
            TaskLocation::Same
        ));
        // Different dirs inside the same repo and branch stay quiet.
        assert!(matches!(
            resolve_location(Some("/a/src"), Some("/a"), &mut repo_everywhere),
            TaskLocation::Same
        ));
    }

    #[test]
    fn location_repo_when_branch_differs() {
        let mut lookup = |cwd: &str| match cwd {
            "/repo" => Some(("myrepo".to_string(), "master".to_string())),
            _ => Some(("myrepo".to_string(), "fix-1".to_string())),
        };
        match resolve_location(Some("/repo/.wt/fix-1"), Some("/repo"), &mut lookup) {
            TaskLocation::Repo { repo, branch } => {
                assert_eq!(repo, "myrepo");
                assert_eq!(branch, "fix-1");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn location_dir_uses_last_path_component() {
        let mut no_repo = |_: &str| None;
        match resolve_location(Some("/tmp/scratch-dir"), Some("/home/u"), &mut no_repo) {
            TaskLocation::Dir(d) => assert_eq!(d, "scratch-dir"),
            other => panic!("unexpected: {other:?}"),
        }
        match resolve_location(Some("C:\\work\\jobs"), Some("/home/u"), &mut no_repo) {
            TaskLocation::Dir(d) => assert_eq!(d, "jobs"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
