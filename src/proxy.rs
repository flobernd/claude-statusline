//! The session route of the cpa-claude-statusline CLIProxyAPI plugin: the one source that names
//! the account serving a proxied session. Pure apart from `fetch_status`, the session cache
//! files the child writes, and the negative cache file.

use crate::schema::{self, lenient, lenient_vec};
use crate::usage::{Limits, Window};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ROUTE_PATH: &str = "/v0/resource/plugins/cpa-claude-statusline/session";

/// A base URL whose route failed is left alone for this long, whether the failure was a
/// conclusive answer without the plugin or no answer at all: long enough that a gateway
/// without the plugin costs one request per five minutes instead of one per tick, short enough
/// that a restarted proxy shows up within a coffee break. Also the ceiling `retry_pending`
/// enforces, since it is the only wait this module books.
pub const NEGATIVE_CACHE_S: u64 = 5 * 60;

/// Milliseconds since the epoch. A clock set before it reads as 0, which makes every stamp
/// look due rather than parking the poll.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The route body. Every field below `schema` parses leniently: a wrong-typed field costs
/// that field, never the line, and unknown keys are ignored so the plugin can grow the schema.
#[derive(Debug, Default, Deserialize)]
pub struct ProxyStatus {
    #[serde(default, deserialize_with = "lenient")]
    pub schema: Option<u64>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub accounts: Vec<ProxyAccount>,
}

/// One credential that served the session. The plugin orders the accounts newest first and
/// the models within an account newest first, so the first model is the one served last.
#[derive(Debug, Default, Deserialize)]
pub struct ProxyAccount {
    /// Parsed for the route's shape; a row names its account by email.
    #[allow(dead_code)]
    #[serde(default, deserialize_with = "lenient")]
    pub provider: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub email: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub plan: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub windows: Option<ProxyWindows>,
    #[serde(default, deserialize_with = "lenient")]
    pub spend: Option<ProxySpend>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub models: Vec<ProxyModel>,
    /// The plugin already orders the accounts, so the stamp is parsed for the shape rather
    /// than sorted on.
    #[allow(dead_code)]
    #[serde(default, deserialize_with = "lenient")]
    pub last_served_at: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ProxyModel {
    #[serde(default, deserialize_with = "lenient")]
    pub id: Option<String>,
    /// Models arrive newest first, so `last_model` takes the first usable id instead of
    /// comparing stamps.
    #[allow(dead_code)]
    #[serde(default, deserialize_with = "lenient")]
    pub last_served_at: Option<i64>,
}

impl ProxyAccount {
    /// The model id of the account's most recent model; an entry without a usable id is skipped
    /// rather than blanking the chip.
    pub fn last_model(&self) -> Option<&str> {
        self.models.iter().find_map(|m| {
            let id = m.id.as_deref()?.trim();
            (!id.is_empty()).then_some(id)
        })
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ProxyWindows {
    #[serde(default, deserialize_with = "lenient")]
    pub fable: Option<ProxyWindow>,
    #[serde(default, deserialize_with = "lenient")]
    pub five_hour: Option<ProxyWindow>,
    #[serde(default, deserialize_with = "lenient")]
    pub seven_day: Option<ProxyWindow>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ProxyWindow {
    /// 0..100.
    #[serde(default, deserialize_with = "lenient")]
    pub used_percentage: Option<f64>,
    /// Epoch seconds.
    #[serde(default, deserialize_with = "lenient")]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ProxySpend {
    #[serde(default, deserialize_with = "lenient")]
    pub limit_cents: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub used_cents: Option<f64>,
    /// 0..100.
    #[serde(default, deserialize_with = "lenient")]
    pub used_percentage: Option<f64>,
}

/// The route lives on the same origin Claude Code talks to, so the base URL is all the
/// statusline needs to find it. The id is percent-encoded because it is external input.
pub fn status_url(base_url: &str, session_id: &str) -> Option<String> {
    let base = base_url.trim().trim_end_matches('/');
    let id = session_id.trim();
    if base.is_empty() || id.is_empty() {
        return None;
    }
    Some(format!("{base}{ROUTE_PATH}?id={}", percent_encode(id)))
}

fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A body without a schema of 1 or higher, or without a single account, is not the plugin's
/// route and yields nothing, so a stray JSON answer from some other server on that base URL
/// cannot paint the line.
pub fn parse_status(body: &str) -> Option<ProxyStatus> {
    let status: ProxyStatus = serde_json::from_str(body).ok()?;
    if status.schema? < 1 || status.accounts.is_empty() {
        return None;
    }
    Some(status)
}

/// What the route answered. `UnknownSession` is the plugin's own 404 for a session it has not
/// seen a request from yet. `Rejected` is a conclusive answer without the plugin: every other
/// status, or a 2xx body without the schema. `Unreachable` is no answer at all: a transport
/// error, a body the connection dropped before it finished, or the budget. `try_fetch` books
/// the same wait for both.
pub enum RouteResult {
    /// `Status` carries the response body, which the child stores as parsed JSON for the
    /// render tick to re-check on read.
    Status(String),
    UnknownSession,
    Rejected,
    Unreachable,
}

/// The plugin's 404 body, as opposed to the host's own 404 for a route that does not exist.
fn is_unknown_session(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(|e| e == "unknown_session"))
        .unwrap_or(false)
}

/// The classification of a conclusive response, apart from the network so a test can drive it
/// with a status and a body. Never returns `Unreachable`: that variant belongs to the caller,
/// which is the one that knows whether an answer arrived at all.
pub(crate) fn classify(status: u16, body: &str) -> RouteResult {
    match status {
        200..=299 => match parse_status(body) {
            Some(_) => RouteResult::Status(body.to_string()),
            None => RouteResult::Rejected,
        },
        404 if is_unknown_session(body) => RouteResult::UnknownSession,
        _ => RouteResult::Rejected,
    }
}

/// A window whose reset has passed is dropped, as Claude Code drops stale payload windows;
/// spend is built by `usage::spend_from_parts`, the same amounts-first rule the native endpoint
/// uses.
pub fn limits(account: &ProxyAccount, now_epoch_s: i64) -> Limits {
    let window = |w: Option<&ProxyWindow>| -> Option<Window> {
        let w = w?;
        let pct = w.used_percentage?;
        if w.resets_at.is_some_and(|at| at <= now_epoch_s) {
            return None;
        }
        Some(Window {
            pct,
            resets_at: w.resets_at,
        })
    };
    let windows = account.windows.as_ref();
    Limits {
        session: window(windows.and_then(|w| w.five_hour.as_ref())),
        week: window(windows.and_then(|w| w.seven_day.as_ref())),
        fable: window(windows.and_then(|w| w.fable.as_ref())),
        spend: account.spend.as_ref().and_then(|s| {
            crate::usage::spend_from_parts(
                s.used_cents,
                s.limit_cents,
                s.used_percentage,
                now_epoch_s,
            )
        }),
    }
}

/// One remembered failure per base URL. The key is the trimmed base URL as
/// `EndpointEnv::custom_base_url` returns it; a BTreeMap keeps the file stable across
/// rewrites.
pub type NegativeCache = BTreeMap<String, NegativeEntry>;

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct NegativeEntry {
    #[serde(default, deserialize_with = "lenient")]
    pub retry_at_ms: Option<u64>,
}

pub fn negative_cache_path() -> Option<PathBuf> {
    schema::home_dir().map(|h| h.join(".claude").join("claude-statusline-proxy.json"))
}

/// A corrupt file, or an entry that is not an object, reads as empty: the cache only saves
/// requests, so losing it costs one GET and never the line.
pub fn load_negative_cache(path: &Path) -> NegativeCache {
    let Some(serde_json::Value::Object(entries)) = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
    else {
        return NegativeCache::new();
    };
    entries
        .into_iter()
        .filter_map(|(base, entry)| Some((base, serde_json::from_value(entry).ok()?)))
        .collect()
}

/// Deletes the file when nothing is left in it, so a healthy setup leaves no trace. Errors
/// are ignored on purpose: the render path can do nothing about them.
pub fn store_negative_cache(path: &Path, cache: &NegativeCache) {
    if cache.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let _ = crate::usage::write_json_atomic(path, cache);
}

/// A base URL can carry embedded credentials as userinfo (`user:pass@host`); stripped before
/// it becomes a file key so a saved credential never lands in
/// `~/.claude/claude-statusline-proxy.json`.
fn cache_key(base_url: &str) -> String {
    let Some((scheme, rest)) = base_url.split_once("://") else {
        return base_url.to_string();
    };
    // The authority ends at the first path, query, or fragment delimiter; an '@' past that
    // point belongs to the request, not to userinfo, and must not be cut out.
    let (authority, tail) = rest
        .find(['/', '?', '#'])
        .map_or((rest, ""), |i| rest.split_at(i));
    match authority.rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://{host}{tail}"),
        None => base_url.to_string(),
    }
}

/// A stamp further ahead than `NEGATIVE_CACHE_S`, the wait this module books, reads as
/// expired: a clock that jumped forward and back must not park the poll.
pub fn retry_pending(cache: &NegativeCache, base_url: &str, now_ms: u64) -> bool {
    cache
        .get(&cache_key(base_url))
        .and_then(|entry| entry.retry_at_ms)
        .is_some_and(|at| {
            at > now_ms && at.saturating_sub(now_ms) <= NEGATIVE_CACHE_S.saturating_mul(1_000)
        })
}

pub fn note_failure(cache: &mut NegativeCache, base_url: &str, now_ms: u64, wait_s: u64) {
    let retry_at_ms = now_ms.saturating_add(wait_s.saturating_mul(1_000));
    cache.insert(
        cache_key(base_url),
        NegativeEntry {
            retry_at_ms: Some(retry_at_ms),
        },
    );
}

/// Clears a base URL's remembered failure, keyed the same way `note_failure` writes it. True
/// when an entry was actually there, so the caller only rewrites the file when the cache
/// changed.
pub fn forget_failure(cache: &mut NegativeCache, base_url: &str) -> bool {
    cache.remove(&cache_key(base_url)).is_some()
}

/// A stored route answer older than this is not shown: the plugin refreshes on every poll, so
/// a minute of silence means the child stopped landing and the line must not paint numbers that
/// no longer describe the session.
pub const STATUS_MAX_AGE_S: u64 = 60;

/// Session files last written more than this long ago are the leftovers of ended sessions.
const SESSION_FILE_MAX_AGE_S: u64 = 24 * 3600;
/// The sweep is bounded so a directory full of leftovers cannot hold up a fetch.
const SESSION_SWEEP_LIMIT: usize = 64;

/// The route's last answer for one session, written by the fetch child and read by every
/// render tick. `status` is the answer as parsed JSON; it is re-checked on read so a file
/// written by an older build never paints a shape this build does not know.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct SessionCache {
    #[serde(default, deserialize_with = "lenient")]
    pub base_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub attempted_at_ms: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub fetched_at_ms: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub status: Option<serde_json::Value>,
}

pub fn sessions_dir() -> Option<PathBuf> {
    schema::home_dir().map(|h| h.join(".claude").join("claude-statusline-sessions"))
}

/// The session id becomes a file name, so only a conservative character set is allowed and a
/// leading dot is refused: the id is external input from the payload.
pub fn valid_cache_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

pub fn session_cache_path(session_id: &str) -> Option<PathBuf> {
    if !valid_cache_id(session_id) {
        return None;
    }
    sessions_dir().map(|d| d.join(format!("{session_id}.json")))
}

pub fn load_session_cache(path: &Path) -> Option<SessionCache> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn same_base(cache: &SessionCache, base_url: &str) -> bool {
    cache
        .base_url
        .as_deref()
        .is_some_and(|b| cache_key(b) == cache_key(base_url))
}

/// The stored answer, when it is this base URL's, fresh, and still the plugin's shape. A stamp
/// further ahead than the ceiling reads as expired, the rule `fetch_due` applies to its own
/// stamp, so a clock that jumped forward and back cannot freeze numbers on the line.
pub fn cached_status(cache: &SessionCache, base_url: &str, now_ms: u64) -> Option<ProxyStatus> {
    if !same_base(cache, base_url) {
        return None;
    }
    let fetched = cache.fetched_at_ms?;
    if fetched.abs_diff(now_ms) > STATUS_MAX_AGE_S.saturating_mul(1_000) {
        return None;
    }
    parse_status(&cache.status.as_ref()?.to_string())
}

/// Due without a file, for another base URL, or once the attempt stamp is an interval old. A
/// stamp further ahead than the interval reads as expired, so a clock that jumped forward and
/// back cannot park the poll.
pub fn fetch_due(
    cache: Option<&SessionCache>,
    base_url: &str,
    interval_s: u64,
    now_ms: u64,
) -> bool {
    let Some(cache) = cache else {
        return true;
    };
    if !same_base(cache, base_url) {
        return true;
    }
    let Some(attempted) = cache.attempted_at_ms else {
        return true;
    };
    let interval_ms = interval_s.saturating_mul(1_000);
    if attempted > now_ms {
        return attempted - now_ms > interval_ms;
    }
    now_ms - attempted >= interval_ms
}

/// The stamped attempt the child records before it asks. The base URL is keyed the way the
/// negative cache keys it, so a credential carried as userinfo never reaches the file.
fn new_attempt(base_url: &str, now_ms: u64) -> SessionCache {
    SessionCache {
        base_url: Some(cache_key(base_url)),
        attempted_at_ms: Some(now_ms),
        ..Default::default()
    }
}

/// The same stamped attempt with the previous answer riding along, so one failed poll shows the
/// last numbers until `cached_status`'s freshness window runs out instead of blanking the line at
/// once and then holding it blank for the negative cache's wait. An answer stored for another
/// base URL is dropped: it belongs to another proxy.
fn carried_attempt(previous: Option<&SessionCache>, base_url: &str, now_ms: u64) -> SessionCache {
    let kept = previous.filter(|c| same_base(c, base_url));
    SessionCache {
        base_url: Some(cache_key(base_url)),
        attempted_at_ms: Some(now_ms),
        fetched_at_ms: kept.and_then(|c| c.fetched_at_ms),
        status: kept.and_then(|c| c.status.clone()),
    }
}

pub fn spawn_fetch_child(session_id: &str) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.arg("--fetch-proxy")
        .arg(session_id)
        // The clock override serves reproducible preview captures; a live poll has to stamp
        // the real clock or its file reads as ancient or from the future on the next tick.
        .env_remove("CLAUDE_STATUSLINE_NOW_MS")
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

/// Entry point of the detached fetch child. Silent on every failure by design: a broken fetch
/// must never splat into the Claude Code footer, and the next tick simply finds no fresh answer.
pub fn run_fetch(session_id: &str) -> i32 {
    let _ = try_fetch(session_id);
    0
}

fn try_fetch(session_id: &str) -> Option<()> {
    let home = schema::home_dir()?;
    let config = schema::load_config(&home.join(".claude").join("claude-statusline.json"));
    if !config.cli_proxy_usage_enabled {
        return None;
    }
    let endpoint = crate::usage::EndpointEnv::from_env();
    let base = endpoint.custom_base_url()?;
    let path = session_cache_path(session_id)?;
    let now = now_ms();
    let previous = load_session_cache(&path);
    // Re-checking the gate doubles as stampede protection when several render ticks spawn
    // children before the first answer lands.
    if !fetch_due(
        previous.as_ref(),
        &base,
        config.proxy_refresh_seconds(),
        now,
    ) {
        return None;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
        sweep_sessions(dir);
    }
    let mut next = new_attempt(&base, now);
    let negative_path = negative_cache_path()?;
    let mut negative = load_negative_cache(&negative_path);
    if retry_pending(&negative, &base, now) {
        let waiting = carried_attempt(previous.as_ref(), &base, now);
        return crate::usage::write_json_atomic(&path, &waiting);
    }
    let url = status_url(&base, session_id)?;
    match fetch_status(&url) {
        RouteResult::Status(body) => {
            if forget_failure(&mut negative, &base) {
                store_negative_cache(&negative_path, &negative);
            }
            next.fetched_at_ms = Some(now_ms());
            next.status = serde_json::from_str(&body).ok();
            crate::usage::write_json_atomic(&path, &next)?;
            Some(())
        }
        // The plugin answered and knows nothing of the session, so nothing about it is worth
        // keeping: no status is stored and the next tick hides the line.
        RouteResult::UnknownSession => crate::usage::write_json_atomic(&path, &next),
        // Rejected names a gateway that answered without the plugin; Unreachable names one
        // that gave no answer at all. Both book the same wait by decision, so a slow or
        // restarting gateway costs the poll the same as an absent plugin.
        RouteResult::Rejected | RouteResult::Unreachable => {
            note_failure(&mut negative, &base, now, NEGATIVE_CACHE_S);
            store_negative_cache(&negative_path, &negative);
            let failed = carried_attempt(previous.as_ref(), &base, now);
            crate::usage::write_json_atomic(&path, &failed)
        }
    }
}

/// Removes session files that no tick has written for a day; errors are ignored, the sweep
/// only tidies.
pub(crate) fn sweep_sessions(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(SESSION_FILE_MAX_AGE_S));
    let Some(cutoff) = cutoff else {
        return;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if removed >= SESSION_SWEEP_LIMIT {
            return;
        }
        let path = entry.path();
        // `write_json_atomic` writes `<id>.<pid>.tmp` before it renames, so a child killed in
        // between leaves one behind that no rename will ever claim.
        if path.extension().is_none_or(|e| e != "json" && e != "tmp") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|m| m < cutoff);
        if old && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
}

/// The session files belong to the proxy path; with the line or the flag off they would only
/// go stale on disk.
pub fn remove_session_caches() {
    if let Some(dir) = sessions_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The only network touchpoint, kept separate so no test can reach it. It runs in the detached
/// child on the usage fetch's budget; nothing renders on it. ureq's timeouts do not cover DNS
/// resolution, so the request runs on its own thread and the total budget is enforced by
/// `recv_timeout`, which bounds a stuck resolver too.
pub fn fetch_status(url: &str) -> RouteResult {
    let total = crate::usage::fetch_timeout();
    let url = url.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(total)
            .timeout(total)
            .build();
        let result = match agent.get(&url).call() {
            Ok(response) | Err(ureq::Error::Status(_, response)) => {
                let status = response.status();
                // A body the connection dropped before it finished is no answer, the same as
                // a transport error: there is nothing conclusive to classify.
                response
                    .into_string()
                    .map_or(RouteResult::Unreachable, |body| classify(status, &body))
            }
            Err(ureq::Error::Transport(_)) => RouteResult::Unreachable,
        };
        let _ = tx.send(result);
    });
    rx.recv_timeout(total).unwrap_or(RouteResult::Unreachable)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_756_820_000;

    #[test]
    fn cache_ids_are_file_name_safe() {
        for ok in ["11111111-2222-4333-8444-555555555555", "a", "A_b.c-9"] {
            assert!(valid_cache_id(ok), "{ok}");
        }
        for bad in [
            "",
            ".hidden",
            "../x",
            "a/b",
            "a b",
            "x\u{0}y",
            &"a".repeat(129),
        ] {
            assert!(!valid_cache_id(bad), "{bad:?}");
        }
        assert!(valid_cache_id(&"a".repeat(128)));
    }

    fn cache(
        base: &str,
        attempted: Option<u64>,
        fetched: Option<u64>,
        body: Option<&str>,
    ) -> SessionCache {
        SessionCache {
            base_url: Some(base.to_string()),
            attempted_at_ms: attempted,
            fetched_at_ms: fetched,
            status: body.map(|b| serde_json::from_str(b).unwrap()),
        }
    }

    const BASE: &str = "http://127.0.0.1:8317";
    const NOW_MS: u64 = 1_756_820_000_000;

    #[test]
    fn cached_status_needs_the_base_url_a_fresh_stamp_and_a_valid_body() {
        let good = cache(
            BASE,
            Some(NOW_MS),
            Some(NOW_MS - 59_000),
            Some(TWO_ACCOUNTS),
        );
        assert_eq!(
            cached_status(&good, BASE, NOW_MS).map(|s| s.accounts.len()),
            Some(2)
        );
        let stale = cache(
            BASE,
            Some(NOW_MS),
            Some(NOW_MS - 61_000),
            Some(TWO_ACCOUNTS),
        );
        assert!(cached_status(&stale, BASE, NOW_MS).is_none());
        let ahead = cache(
            BASE,
            Some(NOW_MS),
            Some(NOW_MS + 61_000),
            Some(TWO_ACCOUNTS),
        );
        assert!(
            cached_status(&ahead, BASE, NOW_MS).is_none(),
            "a stamp beyond the ceiling reads as expired, as it does for the poll"
        );
        let other = cache(
            "http://other:1",
            Some(NOW_MS),
            Some(NOW_MS),
            Some(TWO_ACCOUNTS),
        );
        assert!(cached_status(&other, BASE, NOW_MS).is_none());
        let userinfo = cache(
            "http://u:p@127.0.0.1:8317",
            Some(NOW_MS),
            Some(NOW_MS),
            Some(TWO_ACCOUNTS),
        );
        assert!(
            cached_status(&userinfo, BASE, NOW_MS).is_some(),
            "keyed like the negative cache"
        );
        let empty = cache(
            BASE,
            Some(NOW_MS),
            Some(NOW_MS),
            Some(r#"{"schema":1,"accounts":[]}"#),
        );
        assert!(cached_status(&empty, BASE, NOW_MS).is_none());
        let none = cache(BASE, Some(NOW_MS), None, None);
        assert!(cached_status(&none, BASE, NOW_MS).is_none());
    }

    #[test]
    fn a_new_attempt_keeps_userinfo_out_of_the_session_file() {
        let raw = "http://u:p@127.0.0.1:8317";
        let attempt = new_attempt(raw, NOW_MS);
        assert_eq!(
            attempt.base_url.as_deref(),
            Some(BASE),
            "the credential must not reach the file: {attempt:?}"
        );
        assert_eq!(attempt.attempted_at_ms, Some(NOW_MS));
        assert!(attempt.fetched_at_ms.is_none() && attempt.status.is_none());
        // The stripped key still names the base URL both gates compare against.
        assert!(!fetch_due(Some(&attempt), raw, 5, NOW_MS));
        assert!(!fetch_due(Some(&attempt), BASE, 5, NOW_MS));
    }

    #[test]
    fn a_carried_attempt_keeps_this_base_urls_answer_only() {
        let previous = cache(BASE, Some(1), Some(2), Some(TWO_ACCOUNTS));
        let carried = carried_attempt(Some(&previous), BASE, NOW_MS);
        assert_eq!(
            (carried.attempted_at_ms, carried.fetched_at_ms),
            (Some(NOW_MS), Some(2))
        );
        assert_eq!(
            cached_status(&carried, BASE, 2).map(|s| s.accounts.len()),
            Some(2),
            "the last answer still paints until it ages out"
        );
        let other = cache("http://other:1", Some(1), Some(2), Some(TWO_ACCOUNTS));
        let switched = carried_attempt(Some(&other), BASE, NOW_MS);
        assert!(
            switched.status.is_none() && switched.fetched_at_ms.is_none(),
            "another proxy's answer is dropped on a switch: {switched:?}"
        );
        let first = carried_attempt(None, "http://u:p@127.0.0.1:8317", NOW_MS);
        assert_eq!(
            first.base_url.as_deref(),
            Some(BASE),
            "the credential must not reach the file: {first:?}"
        );
        assert!(first.status.is_none());
    }

    #[test]
    fn fetch_is_due_without_a_cache_for_another_base_or_after_the_interval() {
        assert!(fetch_due(None, BASE, 5, NOW_MS));
        let other = cache("http://other:1", Some(NOW_MS), None, None);
        assert!(fetch_due(Some(&other), BASE, 5, NOW_MS));
        let fresh = cache(BASE, Some(NOW_MS - 4_000), None, None);
        assert!(!fetch_due(Some(&fresh), BASE, 5, NOW_MS));
        let old = cache(BASE, Some(NOW_MS - 5_000), None, None);
        assert!(fetch_due(Some(&old), BASE, 5, NOW_MS));
        let ahead = cache(BASE, Some(NOW_MS + 6_000), None, None);
        assert!(
            fetch_due(Some(&ahead), BASE, 5, NOW_MS),
            "a stamp beyond the interval reads as expired"
        );
        let slightly_ahead = cache(BASE, Some(NOW_MS + 4_000), None, None);
        assert!(!fetch_due(Some(&slightly_ahead), BASE, 5, NOW_MS));
        let unstamped = cache(BASE, None, None, None);
        assert!(fetch_due(Some(&unstamped), BASE, 5, NOW_MS));
    }

    #[test]
    fn session_cache_round_trips_and_tolerates_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        let written = cache(BASE, Some(1), Some(2), Some(TWO_ACCOUNTS));
        crate::usage::write_json_atomic(&path, &written).unwrap();
        let loaded = load_session_cache(&path).unwrap();
        assert_eq!(loaded.base_url.as_deref(), Some(BASE));
        assert_eq!(
            (loaded.attempted_at_ms, loaded.fetched_at_ms),
            (Some(1), Some(2))
        );
        assert!(loaded.status.is_some());
        std::fs::write(&path, "nope").unwrap();
        assert!(load_session_cache(&path).is_none());
        assert!(load_session_cache(&dir.path().join("missing.json")).is_none());
    }

    #[test]
    fn sweep_removes_only_old_session_files() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.json");
        let young = dir.path().join("young.json");
        let old_tmp = dir.path().join("old.4242.tmp");
        let young_tmp = dir.path().join("young.4242.tmp");
        for path in [&old, &young, &old_tmp, &young_tmp] {
            std::fs::write(path, "{}").unwrap();
        }
        let day_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 3600);
        filetime_set(&old, day_ago);
        filetime_set(&old_tmp, day_ago);
        sweep_sessions(dir.path());
        assert!(!old.exists() && young.exists());
        assert!(
            !old_tmp.exists() && young_tmp.exists(),
            "a temporary a killed child left behind ages out with the session files"
        );
    }

    /// `File::set_modified` is the only mtime write the standard library offers.
    fn filetime_set(path: &Path, to: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(to)
            .unwrap();
    }

    #[test]
    fn status_url_strips_slashes_and_encodes_the_id() {
        assert_eq!(
            status_url("http://127.0.0.1:8317/", "abc-123").as_deref(),
            Some(
                "http://127.0.0.1:8317/v0/resource/plugins/cpa-claude-statusline/session?id=abc-123"
            )
        );
        assert_eq!(
            status_url("http://proxy/base", "a b&c").as_deref(),
            Some(
                "http://proxy/base/v0/resource/plugins/cpa-claude-statusline/session?id=a%20b%26c"
            )
        );
        assert!(status_url("", "abc").is_none());
        assert!(status_url("http://proxy", " ").is_none());
    }

    const TWO_ACCOUNTS: &str = r#"{"schema":1,"accounts":[
        {"provider":"claude","email":"git@example.com","plan":"Max 5x",
         "windows":{"five_hour":{"used_percentage":6,"resets_at":1756835400},
                    "seven_day":{"used_percentage":41,"resets_at":1757271600},
                    "fable":{"used_percentage":12,"resets_at":1757271600}},
         "spend":{"used_cents":1234,"limit_cents":5000,"used_percentage":24.7},
         "models":[{"id":"claude-fable-5-1[1m]","last_served_at":1756820000},
                   {"id":"claude-opus-5","last_served_at":1756819000}],
         "last_served_at":1756820000},
        {"provider":"claude","email":"mail@example.com","plan":"Pro 5x",
         "windows":{"five_hour":{"used_percentage":31,"resets_at":1756835400}},
         "models":[{"id":"claude-sonnet-5","last_served_at":1756819800}],
         "last_served_at":1756819800}],
        "updated_at":1756820000}"#;

    #[test]
    fn parse_status_keeps_the_account_order_and_needs_one_account() {
        let status = parse_status(TWO_ACCOUNTS).unwrap();
        assert_eq!(status.accounts.len(), 2);
        assert_eq!(status.accounts[0].email.as_deref(), Some("git@example.com"));
        assert_eq!(
            status.accounts[0].last_model(),
            Some("claude-fable-5-1[1m]")
        );
        assert_eq!(status.accounts[1].last_model(), Some("claude-sonnet-5"));
        assert!(parse_status(r#"{"schema":1,"accounts":[]}"#).is_none());
        assert!(parse_status(r#"{"schema":1}"#).is_none());
        assert!(parse_status(r#"{"accounts":[{"email":"x"}]}"#).is_none());
        assert!(parse_status(r#"{"schema":0,"accounts":[{}]}"#).is_none());
        assert!(parse_status("nope").is_none());
    }

    #[test]
    fn parse_status_is_lenient_inside_an_account() {
        let body = r#"{"schema":1,"accounts":[{"email":"biz@example.com","plan":"max","extra":1,
            "windows":{"five_hour":{"used_percentage":"six","resets_at":1},
                       "seven_day":{"used_percentage":41,"resets_at":1757271600}},
            "spend":{"enabled":true,"used_cents":1234,"limit_cents":5000},
            "models":[{"id":7},{"id":"  "},{"id":"claude-sonnet-5"}],"unknown":{}}]}"#;
        let status = parse_status(body).unwrap();
        let account = &status.accounts[0];
        assert_eq!(account.email.as_deref(), Some("biz@example.com"));
        let windows = account.windows.as_ref().unwrap();
        assert!(
            windows
                .five_hour
                .as_ref()
                .unwrap()
                .used_percentage
                .is_none()
        );
        assert_eq!(
            windows.seven_day.as_ref().unwrap().used_percentage,
            Some(41.0)
        );
        // A model without a usable id, absent or blank, is skipped, so the last model is the
        // first usable one.
        assert_eq!(account.last_model(), Some("claude-sonnet-5"));
    }

    #[test]
    fn classify_separates_the_plugin_404_from_every_rejection() {
        let good = r#"{"schema":1,"accounts":[{"email":"biz@example.com"}]}"#;
        assert!(matches!(classify(200, good), RouteResult::Status(body) if body == good));
        assert!(matches!(
            classify(404, r#"{"error":"unknown_session"}"#),
            RouteResult::UnknownSession
        ));
        assert!(matches!(
            classify(404, r#" {"error": "unknown_session", "id": "x"} "#),
            RouteResult::UnknownSession
        ));
        for (status, body) in [
            (200, r#"{"account":{}}"#),
            (200, r#"{"schema":1,"accounts":[]}"#),
            (200, "nope"),
            (404, "404 page not found"),
            (404, r#"{"error":"not_found"}"#),
            (500, r#"{"error":"unknown_session"}"#),
            (503, ""),
        ] {
            assert!(
                matches!(classify(status, body), RouteResult::Rejected),
                "{status} {body:?} must be rejected"
            );
        }
    }

    #[test]
    fn negative_cache_round_trips_and_reads_corruption_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline-proxy.json");
        assert!(load_negative_cache(&path).is_empty());

        let mut cache = NegativeCache::new();
        note_failure(&mut cache, "http://127.0.0.1:8317", 1_000, NEGATIVE_CACHE_S);
        store_negative_cache(&path, &cache);
        let loaded = load_negative_cache(&path);
        assert_eq!(loaded["http://127.0.0.1:8317"].retry_at_ms, Some(301_000));
        assert!(retry_pending(&loaded, "http://127.0.0.1:8317", 300_999));
        assert!(!retry_pending(&loaded, "http://127.0.0.1:8317", 301_000));
        assert!(!retry_pending(&loaded, "http://other", 0));

        std::fs::write(
            &path,
            r#"{"http://a": {"retry_at_ms": "soon"}, "http://b": 5, "http://c": {"retry_at_ms": 9}}"#,
        )
        .unwrap();
        let loaded = load_negative_cache(&path);
        assert!(loaded["http://a"].retry_at_ms.is_none());
        assert!(!loaded.contains_key("http://b"));
        assert_eq!(loaded["http://c"].retry_at_ms, Some(9));
        for corrupt in ["{broken", "[1]", "null"] {
            std::fs::write(&path, corrupt).unwrap();
            assert!(load_negative_cache(&path).is_empty(), "{corrupt:?}");
        }
    }

    #[test]
    fn storing_an_empty_negative_cache_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline-proxy.json");
        let mut cache = NegativeCache::new();
        note_failure(&mut cache, "http://proxy", 0, NEGATIVE_CACHE_S);
        store_negative_cache(&path, &cache);
        assert!(path.exists());
        assert!(cache.remove("http://proxy").is_some());
        store_negative_cache(&path, &cache);
        assert!(!path.exists());
        // Storing an empty cache without a file is not an error either.
        store_negative_cache(&path, &cache);
    }

    #[test]
    fn negative_cache_key_strips_userinfo_from_the_base_url() {
        let raw = "http://user:pass@127.0.0.1:8317";
        let mut cache = NegativeCache::new();
        note_failure(&mut cache, raw, 0, NEGATIVE_CACHE_S);
        assert!(
            cache.contains_key("http://127.0.0.1:8317"),
            "the key must not carry the credential: {cache:?}"
        );
        assert!(retry_pending(&cache, raw, 0), "the check must strip it too");
        assert!(
            forget_failure(&mut cache, raw),
            "the removal must strip it too"
        );
        assert!(cache.is_empty());

        // An '@' past the authority belongs to the request, not to userinfo: cutting the
        // authority at the first '/', '?', or '#' keeps it out of the strip.
        let query = "http://host?t=a@b";
        note_failure(&mut cache, query, 0, NEGATIVE_CACHE_S);
        assert!(
            cache.contains_key(query),
            "a query-string '@' must not be read as userinfo: {cache:?}"
        );

        // A plain URL with no userinfo at all is used as its own key, unchanged.
        let plain = "http://proxy.example.com";
        note_failure(&mut cache, plain, 0, NEGATIVE_CACHE_S);
        assert!(cache.contains_key(plain), "{cache:?}");
    }

    #[test]
    fn note_failure_books_the_wait_it_is_given() {
        // note_failure stays generic over the wait even though every caller in this codebase
        // now passes NEGATIVE_CACHE_S for both failure kinds.
        let mut cache = NegativeCache::new();
        note_failure(&mut cache, "http://proxy", 0, 45);
        assert_eq!(cache["http://proxy"].retry_at_ms, Some(45_000));
    }

    #[test]
    fn retry_pending_treats_a_stamp_beyond_the_ceiling_as_expired() {
        let mut cache = NegativeCache::new();
        // A clock that jumped forward and back must not park the poll for days: only a stamp
        // within NEGATIVE_CACHE_S of now, the wait this module books, is honored.
        cache.insert(
            "http://proxy".to_string(),
            NegativeEntry {
                retry_at_ms: Some(NEGATIVE_CACHE_S * 1_000 + 1),
            },
        );
        assert!(!retry_pending(&cache, "http://proxy", 0));
    }

    #[test]
    fn limits_drop_expired_windows_and_build_spend_from_amounts() {
        let body = format!(
            r#"{{"schema":1,"accounts":[{{"windows":{{"five_hour":{{"used_percentage":6,"resets_at":{future}}},
                "seven_day":{{"used_percentage":41,"resets_at":{past}}},"fable":{{"used_percentage":12}}}},
                "spend":{{"used_cents":1234,"limit_cents":5000,"used_percentage":99}}}}]}}"#,
            future = NOW + 100,
            past = NOW - 1
        );
        let status = parse_status(&body).unwrap();
        let limits = limits(&status.accounts[0], NOW);
        assert_eq!(limits.session.as_ref().map(|w| w.pct), Some(6.0));
        assert!(limits.week.is_none(), "an expired window must drop");
        assert_eq!(
            limits.fable.as_ref().map(|w| (w.pct, w.resets_at)),
            Some((12.0, None))
        );
        let spend = limits.spend.unwrap();
        assert!(
            (spend.pct.unwrap() - 24.68).abs() < 0.01,
            "amounts win over the reported percent"
        );
        assert!(spend.resets_at.is_some());
    }

    #[test]
    fn limits_spend_falls_back_to_the_reported_percent_or_nothing() {
        let with_percent =
            parse_status(r#"{"schema":1,"accounts":[{"spend":{"used_percentage":40}}]}"#).unwrap();
        assert_eq!(
            limits(&with_percent.accounts[0], NOW)
                .spend
                .and_then(|s| s.pct),
            Some(40.0)
        );
        let without = parse_status(r#"{"schema":1,"accounts":[{"spend":{}}]}"#).unwrap();
        assert!(limits(&without.accounts[0], NOW).spend.is_none());
    }
}
