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

/// A list parses element by element: a malformed entry becomes nothing rather than failing the
/// whole list, and a value that is not a list at all reads as empty.
pub(crate) fn lenient_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let v = serde_json::Value::deserialize(d)?;
    let serde_json::Value::Array(items) = v else {
        return Ok(Vec::new());
    };
    Ok(items
        .into_iter()
        .filter_map(|item| serde_json::from_value(item).ok())
        .collect())
}

#[derive(Debug, Default, Deserialize)]
pub struct Payload {
    #[serde(default, deserialize_with = "lenient")]
    pub cwd: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub transcript_path: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub session_id: Option<String>,
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
    #[serde(default, deserialize_with = "lenient")]
    pub rate_limits: Option<RateLimits>,
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

#[derive(Debug, Default, Deserialize)]
pub struct RateLimits {
    #[serde(default, deserialize_with = "lenient")]
    pub five_hour: Option<RateWindow>,
    #[serde(default, deserialize_with = "lenient")]
    pub seven_day: Option<RateWindow>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RateWindow {
    #[serde(default, deserialize_with = "lenient")]
    pub used_percentage: Option<f64>,
    /// Epoch seconds.
    #[serde(default, deserialize_with = "lenient")]
    pub resets_at: Option<f64>,
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
    pub advanced_usage_limits_enabled: bool,
    pub cli_proxy_usage_enabled: bool,
    pub cli_proxy_usage_max_accounts: usize,
    pub cli_proxy_usage_refresh_seconds: u64,
    pub clickable_links: bool,
    pub disabled_sections: Vec<String>,
    pub subagent_disabled_sections: Vec<String>,
    pub update_check_interval_minutes: u64,
    pub usage_fetch_interval_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            advanced_usage_limits_enabled: false,
            cli_proxy_usage_enabled: false,
            cli_proxy_usage_max_accounts: 3,
            cli_proxy_usage_refresh_seconds: 5,
            clickable_links: true,
            disabled_sections: Vec::new(),
            subagent_disabled_sections: Vec::new(),
            update_check_interval_minutes: 0,
            usage_fetch_interval_seconds: 60,
        }
    }
}

/// The proxy poll interval floor: one request per five seconds is what a render every ten
/// seconds needs at most, and a lower value would turn the poll into a busy loop on a fast
/// refresh interval.
pub const PROXY_REFRESH_FLOOR_S: u64 = 5;

impl Config {
    /// At least one row, so a zero in the file cannot hide a line the user turned on.
    pub fn proxy_max_accounts(&self) -> usize {
        self.cli_proxy_usage_max_accounts.max(1)
    }

    pub fn proxy_refresh_seconds(&self) -> u64 {
        self.cli_proxy_usage_refresh_seconds
            .max(PROXY_REFRESH_FLOOR_S)
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

#[derive(Debug, Default, Deserialize)]
pub struct AccountInfo {
    #[serde(default, deserialize_with = "lenient", rename = "accountUuid")]
    pub account_uuid: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ClaudeJson {
    #[serde(default, deserialize_with = "lenient", rename = "oauthAccount")]
    oauth_account: Option<AccountInfo>,
}

/// The login the usage cache is matched against, from Claude Code's
/// ~/.claude.json. Anything missing or malformed yields None, which reads
/// as an unknown login, never as an error the statusline should surface.
pub fn load_account_info(claude_json_path: &Path) -> AccountInfo {
    let Ok(text) = std::fs::read_to_string(claude_json_path) else {
        return AccountInfo::default();
    };
    serde_json::from_str::<ClaudeJson>(&text)
        .ok()
        .and_then(|c| c.oauth_account)
        .unwrap_or_default()
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
    fn payload_rate_limits_parse() {
        let raw = r#"{
            "rate_limits": {
                "five_hour": {"used_percentage": 42, "resets_at": 1753290000},
                "seven_day": {"used_percentage": 63.5, "resets_at": 1753500000}
            }
        }"#;
        let p = parse_payload(raw).unwrap();
        let rl = p.rate_limits.unwrap();
        let five = rl.five_hour.unwrap();
        assert_eq!(five.used_percentage, Some(42.0));
        assert_eq!(five.resets_at, Some(1_753_290_000.0));
        let seven = rl.seven_day.unwrap();
        assert_eq!(seven.used_percentage, Some(63.5));
        assert_eq!(seven.resets_at, Some(1_753_500_000.0));
    }

    #[test]
    fn rate_limits_wrong_typed_fields_become_none_without_killing_neighbors() {
        let raw = r#"{
            "rate_limits": {
                "five_hour": {"used_percentage": "garbage", "resets_at": 1753290000},
                "seven_day": "nope"
            }
        }"#;
        let p = parse_payload(raw).unwrap();
        let rl = p.rate_limits.unwrap();
        let five = rl.five_hour.unwrap();
        assert_eq!(five.used_percentage, None);
        assert_eq!(five.resets_at, Some(1_753_290_000.0));
        assert!(rl.seven_day.is_none());
    }

    #[test]
    fn usage_limits_config_defaults_and_full_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline.json");
        let c = load_config(&path);
        assert!(!c.advanced_usage_limits_enabled);
        assert_eq!(c.usage_fetch_interval_seconds, 60);

        std::fs::write(&path, "{}").unwrap();
        let c = load_config(&path);
        assert!(!c.advanced_usage_limits_enabled);
        assert_eq!(c.usage_fetch_interval_seconds, 60);

        std::fs::write(
            &path,
            r#"{"advanced_usage_limits_enabled": true, "usage_fetch_interval_seconds": 300}"#,
        )
        .unwrap();
        let c = load_config(&path);
        assert!(c.advanced_usage_limits_enabled);
        assert_eq!(c.usage_fetch_interval_seconds, 300);
        // The new keys must not disturb the other defaults.
        assert!(c.clickable_links && c.disabled_sections.is_empty());
    }

    #[test]
    fn partial_usage_limits_config_keeps_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline.json");
        std::fs::write(&path, r#"{"advanced_usage_limits_enabled": true}"#).unwrap();
        let c = load_config(&path);
        assert!(c.advanced_usage_limits_enabled);
        assert_eq!(c.usage_fetch_interval_seconds, 60);
        assert!(c.clickable_links && c.subagent_disabled_sections.is_empty());
    }

    #[test]
    fn load_account_info_reads_oauth_account() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(
            &path,
            r#"{"oauthAccount":{"organizationType":"claude_max","accountUuid":"u-1","emailAddress":"me@example.com"}}"#,
        )
        .unwrap();
        let info = load_account_info(&path);
        assert_eq!(info.account_uuid.as_deref(), Some("u-1"));
    }

    #[test]
    fn load_account_info_missing_file_or_fields_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        let info = load_account_info(&path);
        assert!(info.account_uuid.is_none());

        std::fs::write(&path, r#"{"oauthAccount":{"accountUuid": 42}}"#).unwrap();
        let info = load_account_info(&path);
        assert!(info.account_uuid.is_none());
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

    #[test]
    fn update_check_interval_defaults_to_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline.json");
        assert_eq!(load_config(&path).update_check_interval_minutes, 0);

        std::fs::write(&path, r#"{"update_check_interval_minutes": 1440}"#).unwrap();
        let c = load_config(&path);
        assert_eq!(c.update_check_interval_minutes, 1440);
        // The new key must not disturb the other defaults.
        assert!(c.clickable_links && c.disabled_sections.is_empty());
        assert_eq!(c.usage_fetch_interval_seconds, 60);
    }

    #[test]
    fn session_id_parses_and_a_wrong_type_becomes_none() {
        let p = parse_payload(r#"{"session_id": "11111111-2222-4333-8444-555555555555"}"#).unwrap();
        assert_eq!(
            p.session_id.as_deref(),
            Some("11111111-2222-4333-8444-555555555555")
        );
        let p = parse_payload(r#"{"session_id": 42}"#).unwrap();
        assert!(p.session_id.is_none());
    }

    #[test]
    fn cli_proxy_flag_defaults_off_and_parses() {
        assert!(!Config::default().cli_proxy_usage_enabled);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline.json");
        std::fs::write(&path, r#"{"cli_proxy_usage_enabled": true}"#).unwrap();
        assert!(load_config(&path).cli_proxy_usage_enabled);
    }

    #[test]
    fn proxy_keys_default_and_floor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline.json");
        let c = load_config(&path);
        assert_eq!(c.cli_proxy_usage_max_accounts, 3);
        assert_eq!(c.cli_proxy_usage_refresh_seconds, 5);
        assert_eq!(c.proxy_max_accounts(), 3);
        assert_eq!(c.proxy_refresh_seconds(), 5);

        std::fs::write(
            &path,
            r#"{"cli_proxy_usage_max_accounts": 0, "cli_proxy_usage_refresh_seconds": 1}"#,
        )
        .unwrap();
        let c = load_config(&path);
        assert_eq!(c.proxy_max_accounts(), 1, "below one reads as one");
        assert_eq!(c.proxy_refresh_seconds(), 5, "below five reads as five");

        std::fs::write(
            &path,
            r#"{"cli_proxy_usage_max_accounts": 2, "cli_proxy_usage_refresh_seconds": 30}"#,
        )
        .unwrap();
        let c = load_config(&path);
        assert_eq!(c.proxy_max_accounts(), 2);
        assert_eq!(c.proxy_refresh_seconds(), 30);
    }
}
