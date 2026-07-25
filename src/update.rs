use crate::schema::{self, lenient};
use crate::usage;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
        assert!(load_snapshot(&dir.path().join("missing.json")).is_none());
    }
}
