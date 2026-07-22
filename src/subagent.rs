#![allow(dead_code)]

use serde::Deserialize;

use crate::schema::lenient;

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
}
