use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use std::path::{Path, PathBuf};

/// Field-level leniency: a wrong-typed field becomes None instead of
/// failing the whole payload. The statusline must render whatever
/// upstream got right, not blank on the first field it got wrong.
pub(crate) fn lenient<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).ok())
}

#[derive(Debug, Default, Deserialize)]
pub struct Payload {
    #[serde(default, deserialize_with = "lenient")]
    pub cwd: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub transcript_path: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub model: Option<Model>,
    #[serde(default, deserialize_with = "lenient")]
    pub workspace: Option<Workspace>,
    #[serde(default, deserialize_with = "lenient")]
    pub context_window: Option<ContextWindow>,
    #[serde(default, deserialize_with = "lenient")]
    pub effort: Option<Effort>,
    #[serde(default, deserialize_with = "lenient")]
    pub pr: Option<Pr>,
    #[serde(default, deserialize_with = "lenient")]
    pub worktree: Option<Worktree>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Model {
    #[serde(default, deserialize_with = "lenient")]
    pub display_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Workspace {
    #[serde(default, deserialize_with = "lenient")]
    pub current_dir: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub project_dir: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub git_worktree: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "lenient")]
    pub repo: Option<Repo>,
}

impl Workspace {
    /// Upstream docs type git_worktree as a string but older payloads used
    /// a bool; any non-null value means "inside a linked worktree".
    pub fn git_worktree_present(&self) -> bool {
        self.git_worktree
            .as_ref()
            .is_some_and(|v| !v.is_null() && v.as_bool() != Some(false))
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct Repo {
    #[serde(default, deserialize_with = "lenient")]
    pub host: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub owner: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ContextWindow {
    #[serde(default, deserialize_with = "lenient")]
    pub used_percentage: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub context_window_size: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub current_usage: Option<Usage>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Usage {
    #[serde(default, deserialize_with = "lenient")]
    pub input_tokens: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub cache_creation_input_tokens: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub cache_read_input_tokens: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Effort {
    #[serde(default, deserialize_with = "lenient")]
    pub level: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Pr {
    #[serde(default, deserialize_with = "lenient")]
    pub number: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub review_state: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Worktree {
    #[serde(default, deserialize_with = "lenient")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub branch: Option<String>,
}

pub fn parse_payload(raw: &str) -> Option<Payload> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    if !value.is_object() {
        return None;
    }
    serde_json::from_value(value).ok()
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub clickable_links: bool,
    pub disabled_sections: Vec<String>,
    pub subagent_disabled_sections: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            clickable_links: true,
            disabled_sections: Vec::new(),
            subagent_disabled_sections: Vec::new(),
        }
    }
}

pub fn load_config(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(config) => config,
            Err(e) => {
                eprintln!(
                    "claude-statusline: ignoring malformed config {}: {e}",
                    path.display()
                );
                Config::default()
            }
        },
        Err(_) => Config::default(),
    }
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_payload_parses() {
        let raw = r#"{
            "cwd": "/home/u/proj",
            "transcript_path": "/home/u/.claude/projects/p/s.jsonl",
            "model": {"id": "claude-sonnet-5", "display_name": "Sonnet 5"},
            "workspace": {
                "current_dir": "/home/u/proj/src",
                "project_dir": "/home/u/proj",
                "git_worktree": "wt-name",
                "repo": {"host": "github.com", "owner": "user", "name": "proj"}
            },
            "context_window": {
                "used_percentage": 42,
                "context_window_size": 1000000,
                "current_usage": {
                    "input_tokens": 412000, "output_tokens": 18500,
                    "cache_creation_input_tokens": 12000,
                    "cache_read_input_tokens": 365000
                }
            },
            "effort": {"level": "xhigh"},
            "pr": {"number": 86, "url": "https://github.com/user/proj/pull/86", "review_state": "approved"},
            "worktree": {"name": "fix", "branch": "fix/bug-123"}
        }"#;
        let p = parse_payload(raw).unwrap();
        assert_eq!(p.model.unwrap().display_name.as_deref(), Some("Sonnet 5"));
        let cw = p.context_window.unwrap();
        assert_eq!(cw.used_percentage, Some(42.0));
        assert_eq!(
            cw.current_usage.unwrap().cache_read_input_tokens,
            Some(365_000.0)
        );
        assert_eq!(p.effort.unwrap().level.as_deref(), Some("xhigh"));
        assert_eq!(p.pr.as_ref().unwrap().number, Some(86));
        assert!(p.workspace.unwrap().git_worktree_present());
    }

    #[test]
    fn wrong_typed_fields_become_none_without_killing_neighbors() {
        let raw = r#"{
            "model": {"display_name": 42},
            "context_window": {"used_percentage": "garbage", "context_window_size": 200000},
            "effort": "high"
        }"#;
        let p = parse_payload(raw).unwrap();
        assert_eq!(p.model.unwrap().display_name, None);
        let cw = p.context_window.unwrap();
        assert_eq!(cw.used_percentage, None);
        assert_eq!(cw.context_window_size, Some(200_000.0));
        assert!(p.effort.is_none());
    }

    #[test]
    fn undecodable_or_non_object_payload_is_none() {
        assert!(parse_payload("not json").is_none());
        assert!(parse_payload("[1, 2]").is_none());
        assert!(parse_payload("{\"a\": NaN}").is_none());
    }

    #[test]
    fn empty_object_parses_to_all_none() {
        let p = parse_payload("{}").unwrap();
        assert!(p.model.is_none() && p.workspace.is_none() && p.context_window.is_none());
    }

    #[test]
    fn pr_number_rejects_fractional_values() {
        let p = parse_payload(r#"{"pr": {"number": 86.5}}"#).unwrap();
        assert_eq!(p.pr.unwrap().number, None);
    }

    #[test]
    fn git_worktree_false_or_null_is_absent() {
        let p = parse_payload(r#"{"workspace": {"git_worktree": false}}"#).unwrap();
        assert!(!p.workspace.unwrap().git_worktree_present());
        let p = parse_payload(r#"{"workspace": {"git_worktree": null}}"#).unwrap();
        assert!(!p.workspace.unwrap().git_worktree_present());
    }

    #[test]
    fn config_defaults_and_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline.json");
        let c = load_config(&path);
        assert!(c.clickable_links && c.disabled_sections.is_empty());

        std::fs::write(&path, r#"{"clickable_links": false}"#).unwrap();
        let c = load_config(&path);
        assert!(!c.clickable_links);

        std::fs::write(&path, "{broken").unwrap();
        let c = load_config(&path);
        assert!(c.clickable_links);
    }

    #[test]
    fn subagent_disabled_sections_parse_and_default_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline.json");
        assert!(load_config(&path).subagent_disabled_sections.is_empty());

        std::fs::write(
            &path,
            r#"{"subagent_disabled_sections": ["activity", "effort"]}"#,
        )
        .unwrap();
        let c = load_config(&path);
        assert_eq!(c.subagent_disabled_sections, vec!["activity", "effort"]);
        // The new key must not disturb the other defaults.
        assert!(c.clickable_links && c.disabled_sections.is_empty());
    }
}
