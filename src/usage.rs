use crate::schema::{self, Config, lenient};
use chrono::{Datelike, Local, TimeZone};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Response shape of the private api/oauth/usage endpoint. The endpoint is
/// unofficial, so every field parses leniently: shape drift costs a chip,
/// never the whole line.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct EndpointUtilization {
    #[serde(default, deserialize_with = "lenient")]
    pub five_hour: Option<EndpointWindow>,
    #[serde(default, deserialize_with = "lenient")]
    pub seven_day: Option<EndpointWindow>,
    #[serde(default, deserialize_with = "lenient")]
    pub extra_usage: Option<ExtraUsage>,
    #[serde(default, deserialize_with = "lenient")]
    pub limits: Option<Vec<ScopedLimit>>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct EndpointWindow {
    /// 0..100, same scale as the payload percentage (verified live).
    #[serde(default, deserialize_with = "lenient")]
    pub utilization: Option<f64>,
    /// RFC3339 timestamp.
    #[serde(default, deserialize_with = "lenient")]
    pub resets_at: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ExtraUsage {
    #[serde(default, deserialize_with = "lenient")]
    pub is_enabled: Option<bool>,
    /// Cents.
    #[serde(default, deserialize_with = "lenient")]
    pub monthly_limit: Option<f64>,
    /// Cents.
    #[serde(default, deserialize_with = "lenient")]
    pub used_credits: Option<f64>,
    /// 0..100.
    #[serde(default, deserialize_with = "lenient")]
    pub utilization: Option<f64>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ScopedLimit {
    #[serde(default, deserialize_with = "lenient")]
    pub kind: Option<String>,
    /// 0..100.
    #[serde(default, deserialize_with = "lenient")]
    pub percent: Option<f64>,
    /// RFC3339 timestamp.
    #[serde(default, deserialize_with = "lenient")]
    pub resets_at: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub scope: Option<Scope>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Scope {
    #[serde(default, deserialize_with = "lenient")]
    pub model: Option<ScopeModel>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ScopeModel {
    #[serde(default, deserialize_with = "lenient")]
    pub display_name: Option<String>,
}

/// On-disk cache written by the fetch child and read by the render path.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Snapshot {
    #[serde(default)]
    pub fetched_at_ms: u64,
    #[serde(default, deserialize_with = "lenient")]
    pub account_uuid: Option<String>,
    #[serde(default)]
    pub utilization: EndpointUtilization,
    #[serde(default, deserialize_with = "lenient")]
    pub profile: Option<Profile>,
    #[serde(default, deserialize_with = "lenient")]
    pub profile_fetched_at_ms: Option<u64>,
}

/// Account identity from the private api/oauth/profile endpoint, reduced to what the line shows.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Profile {
    #[serde(default, deserialize_with = "lenient")]
    pub email: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub plan: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ProfileResponse {
    #[serde(default, deserialize_with = "lenient")]
    account: Option<ProfileAccount>,
    #[serde(default, deserialize_with = "lenient")]
    organization: Option<ProfileOrganization>,
}

#[derive(Debug, Default, Deserialize)]
struct ProfileAccount {
    #[serde(default, deserialize_with = "lenient")]
    email: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    has_claude_max: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    has_claude_pro: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct ProfileOrganization {
    #[serde(default, deserialize_with = "lenient")]
    organization_type: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    subscription_status: Option<String>,
}

/// The profile changes on a plan switch and little else, so a day between fetches is plenty.
pub(crate) const PROFILE_INTERVAL_S: u64 = 24 * 60 * 60;

pub(crate) fn profile_from_body(body: &str) -> Option<Profile> {
    let response: ProfileResponse = serde_json::from_str(body).ok()?;
    let account = response.account.unwrap_or_default();
    let organization = response.organization.unwrap_or_default();
    Some(Profile {
        email: account
            .email
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty()),
        plan: crate::plan::derive(
            organization.organization_type.as_deref(),
            account.has_claude_max,
            account.has_claude_pro,
            organization.subscription_status.as_deref(),
        ),
    })
}

pub fn cache_path() -> Option<PathBuf> {
    schema::home_dir().map(|h| h.join(".claude").join("claude-statusline-usage.json"))
}

/// Environment overrides that point Claude Code away from the official
/// Claude API. Snapshotted once so the hide/show decision stays pure and
/// testable. The statusline inherits Claude Code's environment, including
/// `env` entries from settings.json, so these are directly visible.
#[derive(Debug, Default)]
pub struct EndpointEnv {
    pub auth_token: Option<String>,
    pub base_url: Option<String>,
    pub use_bedrock: Option<String>,
    pub use_vertex: Option<String>,
}

impl EndpointEnv {
    pub fn from_env() -> Self {
        let var = |key: &str| std::env::var(key).ok();
        Self {
            auth_token: var("ANTHROPIC_AUTH_TOKEN"),
            base_url: var("ANTHROPIC_BASE_URL"),
            use_bedrock: var("CLAUDE_CODE_USE_BEDROCK"),
            use_vertex: var("CLAUDE_CODE_USE_VERTEX"),
        }
    }

    /// Every ambiguity errs toward official so a stray empty variable can
    /// never hide the line. A custom bearer token counts even next to an
    /// official base URL because it means gateway auth either way. The
    /// Bedrock/Vertex base URL variables need no check of their own:
    /// Claude Code ignores them unless the matching mode flag is truthy.
    pub fn is_custom(&self) -> bool {
        is_set(&self.auth_token)
            || self
                .base_url
                .as_deref()
                .map(str::trim)
                .is_some_and(|url| !url.is_empty() && !is_official_url(url))
            || flag_enabled(&self.use_bedrock)
            || flag_enabled(&self.use_vertex)
    }

    /// The base URL of a custom HTTP endpoint, with trailing slashes removed. None for the
    /// official API and for Bedrock or Vertex sessions, where no HTTP base URL applies and a
    /// proxy route cannot exist. A bearer token alone does not name a host, so it plays no part.
    pub fn custom_base_url(&self) -> Option<String> {
        if flag_enabled(&self.use_bedrock) || flag_enabled(&self.use_vertex) {
            return None;
        }
        let url = self.base_url.as_deref()?.trim();
        if url.is_empty() || is_official_url(url) {
            return None;
        }
        Some(url.trim_end_matches('/').to_string())
    }
}

fn is_set(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|v| !v.trim().is_empty())
}

fn flag_enabled(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|v| {
        let v = v.trim().to_ascii_lowercase();
        !v.is_empty() && v != "0" && v != "false"
    })
}

/// api.claude.com counts as official alongside api.anthropic.com because
/// Claude Code treats both as first-party hosts.
fn is_official_url(url: &str) -> bool {
    let normalized = url.trim_end_matches('/').to_ascii_lowercase();
    normalized == "https://api.anthropic.com" || normalized == "https://api.claude.com"
}

/// A snapshot taken under a different account must read as absent so a
/// /login switch never shows another account's numbers.
pub fn load_snapshot(path: &Path, current_uuid: Option<&str>) -> Option<Snapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    let snapshot: Snapshot = serde_json::from_str(&text).ok()?;
    if snapshot.account_uuid.as_deref() != current_uuid {
        return None;
    }
    Some(snapshot)
}

#[derive(Debug)]
pub struct Window {
    /// 0..100.
    pub pct: f64,
    /// Epoch seconds.
    pub resets_at: Option<i64>,
}

#[derive(Debug)]
pub struct Spend {
    pub used_cents: Option<f64>,
    pub limit_cents: Option<f64>,
    /// 0..100.
    pub pct: Option<f64>,
    /// Epoch seconds.
    pub resets_at: Option<i64>,
}

#[derive(Debug, Default)]
pub struct Limits {
    pub session: Option<Window>,
    pub week: Option<Window>,
    pub fable: Option<Window>,
    pub spend: Option<Spend>,
}

/// The payload wins for session/week because it refreshes on every render
/// tick; the cached endpoint snapshot only backfills and supplies the data
/// the payload never carries (fable, spend).
pub fn merge(
    payload: Option<&schema::RateLimits>,
    endpoint: Option<&EndpointUtilization>,
    now_epoch_s: i64,
) -> Limits {
    let session = payload
        .and_then(|p| p.five_hour.as_ref())
        .and_then(payload_window)
        .or_else(|| {
            endpoint
                .and_then(|e| e.five_hour.as_ref())
                .and_then(endpoint_window)
        });
    let week = payload
        .and_then(|p| p.seven_day.as_ref())
        .and_then(payload_window)
        .or_else(|| {
            endpoint
                .and_then(|e| e.seven_day.as_ref())
                .and_then(endpoint_window)
        });
    Limits {
        session,
        week,
        fable: endpoint.and_then(fable_window),
        spend: endpoint
            .and_then(|e| e.extra_usage.as_ref())
            .and_then(|extra| spend_from(extra, now_epoch_s)),
    }
}

fn payload_window(window: &schema::RateWindow) -> Option<Window> {
    Some(Window {
        pct: window.used_percentage?,
        resets_at: window.resets_at.map(|s| s as i64),
    })
}

fn endpoint_window(window: &EndpointWindow) -> Option<Window> {
    Some(Window {
        pct: window.utilization?,
        resets_at: window.resets_at.as_deref().and_then(parse_reset_iso),
    })
}

fn fable_window(endpoint: &EndpointUtilization) -> Option<Window> {
    let limit = endpoint.limits.as_ref()?.iter().find(|l| {
        l.kind.as_deref() == Some("weekly_scoped")
            && l.scope
                .as_ref()
                .and_then(|s| s.model.as_ref())
                .and_then(|m| m.display_name.as_deref())
                .is_some_and(|name| name.contains("Fable"))
    })?;
    Some(Window {
        pct: limit.percent?,
        resets_at: limit.resets_at.as_deref().and_then(parse_reset_iso),
    })
}

fn spend_from(extra: &ExtraUsage, now_epoch_s: i64) -> Option<Spend> {
    spend_from_parts(
        extra.used_credits,
        extra.monthly_limit,
        extra.utilization,
        now_epoch_s,
    )
}

/// Shared by the native endpoint and the CLIProxyAPI proxy route, which report the same shape
/// under different field names.
pub(crate) fn spend_from_parts(
    used_cents: Option<f64>,
    limit_cents: Option<f64>,
    reported_pct: Option<f64>,
    now_epoch_s: i64,
) -> Option<Spend> {
    // Amounts are authoritative when both exist; the reported utilization
    // only fills the gap, so a unit drift there cannot skew real dollars.
    let pct = match (used_cents, limit_cents) {
        (Some(used), Some(limit)) if limit > 0.0 => Some(used / limit * 100.0),
        _ => reported_pct,
    };
    if pct.is_none() && (used_cents.is_none() || limit_cents.is_none()) {
        return None;
    }
    Some(Spend {
        used_cents,
        limit_cents,
        pct,
        resets_at: next_month_start(now_epoch_s),
    })
}

fn parse_reset_iso(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

/// Extra usage renews monthly, so the meter resets on the first of the
/// next month in the user's local time.
fn next_month_start(now_epoch_s: i64) -> Option<i64> {
    let now = chrono::DateTime::from_timestamp(now_epoch_s, 0)?.with_timezone(&Local);
    let (year, month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    // earliest() still lands on day one if a DST jump skips midnight.
    Local
        .with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .earliest()
        .map(|dt| dt.timestamp())
}

/// Pure staleness decision shared by the render-side spawn and the fetch
/// child's stampede re-check; interval 0 always reads as "not due".
pub(crate) fn fetch_due(interval_s: u64, fetched_at_ms: Option<u64>, now_ms: u64) -> bool {
    if interval_s == 0 {
        return false;
    }
    match fetched_at_ms {
        Some(at) => now_ms.saturating_sub(at) >= interval_s.saturating_mul(1_000),
        None => true,
    }
}

/// Staleness needs only fetched_at_ms; a corrupt cache then simply reads
/// as stale instead of dragging the full schema into the render path.
pub(crate) fn read_fetched_at_ms(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("fetched_at_ms")?.as_u64()
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn spawn_fetch_if_stale(config: &Config) {
    // Checked before any filesystem access: interval 0 disables fetching.
    if config.usage_fetch_interval_seconds == 0 {
        return;
    }
    let Some(path) = cache_path() else {
        return;
    };
    if !fetch_due(
        config.usage_fetch_interval_seconds,
        read_fetched_at_ms(&path),
        now_ms(),
    ) {
        return;
    }
    spawn_fetch_child();
}

fn spawn_fetch_child() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.arg("--fetch-usage")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: no console handle
        // and no Ctrl+C propagation, so the child outlives the render tick.
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    // Never waited on: the render path must not block on the network. The
    // parent exits within the tick, so the OS reaps the orphaned child.
    let _ = cmd.spawn();
}

/// Entry point of the detached fetch child. Silent on every failure by
/// design: a broken fetch must never splat into the Claude Code footer,
/// and the last cached values keep rendering.
pub fn run_fetch() -> i32 {
    let _ = try_fetch();
    0
}

const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

fn try_fetch() -> Option<()> {
    let home = schema::home_dir()?;
    let claude_dir = home.join(".claude");
    let config = schema::load_config(&claude_dir.join("claude-statusline.json"));
    let path = cache_path()?;
    // Re-checking staleness doubles as stampede protection when several
    // render ticks spawn children before the first snapshot lands.
    if !fetch_due(
        config.usage_fetch_interval_seconds,
        read_fetched_at_ms(&path),
        now_ms(),
    ) {
        return None;
    }
    let token = read_access_token(&claude_dir.join(".credentials.json"))?;
    let body = fetch_json(USAGE_URL, &token)?;
    let utilization: EndpointUtilization = serde_json::from_str(&body).ok()?;
    let account_uuid = schema::load_account_info(&home.join(".claude.json")).account_uuid;
    // The previous snapshot carries the profile forward. A snapshot of another
    // account reads as absent here, which forces a fresh profile after a
    // /login switch.
    let previous = load_snapshot(&path, account_uuid.as_deref()).unwrap_or_default();
    let (profile, profile_fetched_at_ms) = refresh_profile(
        &token,
        previous.profile,
        previous.profile_fetched_at_ms,
        now_ms(),
    );
    let snapshot = Snapshot {
        fetched_at_ms: now_ms(),
        account_uuid,
        utilization,
        profile,
        profile_fetched_at_ms,
    };
    write_json_atomic(&path, &snapshot)
}

/// A failed profile fetch keeps the previous profile and its stamp, so the
/// next child run retries instead of waiting out a day on nothing.
fn refresh_profile(
    token: &str,
    previous: Option<Profile>,
    previous_at_ms: Option<u64>,
    now: u64,
) -> (Option<Profile>, Option<u64>) {
    if !fetch_due(PROFILE_INTERVAL_S, previous_at_ms, now) {
        return (previous, previous_at_ms);
    }
    match fetch_json(PROFILE_URL, token).and_then(|body| profile_from_body(&body)) {
        Some(profile) => (Some(profile), Some(now)),
        None => (previous, previous_at_ms),
    }
}

/// The token feeds the Authorization header and nothing else; it is never
/// logged, stored, or echoed on any failure path.
fn read_access_token(credentials_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(credentials_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("claudeAiOauth")?
        .get("accessToken")?
        .as_str()
        .map(str::to_string)
}

/// The budget of every detached fetch: long enough for a slow network hop, short enough that
/// a stuck child cannot pile up behind the next tick's spawn.
pub(crate) fn fetch_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(5)
}

/// The only network touchpoint, kept separate so no test can reach it.
fn fetch_json(url: &str, token: &str) -> Option<String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(fetch_timeout())
        .timeout(fetch_timeout())
        .build();
    agent
        .get(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .call()
        .ok()?
        .into_string()
        .ok()
}

/// Temp file plus rename so a render tick can never observe a half-written
/// snapshot. The pid suffix keeps racing children off each other's file.
pub(crate) fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Option<()> {
    let json = serde_json::to_string(value).ok()?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, json).ok()?;
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Local, Timelike};

    const FULL_BODY: &str = r#"{
        "five_hour": {"utilization": 42.0, "resets_at": "2026-07-23T18:00:00Z"},
        "seven_day": {"utilization": 63.5, "resets_at": "2026-07-27T00:00:00Z"},
        "extra_usage": {
            "is_enabled": true,
            "monthly_limit": 100000,
            "used_credits": 100200,
            "utilization": 100.2,
            "currency": "USD",
            "disabled_reason": null
        },
        "limits": [
            {
                "kind": "weekly_scoped",
                "group": "model",
                "percent": 81,
                "resets_at": "2026-07-28T00:00:00Z",
                "scope": {"model": {"id": "fable-5", "display_name": "Fable 5"}}
            },
            {
                "kind": "weekly_scoped",
                "group": "model",
                "percent": 12,
                "resets_at": "2026-07-28T00:00:00Z",
                "scope": {"model": {"id": "sonnet-5", "display_name": "Sonnet 5"}}
            }
        ]
    }"#;

    /// 2026-07-23T18:00:00Z, the five_hour resets_at in FULL_BODY.
    const NOW_S: i64 = 1_784_829_600;
    /// 2026-07-28T00:00:00Z, the weekly resets_at in FULL_BODY.
    const WEEKLY_RESET_S: i64 = 1_785_196_800;

    fn full_endpoint() -> EndpointUtilization {
        serde_json::from_str(FULL_BODY).unwrap()
    }

    fn payload_limits() -> schema::RateLimits {
        schema::RateLimits {
            five_hour: Some(schema::RateWindow {
                used_percentage: Some(42.0),
                resets_at: Some(1_784_836_800.0),
            }),
            seven_day: Some(schema::RateWindow {
                used_percentage: Some(63.5),
                resets_at: Some(1_785_196_800.0),
            }),
        }
    }

    #[test]
    fn full_endpoint_body_parses() {
        let e = full_endpoint();
        let five = e.five_hour.as_ref().unwrap();
        assert_eq!(five.utilization, Some(42.0));
        assert_eq!(five.resets_at.as_deref(), Some("2026-07-23T18:00:00Z"));
        assert_eq!(e.seven_day.as_ref().unwrap().utilization, Some(63.5));
        let extra = e.extra_usage.as_ref().unwrap();
        assert_eq!(extra.is_enabled, Some(true));
        assert_eq!(extra.monthly_limit, Some(100_000.0));
        assert_eq!(extra.used_credits, Some(100_200.0));
        assert_eq!(extra.utilization, Some(100.2));
        let limits = e.limits.as_ref().unwrap();
        assert_eq!(limits.len(), 2);
        assert_eq!(limits[0].kind.as_deref(), Some("weekly_scoped"));
        assert_eq!(limits[0].percent, Some(81.0));
        let name = limits[0]
            .scope
            .as_ref()
            .unwrap()
            .model
            .as_ref()
            .unwrap()
            .display_name
            .as_deref();
        assert_eq!(name, Some("Fable 5"));
    }

    #[test]
    fn wrong_typed_endpoint_fields_become_none_without_killing_neighbors() {
        let raw = r#"{
            "five_hour": {"utilization": "garbage", "resets_at": "2026-07-23T18:00:00Z"},
            "seven_day": "nope",
            "extra_usage": {"monthly_limit": true, "used_credits": 100200},
            "limits": [{"kind": 42, "percent": 81, "scope": {"model": {"display_name": "Fable 5"}}}]
        }"#;
        let e: EndpointUtilization = serde_json::from_str(raw).unwrap();
        let five = e.five_hour.unwrap();
        assert_eq!(five.utilization, None);
        assert_eq!(five.resets_at.as_deref(), Some("2026-07-23T18:00:00Z"));
        assert!(e.seven_day.is_none());
        let extra = e.extra_usage.unwrap();
        assert_eq!(extra.monthly_limit, None);
        assert_eq!(extra.used_credits, Some(100_200.0));
        let limit = &e.limits.unwrap()[0];
        assert!(limit.kind.is_none());
        assert_eq!(limit.percent, Some(81.0));
    }

    #[test]
    fn merge_prefers_payload_windows() {
        let limits = merge(Some(&payload_limits()), Some(&full_endpoint()), NOW_S);
        let session = limits.session.unwrap();
        assert_eq!(session.pct, 42.0);
        assert_eq!(session.resets_at, Some(1_784_836_800));
        let week = limits.week.unwrap();
        assert_eq!(week.pct, 63.5);
        assert_eq!(week.resets_at, Some(1_785_196_800));
    }

    #[test]
    fn merge_backfills_windows_from_endpoint() {
        let limits = merge(None, Some(&full_endpoint()), NOW_S);
        let session = limits.session.unwrap();
        assert!((session.pct - 42.0).abs() < 1e-9);
        assert_eq!(session.resets_at, Some(NOW_S));
        let week = limits.week.unwrap();
        assert!((week.pct - 63.5).abs() < 1e-9);
    }

    #[test]
    fn merge_backfills_when_payload_window_lacks_a_percentage() {
        let payload = schema::RateLimits {
            five_hour: Some(schema::RateWindow {
                used_percentage: None,
                resets_at: Some(1_784_836_800.0),
            }),
            seven_day: None,
        };
        let limits = merge(Some(&payload), Some(&full_endpoint()), NOW_S);
        assert!((limits.session.unwrap().pct - 42.0).abs() < 1e-9);
        assert!((limits.week.unwrap().pct - 63.5).abs() < 1e-9);
    }

    #[test]
    fn merge_fable_picks_only_the_fable_scoped_limit() {
        let fable = merge(None, Some(&full_endpoint()), NOW_S).fable.unwrap();
        assert_eq!(fable.pct, 81.0);
        assert_eq!(fable.resets_at, Some(WEEKLY_RESET_S));

        let raw = r#"{"limits": [{"kind": "weekly_scoped", "percent": 12,
            "scope": {"model": {"display_name": "Sonnet 5"}}}]}"#;
        let e: EndpointUtilization = serde_json::from_str(raw).unwrap();
        assert!(merge(None, Some(&e), NOW_S).fable.is_none());

        let raw = r#"{"limits": [{"kind": "session_scoped", "percent": 12,
            "scope": {"model": {"display_name": "Fable 5"}}}]}"#;
        let e: EndpointUtilization = serde_json::from_str(raw).unwrap();
        assert!(merge(None, Some(&e), NOW_S).fable.is_none());
    }

    #[test]
    fn merge_fable_with_unparseable_reset_keeps_the_percent() {
        let raw = r#"{"limits": [{"kind": "weekly_scoped", "percent": 81,
            "resets_at": "soon", "scope": {"model": {"display_name": "Fable 5"}}}]}"#;
        let e: EndpointUtilization = serde_json::from_str(raw).unwrap();
        let fable = merge(None, Some(&e), NOW_S).fable.unwrap();
        assert_eq!(fable.pct, 81.0);
        assert_eq!(fable.resets_at, None);
    }

    #[test]
    fn merge_spend_uses_amounts_and_computes_percent() {
        let spend = merge(None, Some(&full_endpoint()), NOW_S).spend.unwrap();
        assert_eq!(spend.used_cents, Some(100_200.0));
        assert_eq!(spend.limit_cents, Some(100_000.0));
        assert!((spend.pct.unwrap() - 100.2).abs() < 1e-9);

        let reset = chrono::DateTime::from_timestamp(spend.resets_at.unwrap(), 0)
            .unwrap()
            .with_timezone(&Local);
        let now = chrono::DateTime::from_timestamp(NOW_S, 0)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(
            (reset.year(), reset.month(), reset.day()),
            (now.year(), now.month() + 1, 1)
        );
        assert_eq!((reset.hour(), reset.minute(), reset.second()), (0, 0, 0));
    }

    #[test]
    fn merge_spend_reset_rolls_december_into_january() {
        // 2026-12-15T12:00:00Z is mid-December in every timezone.
        const DEC_NOW_S: i64 = 1_797_336_000;
        let raw = r#"{"extra_usage": {"monthly_limit": 50000, "used_credits": 12500}}"#;
        let e: EndpointUtilization = serde_json::from_str(raw).unwrap();
        let spend = merge(None, Some(&e), DEC_NOW_S).spend.unwrap();
        assert!((spend.pct.unwrap() - 25.0).abs() < 1e-9);
        let reset = chrono::DateTime::from_timestamp(spend.resets_at.unwrap(), 0)
            .unwrap()
            .with_timezone(&Local);
        let now = chrono::DateTime::from_timestamp(DEC_NOW_S, 0)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(
            (reset.year(), reset.month(), reset.day()),
            (now.year() + 1, 1, 1)
        );
    }

    #[test]
    fn merge_spend_falls_back_to_utilization_percent() {
        let raw = r#"{"extra_usage": {"is_enabled": true, "utilization": 37.0}}"#;
        let e: EndpointUtilization = serde_json::from_str(raw).unwrap();
        let spend = merge(None, Some(&e), NOW_S).spend.unwrap();
        assert_eq!(spend.used_cents, None);
        assert_eq!(spend.limit_cents, None);
        assert!((spend.pct.unwrap() - 37.0).abs() < 1e-9);
    }

    #[test]
    fn merge_without_usable_data_is_empty() {
        let limits = merge(None, None, NOW_S);
        assert!(limits.session.is_none() && limits.week.is_none());
        assert!(limits.fable.is_none() && limits.spend.is_none());

        let raw = r#"{"extra_usage": {"is_enabled": false}}"#;
        let e: EndpointUtilization = serde_json::from_str(raw).unwrap();
        assert!(merge(None, Some(&e), NOW_S).spend.is_none());
    }

    #[test]
    fn snapshot_round_trip_and_account_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline-usage.json");
        let snapshot = Snapshot {
            fetched_at_ms: 1_784_829_600_000,
            account_uuid: Some("u-1".to_string()),
            utilization: full_endpoint(),
            profile: None,
            profile_fetched_at_ms: None,
        };
        write_json_atomic(&path, &snapshot).unwrap();

        let loaded = load_snapshot(&path, Some("u-1")).unwrap();
        assert_eq!(loaded.fetched_at_ms, 1_784_829_600_000);
        assert_eq!(loaded.account_uuid.as_deref(), Some("u-1"));
        assert_eq!(
            loaded.utilization.five_hour.unwrap().utilization,
            Some(42.0)
        );

        assert!(load_snapshot(&path, Some("u-2")).is_none());
        assert!(load_snapshot(&path, None).is_none());
        assert!(load_snapshot(&dir.path().join("missing.json"), Some("u-1")).is_none());
    }

    #[test]
    fn snapshot_without_uuid_matches_only_an_unknown_account() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline-usage.json");
        let snapshot = Snapshot {
            fetched_at_ms: 1,
            account_uuid: None,
            utilization: EndpointUtilization::default(),
            profile: None,
            profile_fetched_at_ms: None,
        };
        write_json_atomic(&path, &snapshot).unwrap();
        assert!(load_snapshot(&path, None).is_some());
        assert!(load_snapshot(&path, Some("u-1")).is_none());
    }

    #[test]
    fn atomic_write_leaves_only_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline-usage.json");
        let snapshot = Snapshot {
            fetched_at_ms: 1,
            account_uuid: Some("u-1".to_string()),
            utilization: full_endpoint(),
            profile: None,
            profile_fetched_at_ms: None,
        };
        write_json_atomic(&path, &snapshot).unwrap();
        let updated = Snapshot {
            fetched_at_ms: 2,
            ..snapshot
        };
        write_json_atomic(&path, &updated).unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "no stray temp files may remain");
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Snapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.fetched_at_ms, 2);
        assert_eq!(read_fetched_at_ms(&path), Some(2));
    }

    #[test]
    fn profile_body_maps_to_email_and_plan() {
        let body = r#"{"account":{"email":" biz@example.com ","has_claude_max":true,"has_claude_pro":false},
            "organization":{"organization_type":"claude_max","subscription_status":"active"}}"#;
        let p = profile_from_body(body).unwrap();
        assert_eq!(p.email.as_deref(), Some("biz@example.com"));
        assert_eq!(p.plan.as_deref(), Some("max"));
        let body = r#"{"account":{"email":"a@b.c","has_claude_max":"yes"},
            "organization":{"organization_type":"claude_enterprise"}}"#;
        let p = profile_from_body(body).unwrap();
        assert_eq!(p.plan.as_deref(), Some("enterprise"));
        assert!(profile_from_body("nope").is_none());
    }

    #[test]
    fn snapshot_round_trips_the_profile_and_tolerates_its_absence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let snapshot = Snapshot {
            fetched_at_ms: 5,
            account_uuid: Some("u".to_string()),
            utilization: EndpointUtilization::default(),
            profile: Some(Profile {
                email: Some("a@b.c".to_string()),
                plan: Some("pro".to_string()),
            }),
            profile_fetched_at_ms: Some(4),
        };
        write_json_atomic(&path, &snapshot).unwrap();
        let loaded = load_snapshot(&path, Some("u")).unwrap();
        assert_eq!(loaded.profile.unwrap().email.as_deref(), Some("a@b.c"));
        assert_eq!(loaded.profile_fetched_at_ms, Some(4));
        std::fs::write(
            &path,
            r#"{"fetched_at_ms": 5, "account_uuid": "u", "utilization": {}}"#,
        )
        .unwrap();
        let loaded = load_snapshot(&path, Some("u")).unwrap();
        assert!(loaded.profile.is_none() && loaded.profile_fetched_at_ms.is_none());
    }

    #[test]
    fn profile_interval_is_a_day() {
        assert!(fetch_due(PROFILE_INTERVAL_S, None, 0));
        assert!(!fetch_due(
            PROFILE_INTERVAL_S,
            Some(1_000),
            1_000 + 86_399_999
        ));
        assert!(fetch_due(
            PROFILE_INTERVAL_S,
            Some(1_000),
            1_000 + 86_400_000
        ));
    }

    #[test]
    fn staleness_decision_at_the_interval_boundary() {
        assert!(fetch_due(60, None, 0), "missing cache is always stale");
        assert!(!fetch_due(60, Some(1_000), 60_999));
        assert!(fetch_due(60, Some(1_000), 61_000));
        // A cache stamped in the future (clock skew) reads as fresh.
        assert!(!fetch_due(60, Some(10_000), 5_000));
    }

    #[test]
    fn interval_zero_short_circuits_fetching() {
        assert!(!fetch_due(0, None, 1_000_000));
        let config = Config {
            usage_fetch_interval_seconds: 0,
            ..Config::default()
        };
        // Must return without touching the cache file or spawning.
        spawn_fetch_if_stale(&config);
    }

    fn endpoint_env(
        auth_token: Option<&str>,
        base_url: Option<&str>,
        use_bedrock: Option<&str>,
        use_vertex: Option<&str>,
    ) -> EndpointEnv {
        EndpointEnv {
            auth_token: auth_token.map(str::to_string),
            base_url: base_url.map(str::to_string),
            use_bedrock: use_bedrock.map(str::to_string),
            use_vertex: use_vertex.map(str::to_string),
        }
    }

    #[test]
    fn endpoint_unset_or_blank_variables_read_as_official() {
        assert!(!EndpointEnv::default().is_custom());
        assert!(!endpoint_env(Some(""), Some(""), Some(""), Some("")).is_custom());
        assert!(!endpoint_env(Some("  "), Some(" "), Some("\t"), Some(" ")).is_custom());
    }

    #[test]
    fn endpoint_official_base_urls_read_as_official() {
        for url in [
            "https://api.anthropic.com",
            "https://api.anthropic.com/",
            "https://API.Anthropic.com",
            "https://api.claude.com",
            "https://api.claude.com/",
            "  https://api.anthropic.com  ",
        ] {
            assert!(
                !endpoint_env(None, Some(url), None, None).is_custom(),
                "{url} must read as official"
            );
        }
    }

    #[test]
    fn endpoint_custom_base_urls_read_as_custom() {
        for url in [
            "https://gateway.example.com",
            "https://litellm.internal:4000",
            "http://api.anthropic.com",
            "https://api.anthropic.com/v1",
            "https://anthropic.com",
        ] {
            assert!(
                endpoint_env(None, Some(url), None, None).is_custom(),
                "{url} must read as custom"
            );
        }
    }

    #[test]
    fn endpoint_mode_flags_hide_only_when_truthy() {
        for value in ["1", "true", "TRUE", "yes", "anything"] {
            assert!(endpoint_env(None, None, Some(value), None).is_custom());
            assert!(endpoint_env(None, None, None, Some(value)).is_custom());
        }
        for value in ["0", "false", "FALSE", " false "] {
            assert!(!endpoint_env(None, None, Some(value), None).is_custom());
            assert!(!endpoint_env(None, None, None, Some(value)).is_custom());
        }
    }

    #[test]
    fn endpoint_auth_token_reads_as_custom() {
        assert!(endpoint_env(Some("sk-gateway-token"), None, None, None).is_custom());
        // Even next to an explicitly official base URL: a custom bearer
        // token means gateway auth regardless of the URL.
        assert!(endpoint_env(Some("t"), Some("https://api.anthropic.com"), None, None).is_custom());
    }

    #[test]
    fn custom_base_url_is_the_trimmed_non_official_http_base() {
        assert_eq!(
            endpoint_env(None, Some(" http://127.0.0.1:8317/ "), None, None).custom_base_url(),
            Some("http://127.0.0.1:8317".to_string())
        );
        assert!(
            endpoint_env(None, None, None, None)
                .custom_base_url()
                .is_none()
        );
        assert!(
            endpoint_env(None, Some("https://api.anthropic.com/"), None, None)
                .custom_base_url()
                .is_none()
        );
        assert!(
            endpoint_env(Some("tok"), Some("https://api.claude.com"), None, None)
                .custom_base_url()
                .is_none()
        );
        assert!(
            endpoint_env(None, Some("http://proxy"), Some("1"), None)
                .custom_base_url()
                .is_none()
        );
        assert!(
            endpoint_env(None, Some("http://proxy"), None, Some("true"))
                .custom_base_url()
                .is_none()
        );
        assert_eq!(
            endpoint_env(None, Some("http://proxy"), Some("0"), Some("false")).custom_base_url(),
            Some("http://proxy".to_string())
        );
    }
}
