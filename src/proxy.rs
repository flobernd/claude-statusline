//! The session route of the cpa-claude-statusline CLIProxyAPI plugin: the one source that names
//! the account serving a proxied session. Pure apart from `fetch_status`.

// A later task wires this module into rendering; until then nothing outside its own tests
// calls in, so the whole surface reads as dead code to a build that excludes cfg(test).
#![allow(dead_code)]

use crate::schema::{lenient, lenient_vec};
use crate::usage::{Limits, Window};
use serde::Deserialize;

pub const ROUTE_PATH: &str = "/v0/resource/plugins/cpa-claude-statusline/session";

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
    #[serde(default, deserialize_with = "lenient")]
    pub last_served_at: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ProxyModel {
    #[serde(default, deserialize_with = "lenient")]
    pub id: Option<String>,
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

/// The only network touchpoint, kept separate so no test can reach it. The budget follows the
/// 500 ms the git calls get in the same render tick: a loopback or LAN proxy answers in about
/// a millisecond, and a proxy that is down means Claude Code cannot work either.
pub fn fetch_status(url: &str) -> Option<ProxyStatus> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_millis(100))
        .timeout(std::time::Duration::from_millis(250))
        .build();
    let body = agent.get(url).call().ok()?.into_string().ok()?;
    parse_status(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_756_820_000;

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
