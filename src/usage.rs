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
    /// 0..1 fraction, unlike the payload's 0..100 percentage.
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
    /// 0..1 fraction.
    #[serde(default, deserialize_with = "lenient")]
    pub utilization: Option<f64>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ScopedLimit {
    #[serde(default, deserialize_with = "lenient")]
    pub kind: Option<String>,
    /// Already 0..100, unlike the window utilization fractions.
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
}

pub fn cache_path() -> Option<PathBuf> {
    schema::home_dir().map(|h| h.join(".claude").join("claude-statusline-usage.json"))
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
        pct: window.utilization? * 100.0,
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
    // Amounts are authoritative when both exist; the reported utilization
    // only fills the gap, so a unit drift there cannot skew real dollars.
    let pct = match (extra.used_credits, extra.monthly_limit) {
        (Some(used), Some(limit)) if limit > 0.0 => Some(used / limit * 100.0),
        _ => extra.utilization.map(|fraction| fraction * 100.0),
    };
    if pct.is_none() && (extra.used_credits.is_none() || extra.monthly_limit.is_none()) {
        return None;
    }
    Some(Spend {
        used_cents: extra.used_credits,
        limit_cents: extra.monthly_limit,
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
fn fetch_due(interval_s: u64, fetched_at_ms: Option<u64>, now_ms: u64) -> bool {
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
fn read_fetched_at_ms(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("fetched_at_ms")?.as_u64()
}

fn now_ms() -> u64 {
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
    let body = fetch_body(&token)?;
    let utilization: EndpointUtilization = serde_json::from_str(&body).ok()?;
    let account_uuid = schema::load_account_info(&home.join(".claude.json")).account_uuid;
    let snapshot = Snapshot {
        fetched_at_ms: now_ms(),
        account_uuid,
        utilization,
    };
    write_snapshot_atomic(&path, &snapshot)
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

/// The only network touchpoint, kept separate so no test can reach it.
fn fetch_body(token: &str) -> Option<String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(5))
        .build();
    agent
        .get("https://api.anthropic.com/api/oauth/usage")
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .call()
        .ok()?
        .into_string()
        .ok()
}

/// Temp file plus rename so a render tick can never observe a half-written
/// snapshot. The pid suffix keeps racing children off each other's file.
fn write_snapshot_atomic(path: &Path, snapshot: &Snapshot) -> Option<()> {
    let json = serde_json::to_string(snapshot).ok()?;
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
        "five_hour": {"utilization": 0.42, "resets_at": "2026-07-23T18:00:00Z"},
        "seven_day": {"utilization": 0.635, "resets_at": "2026-07-27T00:00:00Z"},
        "extra_usage": {
            "is_enabled": true,
            "monthly_limit": 100000,
            "used_credits": 100200,
            "utilization": 1.002,
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
        assert_eq!(five.utilization, Some(0.42));
        assert_eq!(five.resets_at.as_deref(), Some("2026-07-23T18:00:00Z"));
        assert_eq!(e.seven_day.as_ref().unwrap().utilization, Some(0.635));
        let extra = e.extra_usage.as_ref().unwrap();
        assert_eq!(extra.is_enabled, Some(true));
        assert_eq!(extra.monthly_limit, Some(100_000.0));
        assert_eq!(extra.used_credits, Some(100_200.0));
        assert_eq!(extra.utilization, Some(1.002));
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
        let raw = r#"{"extra_usage": {"is_enabled": true, "utilization": 0.37}}"#;
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
        };
        write_snapshot_atomic(&path, &snapshot).unwrap();

        let loaded = load_snapshot(&path, Some("u-1")).unwrap();
        assert_eq!(loaded.fetched_at_ms, 1_784_829_600_000);
        assert_eq!(loaded.account_uuid.as_deref(), Some("u-1"));
        assert_eq!(
            loaded.utilization.five_hour.unwrap().utilization,
            Some(0.42)
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
        };
        write_snapshot_atomic(&path, &snapshot).unwrap();
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
        };
        write_snapshot_atomic(&path, &snapshot).unwrap();
        let updated = Snapshot {
            fetched_at_ms: 2,
            ..snapshot
        };
        write_snapshot_atomic(&path, &updated).unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "no stray temp files may remain");
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Snapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.fetched_at_ms, 2);
        assert_eq!(read_fetched_at_ms(&path), Some(2));
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
}
