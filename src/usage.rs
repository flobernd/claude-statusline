use crate::backoff;
use crate::schema::{self, Config, lenient};
use chrono::{Datelike, Local, TimeZone};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

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

/// On-disk cache written by the fetch child and read by the render path. The next-at stamps
/// carry the retry schedule of each kind, so a failed fetch waits out its backoff instead of
/// being retried on every render tick; a snapshot without them reads as due for both kinds.
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
    #[serde(default, deserialize_with = "lenient")]
    pub usage_next_at_ms: Option<u64>,
    /// Absent after a success, so the ladder restarts at its first rung.
    #[serde(default, deserialize_with = "lenient")]
    pub usage_backoff_ms: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub profile_next_at_ms: Option<u64>,
    /// Absent after a success, so the ladder restarts at its first rung.
    #[serde(default, deserialize_with = "lenient")]
    pub profile_backoff_ms: Option<u64>,
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

/// The profile changes on a plan switch and little else, so an hour between fetches is
/// plenty, and short enough that a switch shows up within the same session.
pub(crate) const PROFILE_INTERVAL_S: u64 = 60 * 60;

pub(crate) fn profile_from_body(body: &str) -> Option<Profile> {
    let response: ProfileResponse = serde_json::from_str(body).ok()?;
    let account = response.account.unwrap_or_default();
    let organization = response.organization.unwrap_or_default();
    let profile = Profile {
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
    };
    // A 200 whose shape no longer matches (an endpoint change) parses to an
    // empty profile. Reading that as success would overwrite a good cached
    // profile and clear its ladder; None instead makes the profile kind a
    // failure, so the child keeps the previous profile and books the ladder.
    if profile.email.is_none() && profile.plan.is_none() {
        return None;
    }
    Some(profile)
}

/// A 2xx body that parses but carries no window at all (an error envelope, or a shape the
/// endpoint no longer sends) mirrors the profile's emptiness guard: reading it as success
/// would overwrite a good cached utilization and clear its ladder, so it counts as a failure
/// of the usage kind instead, and the child keeps the previous data and books the ladder.
fn utilization_from_body(body: &str) -> Option<EndpointUtilization> {
    let utilization: EndpointUtilization = serde_json::from_str(body).ok()?;
    let has_window = utilization.five_hour.is_some()
        || utilization.seven_day.is_some()
        || utilization.limits.as_ref().is_some_and(|l| !l.is_empty());
    has_window.then_some(utilization)
}

pub fn cache_path() -> Option<PathBuf> {
    schema::home_dir().map(|h| cache_path_in(&h))
}

fn cache_path_in(home: &Path) -> PathBuf {
    home.join(".claude").join("claude-statusline-usage.json")
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

/// A snapshot taken under a different known account must read as absent so a /login switch
/// never shows another account's numbers; the file goes with it, so those numbers do not sit
/// on disk until the next child run either. When the local account is not known, a mismatch
/// still reads as absent but the file stays: a torn read of `~/.claude.json` mid-write by
/// Claude Code must not destroy a good cache over a transient read failure.
pub fn load_snapshot(path: &Path, current_uuid: Option<&str>) -> Option<Snapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    let snapshot: Snapshot = serde_json::from_str(&text).ok()?;
    if snapshot.account_uuid.as_deref() != current_uuid {
        if current_uuid.is_some() {
            let _ = std::fs::remove_file(path);
        }
        return None;
    }
    Some(snapshot)
}

/// Every error is ignored: a missing file is the common case, and a render
/// tick can do nothing about a file it cannot unlink.
pub fn remove_cache() {
    if let Some(path) = cache_path() {
        let _ = std::fs::remove_file(path);
    }
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

/// A kind is due when its next-at stamp is absent, not in the future, or further ahead than
/// `max_ahead_ms`: the largest wait the code books for that kind, so a stamp beyond it could
/// only come from a clock that jumped forward and back, and must not park the poll for days.
/// Shared by the render tick's spawn gate and the fetch child's stampede re-check, so both read
/// one schedule.
pub(crate) fn due(next_at_ms: Option<u64>, now_ms: u64, max_ahead_ms: u64) -> bool {
    next_at_ms.is_none_or(|at| at <= now_ms || at.saturating_sub(now_ms) > max_ahead_ms)
}

/// The spawn gate needs only the two next-at stamps; a corrupt cache then reads as due
/// instead of dragging the full schema into the render path.
pub(crate) fn read_next_at_ms(path: &Path) -> (Option<u64>, Option<u64>) {
    let value: Option<serde_json::Value> = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let stamp = |key: &str| value.as_ref()?.get(key)?.as_u64();
    (stamp("usage_next_at_ms"), stamp("profile_next_at_ms"))
}

pub fn spawn_fetch_if_stale(config: &Config) {
    // Checked before any filesystem access: interval 0 disables fetching.
    if config.usage_fetch_interval_seconds == 0 {
        return;
    }
    let Some(path) = cache_path() else {
        return;
    };
    let (usage_next_at_ms, profile_next_at_ms) = read_next_at_ms(&path);
    let now = now_ms();
    // The ceiling is the larger of the kind's own interval and one hour, so a configured
    // interval under an hour still allows a stamp to sit that far out without reading as due.
    let usage_ceiling_ms = config.usage_fetch_interval_seconds.max(PROFILE_INTERVAL_S) * 1_000;
    let profile_ceiling_ms = PROFILE_INTERVAL_S * 1_000;
    if !due(usage_next_at_ms, now, usage_ceiling_ms)
        && !due(profile_next_at_ms, now, profile_ceiling_ms)
    {
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

/// What one network call produced. A failure keeps the Retry-After the server sent, if any,
/// so the schedule can honor it.
pub(crate) enum Fetched {
    Body(String),
    Failed { retry_after: Option<Duration> },
}

/// (url, token) -> outcome. The child takes its network call as a parameter so the control
/// flow can be tested without a network.
type Fetch<'a> = &'a dyn Fn(&str, &str) -> Fetched;

/// One attempt at a kind, once its body was parsed.
enum Outcome {
    Success,
    Failure { retry_after: Option<Duration> },
}

/// The retry state of one fetch kind, written to the snapshot fields of that kind.
#[derive(Debug, PartialEq, Eq)]
struct Schedule {
    next_at_ms: u64,
    backoff_ms: Option<u64>,
}

/// The outcome table. A success clears the ladder and books the next fetch one interval
/// ahead. A Retry-After books the wait the server asked for and leaves the ladder alone, so
/// the failures before the window still count after it. Any other failure climbs one rung.
fn schedule(
    previous_backoff_ms: Option<u64>,
    outcome: &Outcome,
    interval: Duration,
    now_ms: u64,
) -> Schedule {
    let (wait, backoff_ms) = match outcome {
        Outcome::Success => (interval, None),
        Outcome::Failure {
            retry_after: Some(wait),
        } => (*wait, previous_backoff_ms),
        Outcome::Failure { retry_after: None } => {
            let rung = backoff::next_backoff(previous_backoff_ms.map(Duration::from_millis));
            (rung, Some(rung.as_millis() as u64))
        }
    };
    Schedule {
        next_at_ms: now_ms.saturating_add(wait.as_millis() as u64),
        backoff_ms,
    }
}

/// Runs one due kind. The parser decides success, because a 2xx body that does not parse is
/// as useless as a 5xx and must not stamp the kind fresh.
fn attempt<T>(
    fetch: Fetch<'_>,
    url: &str,
    token: &str,
    parse: impl FnOnce(&str) -> Option<T>,
) -> (Option<T>, Outcome) {
    match fetch(url, token) {
        Fetched::Body(body) => match parse(&body) {
            Some(data) => (Some(data), Outcome::Success),
            None => (None, Outcome::Failure { retry_after: None }),
        },
        Fetched::Failed { retry_after } => (None, Outcome::Failure { retry_after }),
    }
}

fn try_fetch() -> Option<()> {
    let home = schema::home_dir()?;
    try_fetch_with(&home, &fetch_json, now_ms())
}

/// The control flow of the child. Each kind runs on its own schedule, usage first, and the
/// profile runs whether or not the usage fetch succeeded: a rate-limited usage endpoint must
/// not starve the account chip. A failure keeps the previous data and stamp of its kind.
fn try_fetch_with(home: &Path, fetch: Fetch<'_>, now_ms: u64) -> Option<()> {
    let claude_dir = home.join(".claude");
    let config = schema::load_config(&claude_dir.join("claude-statusline.json"));
    if config.usage_fetch_interval_seconds == 0 {
        return None;
    }
    let path = cache_path_in(home);
    let account_uuid = schema::load_account_info(&home.join(".claude.json")).account_uuid;
    // A snapshot of another account reads as absent, which forces a fresh profile after a
    // /login switch.
    let mut snapshot = load_snapshot(&path, account_uuid.as_deref()).unwrap_or_default();
    snapshot.account_uuid = account_uuid;
    // Re-checking the schedule doubles as stampede protection when several render ticks
    // spawn children before the first snapshot lands.
    let usage_ceiling_ms = config.usage_fetch_interval_seconds.max(PROFILE_INTERVAL_S) * 1_000;
    let profile_ceiling_ms = PROFILE_INTERVAL_S * 1_000;
    let usage_due = due(snapshot.usage_next_at_ms, now_ms, usage_ceiling_ms);
    let profile_due = due(snapshot.profile_next_at_ms, now_ms, profile_ceiling_ms);
    if !usage_due && !profile_due {
        return None;
    }
    // A missing or unreadable token is not an early return: every due kind fails without a
    // network call and books its ladder below, so a session with no OAuth login backs off
    // one rung per render tick instead of spawning a fresh child on every one.
    let token = read_access_token(&claude_dir.join(".credentials.json"));
    if usage_due {
        let (utilization, outcome) = match token.as_deref() {
            Some(token) => attempt(fetch, USAGE_URL, token, utilization_from_body),
            None => (None, Outcome::Failure { retry_after: None }),
        };
        if let Some(utilization) = utilization {
            snapshot.utilization = utilization;
            snapshot.fetched_at_ms = now_ms;
        }
        let next = schedule(
            snapshot.usage_backoff_ms,
            &outcome,
            Duration::from_secs(config.usage_fetch_interval_seconds),
            now_ms,
        );
        snapshot.usage_next_at_ms = Some(next.next_at_ms);
        snapshot.usage_backoff_ms = next.backoff_ms;
    }
    if profile_due {
        let (profile, outcome) = match token.as_deref() {
            Some(token) => attempt(fetch, PROFILE_URL, token, profile_from_body),
            None => (None, Outcome::Failure { retry_after: None }),
        };
        if let Some(profile) = profile {
            snapshot.profile = Some(profile);
            snapshot.profile_fetched_at_ms = Some(now_ms);
        }
        let next = schedule(
            snapshot.profile_backoff_ms,
            &outcome,
            Duration::from_secs(PROFILE_INTERVAL_S),
            now_ms,
        );
        snapshot.profile_next_at_ms = Some(next.next_at_ms);
        snapshot.profile_backoff_ms = next.backoff_ms;
    }
    write_json_atomic(&path, &snapshot)
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
pub(crate) fn fetch_timeout() -> Duration {
    Duration::from_secs(5)
}

/// The only network touchpoint, kept separate so no test can reach it. A status error keeps
/// the Retry-After of its response, so a 429 window is waited out rather than hammered.
fn fetch_json(url: &str, token: &str) -> Fetched {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(fetch_timeout())
        .timeout(fetch_timeout())
        .build();
    let response = agent
        .get(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .call();
    match response {
        Ok(response) => match response.into_string() {
            Ok(body) => Fetched::Body(body),
            Err(_) => Fetched::Failed { retry_after: None },
        },
        Err(ureq::Error::Status(_, response)) => Fetched::Failed {
            retry_after: backoff::retry_after(
                response.header("Retry-After"),
                (now_ms() / 1_000) as i64,
            ),
        },
        Err(ureq::Error::Transport(_)) => Fetched::Failed { retry_after: None },
    }
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
            ..Snapshot::default()
        };
        write_json_atomic(&path, &snapshot).unwrap();

        let loaded = load_snapshot(&path, Some("u-1")).unwrap();
        assert_eq!(loaded.fetched_at_ms, 1_784_829_600_000);
        assert_eq!(loaded.account_uuid.as_deref(), Some("u-1"));
        assert_eq!(
            loaded.utilization.five_hour.unwrap().utilization,
            Some(42.0)
        );

        assert!(load_snapshot(&dir.path().join("missing.json"), Some("u-1")).is_none());
        assert!(load_snapshot(&path, Some("u-2")).is_none());
        assert!(
            !path.exists(),
            "another known account's snapshot is removed"
        );
        write_json_atomic(&path, &snapshot).unwrap();
        assert!(load_snapshot(&path, None).is_none());
        assert!(
            path.exists(),
            "an unknown local account must not destroy the cache"
        );
    }

    #[test]
    fn snapshot_without_uuid_matches_only_an_unknown_account() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline-usage.json");
        let snapshot = Snapshot {
            fetched_at_ms: 1,
            ..Snapshot::default()
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
            ..Snapshot::default()
        };
        write_json_atomic(&path, &snapshot).unwrap();
        let updated = Snapshot {
            fetched_at_ms: 2,
            usage_next_at_ms: Some(3),
            ..snapshot
        };
        write_json_atomic(&path, &updated).unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "no stray temp files may remain");
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Snapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.fetched_at_ms, 2);
        assert_eq!(read_fetched_at_ms(&path), Some(2));
        assert_eq!(read_next_at_ms(&path), (Some(3), None));
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
        assert!(profile_from_body("{}").is_none());
        assert!(profile_from_body(r#"{"account":{"uuid":"u"}}"#).is_none());
    }

    #[test]
    fn snapshot_round_trips_the_profile_and_tolerates_its_absence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let snapshot = Snapshot {
            fetched_at_ms: 5,
            account_uuid: Some("u".to_string()),
            profile: Some(Profile {
                email: Some("a@b.c".to_string()),
                plan: Some("pro".to_string()),
            }),
            profile_fetched_at_ms: Some(4),
            ..Snapshot::default()
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
    fn snapshot_round_trips_the_schedule_and_tolerates_its_absence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let snapshot = Snapshot {
            account_uuid: Some("u".to_string()),
            usage_next_at_ms: Some(10),
            usage_backoff_ms: Some(120_000),
            profile_next_at_ms: Some(20),
            profile_backoff_ms: Some(240_000),
            ..Snapshot::default()
        };
        write_json_atomic(&path, &snapshot).unwrap();
        let loaded = load_snapshot(&path, Some("u")).unwrap();
        assert_eq!(
            (loaded.usage_next_at_ms, loaded.usage_backoff_ms),
            (Some(10), Some(120_000))
        );
        assert_eq!(
            (loaded.profile_next_at_ms, loaded.profile_backoff_ms),
            (Some(20), Some(240_000))
        );
        assert_eq!(read_next_at_ms(&path), (Some(10), Some(20)));

        std::fs::write(
            &path,
            r#"{"fetched_at_ms": 5, "account_uuid": "u", "utilization": {}}"#,
        )
        .unwrap();
        let loaded = load_snapshot(&path, Some("u")).unwrap();
        assert!(loaded.usage_next_at_ms.is_none() && loaded.usage_backoff_ms.is_none());
        assert!(loaded.profile_next_at_ms.is_none() && loaded.profile_backoff_ms.is_none());
        assert_eq!(read_next_at_ms(&path), (None, None));

        std::fs::write(&path, "{broken").unwrap();
        assert_eq!(read_next_at_ms(&path), (None, None));
        assert_eq!(
            read_next_at_ms(&dir.path().join("missing.json")),
            (None, None)
        );
    }

    #[test]
    fn due_when_the_stamp_is_absent_or_passed() {
        assert!(due(None, 0, 10_000), "a missing stamp is always due");
        assert!(due(Some(1_000), 1_000, 10_000));
        assert!(due(Some(1_000), 1_001, 10_000));
        // A stamp in the future (a booked retry, or clock skew) is not due.
        assert!(!due(Some(1_001), 1_000, 10_000));
    }

    #[test]
    fn due_treats_a_stamp_beyond_the_ceiling_as_due() {
        // A clock that jumped forward and back must not park the poll for days: a stamp
        // beyond the ceiling reads as due, the same as a missing one.
        assert!(
            !due(Some(11_000), 1_000, 10_000),
            "within the ceiling stays not due"
        );
        assert!(
            due(Some(11_001), 1_000, 10_000),
            "beyond the ceiling reads as due"
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
    fn interval_zero_short_circuits_fetching() {
        assert!(!fetch_due(0, None, 1_000_000));
        let config = Config {
            usage_fetch_interval_seconds: 0,
            ..Config::default()
        };
        // Must return without touching the cache file or spawning.
        spawn_fetch_if_stale(&config);
    }

    const PROFILE_BODY: &str = r#"{"account":{"email":"biz@example.com","has_claude_max":true},
        "organization":{"organization_type":"claude_max"}}"#;

    /// A scratch HOME with the fetch enabled, a token, and a local account, so the child
    /// reaches its network call. The snapshot, when given, is the previous cache.
    fn child_home(interval_s: u64, snapshot: Option<&Snapshot>) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("claude-statusline.json"),
            format!(r#"{{"usage_fetch_interval_seconds": {interval_s}}}"#),
        )
        .unwrap();
        std::fs::write(
            dir.join(".credentials.json"),
            r#"{"claudeAiOauth": {"accessToken": "tok"}}"#,
        )
        .unwrap();
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"oauthAccount": {"accountUuid": "u-1"}}"#,
        )
        .unwrap();
        if let Some(snapshot) = snapshot {
            write_json_atomic(&cache_path_in(home.path()), snapshot).unwrap();
        }
        home
    }

    /// Runs the child against a table of answers per URL and reports the URLs it called and
    /// the snapshot it left behind.
    fn run_child(
        home: &Path,
        now_ms: u64,
        answer: impl Fn(&str) -> Fetched,
    ) -> (Option<Snapshot>, Vec<String>) {
        let calls = std::cell::RefCell::new(Vec::new());
        let fetch = |url: &str, token: &str| {
            assert_eq!(token, "tok");
            calls.borrow_mut().push(url.to_string());
            answer(url)
        };
        try_fetch_with(home, &fetch, now_ms);
        let snapshot = std::fs::read_to_string(cache_path_in(home))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        (snapshot, calls.into_inner())
    }

    fn body(text: &str) -> Fetched {
        Fetched::Body(text.to_string())
    }

    fn failed(retry_after: Option<Duration>) -> Fetched {
        Fetched::Failed { retry_after }
    }

    #[test]
    fn schedule_follows_the_outcome_table() {
        let now = 1_000_000;
        let interval = Duration::from_secs(60);
        assert_eq!(
            schedule(Some(240_000), &Outcome::Success, interval, now),
            Schedule {
                next_at_ms: now + 60_000,
                backoff_ms: None
            }
        );
        let held = Outcome::Failure {
            retry_after: Some(Duration::from_secs(30)),
        };
        assert_eq!(
            schedule(Some(240_000), &held, interval, now),
            Schedule {
                next_at_ms: now + 30_000,
                backoff_ms: Some(240_000)
            }
        );
        let failed = Outcome::Failure { retry_after: None };
        assert_eq!(
            schedule(None, &failed, interval, now),
            Schedule {
                next_at_ms: now + 120_000,
                backoff_ms: Some(120_000)
            }
        );
        assert_eq!(
            schedule(Some(480_000), &failed, interval, now),
            Schedule {
                next_at_ms: now + 600_000,
                backoff_ms: Some(600_000)
            }
        );
    }

    #[test]
    fn child_honors_retry_after_and_keeps_the_previous_usage() {
        let now = 1_000_000;
        let previous = Snapshot {
            fetched_at_ms: 5,
            account_uuid: Some("u-1".to_string()),
            utilization: full_endpoint(),
            ..Snapshot::default()
        };
        let home = child_home(60, Some(&previous));
        let (snapshot, calls) = run_child(home.path(), now, |url| {
            if url == USAGE_URL {
                failed(Some(Duration::from_secs(120)))
            } else {
                body(PROFILE_BODY)
            }
        });
        let snapshot = snapshot.unwrap();
        assert_eq!(calls, [USAGE_URL, PROFILE_URL]);
        assert_eq!(
            snapshot.utilization.five_hour.unwrap().utilization,
            Some(42.0)
        );
        assert_eq!(snapshot.fetched_at_ms, 5);
        assert_eq!(snapshot.usage_next_at_ms, Some(now + 120_000));
        assert!(
            snapshot.usage_backoff_ms.is_none(),
            "a Retry-After leaves the ladder alone"
        );
        assert_eq!(
            snapshot.profile.unwrap().email.as_deref(),
            Some("biz@example.com")
        );
        assert_eq!(snapshot.profile_fetched_at_ms, Some(now));
        assert_eq!(snapshot.profile_next_at_ms, Some(now + 3_600_000));
    }

    #[test]
    fn child_climbs_the_ladder_on_failures_without_retry_after() {
        let now = 1_000_000;
        let home = child_home(60, None);
        let (snapshot, calls) = run_child(home.path(), now, |_| failed(None));
        let snapshot = snapshot.unwrap();
        assert_eq!(calls, [USAGE_URL, PROFILE_URL]);
        assert_eq!(snapshot.account_uuid.as_deref(), Some("u-1"));
        assert_eq!(
            (snapshot.usage_next_at_ms, snapshot.usage_backoff_ms),
            (Some(now + 120_000), Some(120_000))
        );
        assert_eq!(
            (snapshot.profile_next_at_ms, snapshot.profile_backoff_ms),
            (Some(now + 120_000), Some(120_000))
        );

        // A 2xx body that does not parse is a failure of its kind too, one rung up.
        let later = now + 120_000;
        let (snapshot, calls) = run_child(home.path(), later, |_| body("nope"));
        let snapshot = snapshot.unwrap();
        assert_eq!(calls, [USAGE_URL, PROFILE_URL]);
        assert_eq!(
            (snapshot.usage_next_at_ms, snapshot.usage_backoff_ms),
            (Some(later + 240_000), Some(240_000))
        );
        assert_eq!(snapshot.fetched_at_ms, 0);
        assert!(snapshot.profile.is_none());
    }

    #[test]
    fn child_treats_an_empty_usage_body_as_a_failure() {
        let now = 1_000_000;
        let previous = Snapshot {
            account_uuid: Some("u-1".to_string()),
            utilization: full_endpoint(),
            profile_next_at_ms: Some(now + 1),
            ..Snapshot::default()
        };
        let home = child_home(60, Some(&previous));
        let (snapshot, calls) = run_child(home.path(), now, |_| body("{}"));
        let snapshot = snapshot.unwrap();
        assert_eq!(calls, [USAGE_URL]);
        assert_eq!(
            snapshot.utilization.five_hour.unwrap().utilization,
            Some(42.0),
            "an error envelope on a 2xx must not empty the cache"
        );
        assert_eq!(
            (snapshot.usage_next_at_ms, snapshot.usage_backoff_ms),
            (Some(now + 120_000), Some(120_000)),
            "an error envelope on a 2xx must not clear the ladder"
        );
    }

    #[test]
    fn child_success_clears_the_backoff_and_books_the_interval() {
        let now = 1_000_000;
        let previous = Snapshot {
            account_uuid: Some("u-1".to_string()),
            usage_next_at_ms: Some(now),
            usage_backoff_ms: Some(240_000),
            profile_next_at_ms: Some(now - 1),
            profile_backoff_ms: Some(120_000),
            ..Snapshot::default()
        };
        let home = child_home(300, Some(&previous));
        let (snapshot, calls) = run_child(home.path(), now, |url| {
            body(if url == USAGE_URL {
                FULL_BODY
            } else {
                PROFILE_BODY
            })
        });
        let snapshot = snapshot.unwrap();
        assert_eq!(calls, [USAGE_URL, PROFILE_URL]);
        assert_eq!(snapshot.fetched_at_ms, now);
        assert_eq!(
            snapshot.utilization.five_hour.unwrap().utilization,
            Some(42.0)
        );
        assert_eq!(
            (snapshot.usage_next_at_ms, snapshot.usage_backoff_ms),
            (Some(now + 300_000), None)
        );
        assert_eq!(snapshot.profile.unwrap().plan.as_deref(), Some("max"));
        assert_eq!(
            (snapshot.profile_next_at_ms, snapshot.profile_backoff_ms),
            (Some(now + 3_600_000), None)
        );
    }

    #[test]
    fn child_fetches_only_the_due_kind_and_writes_nothing_when_none_is() {
        let now = 1_000_000;
        let previous = Snapshot {
            account_uuid: Some("u-1".to_string()),
            profile: Some(Profile {
                email: Some("kept@example.com".to_string()),
                plan: None,
            }),
            profile_fetched_at_ms: Some(7),
            profile_next_at_ms: Some(now + 1),
            ..Snapshot::default()
        };
        let home = child_home(60, Some(&previous));
        let (snapshot, calls) = run_child(home.path(), now, |_| body(FULL_BODY));
        let snapshot = snapshot.unwrap();
        assert_eq!(calls, [USAGE_URL]);
        assert_eq!(
            snapshot.profile.unwrap().email.as_deref(),
            Some("kept@example.com")
        );
        assert_eq!(
            (snapshot.profile_fetched_at_ms, snapshot.profile_next_at_ms),
            (Some(7), Some(now + 1))
        );

        // Nothing due: no call and no write, which is the stampede guard.
        let before = std::fs::read_to_string(cache_path_in(home.path())).unwrap();
        let (_, calls) = run_child(home.path(), now, |_| body(FULL_BODY));
        assert!(calls.is_empty());
        assert_eq!(
            std::fs::read_to_string(cache_path_in(home.path())).unwrap(),
            before
        );
    }

    #[test]
    fn child_discards_a_snapshot_of_another_account() {
        let now = 1_000_000;
        let previous = Snapshot {
            account_uuid: Some("u-2".to_string()),
            profile: Some(Profile {
                email: Some("other@example.com".to_string()),
                plan: None,
            }),
            usage_next_at_ms: Some(now + 1),
            profile_next_at_ms: Some(now + 1),
            ..Snapshot::default()
        };
        let home = child_home(60, Some(&previous));
        let (snapshot, calls) = run_child(home.path(), now, |_| failed(None));
        let snapshot = snapshot.unwrap();
        assert_eq!(
            calls,
            [USAGE_URL, PROFILE_URL],
            "a mismatch reads as due for both kinds"
        );
        assert_eq!(snapshot.account_uuid.as_deref(), Some("u-1"));
        assert!(
            snapshot.profile.is_none(),
            "another account's profile never carries over"
        );
    }

    #[test]
    fn child_makes_no_request_without_an_interval_or_a_token() {
        let home = child_home(0, None);
        let (snapshot, calls) = run_child(home.path(), 1_000, |_| body(FULL_BODY));
        assert!(calls.is_empty() && snapshot.is_none());

        let home = child_home(60, None);
        std::fs::remove_file(home.path().join(".claude").join(".credentials.json")).unwrap();
        let now = 1_000;
        let (snapshot, calls) = run_child(home.path(), now, |_| body(FULL_BODY));
        let snapshot = snapshot.unwrap();
        assert!(calls.is_empty(), "a missing token makes no request");
        assert_eq!(snapshot.account_uuid.as_deref(), Some("u-1"));
        assert_eq!(
            (snapshot.usage_next_at_ms, snapshot.usage_backoff_ms),
            (Some(now + 120_000), Some(120_000)),
            "a missing token books the ladder for the due usage kind"
        );
        assert_eq!(
            (snapshot.profile_next_at_ms, snapshot.profile_backoff_ms),
            (Some(now + 120_000), Some(120_000)),
            "a missing token books the ladder for the due profile kind"
        );
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
