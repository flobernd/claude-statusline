use crate::schema::{self, Config, lenient};
use crate::usage;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Version embedded at build time; the only local version source.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// On-disk cache written by the fetch child and read by the render path.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Snapshot {
    #[serde(default)]
    pub fetched_at_ms: u64,
    #[serde(default, deserialize_with = "lenient")]
    pub latest_version: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub release_url: Option<String>,
}

/// Response shape of the releases/latest endpoint, parsed leniently so
/// shape drift reads as "no release", never as an error.
#[derive(Debug, Default, Deserialize)]
struct ReleaseInfo {
    #[serde(default, deserialize_with = "lenient")]
    tag_name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    html_url: Option<String>,
}

pub fn cache_path() -> Option<PathBuf> {
    schema::home_dir().map(|h| h.join(".claude").join("claude-statusline-update.json"))
}

pub fn load_snapshot(path: &Path) -> Option<Snapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Tags carry no prefix by policy, but a stray v on the remote side is
/// tolerated. Anything but a plain numeric triple reads as "no release".
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim();
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Strictly greater only, so dev builds between releases never nag. The
/// returned version is re-rendered from the parsed triple, keeping any
/// stray remote v prefix off the chip.
pub fn available_update(snapshot: &Snapshot, current: &str) -> Option<(String, Option<String>)> {
    let latest = parse_version(snapshot.latest_version.as_deref()?)?;
    if latest <= parse_version(current)? {
        return None;
    }
    Some((
        format!("{}.{}.{}", latest.0, latest.1, latest.2),
        snapshot.release_url.clone(),
    ))
}

/// The stamp lands on failure too, so an offline machine retries once per
/// interval instead of once per render tick; known values carry forward
/// so a transient error cannot clear an already-seen update.
fn next_snapshot(
    release: Option<ReleaseInfo>,
    previous: Option<Snapshot>,
    now_ms: u64,
) -> Snapshot {
    match release {
        Some(ReleaseInfo {
            tag_name: Some(tag),
            html_url,
        }) => Snapshot {
            fetched_at_ms: now_ms,
            latest_version: Some(tag),
            release_url: html_url,
        },
        _ => Snapshot {
            fetched_at_ms: now_ms,
            ..previous.unwrap_or_default()
        },
    }
}

/// releases/latest excludes drafts and prereleases, so notes can be
/// drafted at leisure and the notification fires only on publish.
const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/flobernd/claude-statusline/releases/latest";

/// The config unit is minutes because sub-minute update checks are never
/// sensible; the staleness helper speaks seconds.
fn interval_seconds(config: &Config) -> u64 {
    config.update_check_interval_minutes.saturating_mul(60)
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

pub fn spawn_check_if_stale(config: &Config) {
    // Checked before any filesystem access: interval 0 disables checking.
    if config.update_check_interval_minutes == 0 {
        return;
    }
    let Some(path) = cache_path() else {
        return;
    };
    if !fetch_due(
        interval_seconds(config),
        read_fetched_at_ms(&path),
        crate::clock::now_ms(),
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
    cmd.arg("--fetch-update")
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
/// design: a broken check must never splat into the Claude Code footer.
pub fn run_fetch() -> i32 {
    let _ = try_fetch();
    0
}

fn try_fetch() -> Option<()> {
    let home = schema::home_dir()?;
    let config = schema::load_config(&home.join(".claude").join("claude-statusline.json"));
    let path = cache_path()?;
    // Re-checking staleness doubles as stampede protection when several
    // render ticks spawn children before the first snapshot lands.
    if !fetch_due(
        interval_seconds(&config),
        read_fetched_at_ms(&path),
        crate::clock::now_ms(),
    ) {
        return None;
    }
    let release = fetch_release();
    let snapshot = next_snapshot(release, load_snapshot(&path), crate::clock::now_ms());
    usage::write_json_atomic(&path, &snapshot)
}

/// The only network touchpoint, kept separate so no test can reach it.
fn fetch_release() -> Option<ReleaseInfo> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(5))
        .build();
    let body = agent
        .get(LATEST_RELEASE_URL)
        // GitHub rejects requests without a User-Agent.
        .set(
            "User-Agent",
            concat!("claude-statusline/", env!("CARGO_PKG_VERSION")),
        )
        .set("Accept", "application/vnd.github+json")
        .call()
        .ok()?
        .into_string()
        .ok()?;
    serde_json::from_str(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_triples_parse_with_an_optional_prefix() {
        assert_eq!(parse_version("0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("v1.12.3"), Some((1, 12, 3)));
        assert_eq!(parse_version("V1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version(" 1.2.3 "), Some((1, 2, 3)));
    }

    #[test]
    fn malformed_versions_read_as_no_release() {
        for v in ["", "1", "1.2", "1.2.3.4", "1.2.x", "1.2.3-rc1", "abc", "v"] {
            assert_eq!(parse_version(v), None, "{v} must not parse");
        }
    }

    fn snapshot(latest: Option<&str>, url: Option<&str>) -> Snapshot {
        Snapshot {
            fetched_at_ms: 1,
            latest_version: latest.map(str::to_string),
            release_url: url.map(str::to_string),
        }
    }

    #[test]
    fn update_available_only_when_strictly_newer() {
        let url = "https://github.com/flobernd/claude-statusline/releases/tag/99.0.0";
        let (version, link) =
            available_update(&snapshot(Some("99.0.0"), Some(url)), "0.1.0").unwrap();
        assert_eq!(version, "99.0.0");
        assert_eq!(link.as_deref(), Some(url));

        assert!(available_update(&snapshot(Some("0.1.0"), None), "0.1.0").is_none());
        assert!(available_update(&snapshot(Some("0.0.9"), None), "0.1.0").is_none());
        // A dev build ahead of the latest release stays quiet.
        assert!(available_update(&snapshot(Some("0.2.0"), None), "0.3.0").is_none());
    }

    #[test]
    fn update_display_version_is_normalized() {
        let (version, _) = available_update(&snapshot(Some("v2.0.0"), None), "0.1.0").unwrap();
        assert_eq!(version, "2.0.0");
    }

    #[test]
    fn unusable_snapshot_versions_yield_no_update() {
        assert!(available_update(&snapshot(None, None), "0.1.0").is_none());
        assert!(available_update(&snapshot(Some("soon"), None), "0.1.0").is_none());
    }

    #[test]
    fn numeric_compare_beats_lexicographic_order() {
        let (version, _) = available_update(&snapshot(Some("0.10.0"), None), "0.9.0").unwrap();
        assert_eq!(version, "0.10.0");
    }

    #[test]
    fn successful_fetch_replaces_the_snapshot_values() {
        let release = ReleaseInfo {
            tag_name: Some("0.2.0".to_string()),
            html_url: Some("https://example.com/r".to_string()),
        };
        let previous = snapshot(Some("0.1.5"), Some("https://example.com/old"));
        let next = next_snapshot(Some(release), Some(previous), 42);
        assert_eq!(next.fetched_at_ms, 42);
        assert_eq!(next.latest_version.as_deref(), Some("0.2.0"));
        assert_eq!(next.release_url.as_deref(), Some("https://example.com/r"));
    }

    #[test]
    fn failed_fetch_stamps_the_time_and_carries_values_forward() {
        let previous = snapshot(Some("0.2.0"), Some("https://example.com/r"));
        let next = next_snapshot(None, Some(previous), 42);
        assert_eq!(next.fetched_at_ms, 42);
        assert_eq!(next.latest_version.as_deref(), Some("0.2.0"));
        assert_eq!(next.release_url.as_deref(), Some("https://example.com/r"));

        // A response without a tag counts as a failure, not as "no update".
        let next = next_snapshot(Some(ReleaseInfo::default()), None, 7);
        assert_eq!(next.fetched_at_ms, 7);
        assert!(next.latest_version.is_none() && next.release_url.is_none());
    }

    #[test]
    fn snapshot_round_trips_through_the_cache_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline-update.json");
        let written = snapshot(Some("0.2.0"), Some("https://example.com/r"));
        usage::write_json_atomic(&path, &written).unwrap();
        let loaded = load_snapshot(&path).unwrap();
        assert_eq!(loaded.fetched_at_ms, 1);
        assert_eq!(loaded.latest_version.as_deref(), Some("0.2.0"));
        assert_eq!(loaded.release_url.as_deref(), Some("https://example.com/r"));
        assert_eq!(read_fetched_at_ms(&path), Some(1));
        assert!(load_snapshot(&dir.path().join("missing.json")).is_none());
        assert_eq!(read_fetched_at_ms(&dir.path().join("missing.json")), None);
        std::fs::write(&path, "{broken").unwrap();
        assert_eq!(
            read_fetched_at_ms(&path),
            None,
            "a corrupt cache reads as stale"
        );
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
    fn interval_zero_short_circuits_checking() {
        assert!(!fetch_due(0, None, 1_000_000));
        let config = schema::Config::default();
        assert_eq!(config.update_check_interval_minutes, 0);
        // Must return without touching the cache file or spawning.
        spawn_check_if_stale(&config);
        let enabled = schema::Config {
            update_check_interval_minutes: 1440,
            ..schema::Config::default()
        };
        assert_eq!(interval_seconds(&enabled), 86_400);
    }
}
