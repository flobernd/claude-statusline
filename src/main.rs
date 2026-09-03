mod backoff;
mod bar;
mod commands;
mod fit;
mod format;
mod git;
mod plan;
mod proxy;
mod schema;
mod sections;
mod subagent;
mod theme;
mod transcript;
mod update;
mod usage;

use clap::Parser;
use std::io::Read;

#[derive(Parser)]
#[command(
    name = "claude-statusline",
    version,
    about = "Cross-platform statusline for Claude Code CLI"
)]
struct Cli {
    /// Interactive setup wizard
    #[arg(long)]
    setup: bool,
    /// Write the statusLine entry into Claude Code settings
    #[arg(long)]
    install: bool,
    /// Remove the statusLine entry from Claude Code settings
    #[arg(long)]
    uninstall: bool,
    /// Print install state in machine-readable form
    #[arg(long = "print-config")]
    print_config: bool,
    /// Refresh the usage limits cache and exit (spawned by render ticks)
    #[arg(long = "fetch-usage")]
    fetch_usage: bool,
    /// Refresh the update check cache and exit (spawned by render ticks)
    #[arg(long = "fetch-update")]
    fetch_update: bool,
    /// Poll the CLIProxyAPI plugin route for one session and exit (spawned by render ticks)
    #[arg(long = "fetch-proxy", value_name = "SESSION_ID")]
    fetch_proxy: Option<String>,
    /// Render per-task rows for Claude Code's subagentStatusLine hook
    #[arg(long = "subagent-statusline")]
    subagent_statusline: bool,
    /// With --install: also write the subagentStatusLine entry
    #[arg(long = "with-subagent-statusline")]
    with_subagent_statusline: bool,
    /// With --install: also enable the daily update check
    #[arg(long = "with-update-check")]
    with_update_check: bool,
}

// update drops first: the notice is the least session-critical chip and
// reappears whenever there is room.
const LINE1_DROP: &[&str] = &["update", "cache", "cache_age", "effort"];
// cwd, branch, and worktree are intentionally omitted so they never drop:
// the location and active-worktree identity stay visible at any width.
const LINE2_DROP: &[&str] = &[
    "git_worktree",
    "git_stash",
    "git_sync",
    "git_state",
    "git_files",
    "pr",
];
// usage_session is intentionally last so the five-hour window, the limit users hit first, is
// the last chip standing. The plan and the account go before the model: behind a proxy the
// model names why a second row exists, and the windows outrank all three.
const LINE3_DROP: &[&str] = &[
    "usage_plan",
    "usage_account",
    "usage_model",
    "usage_spend",
    "usage_fable",
    "usage_week",
    "usage_session",
];
const SEP: &str = " \u{2502} ";
// The usage line glyph is attached after the fit, so the fit must leave its
// two columns free.
const GLYPH_WIDTH: usize = 2;

fn main() {
    let cli = Cli::parse();
    if cli.print_config {
        std::process::exit(commands::print_config::run());
    }
    if cli.fetch_usage {
        std::process::exit(usage::run_fetch());
    }
    if cli.fetch_update {
        std::process::exit(update::run_fetch());
    }
    if let Some(session_id) = cli.fetch_proxy.as_deref() {
        std::process::exit(proxy::run_fetch(session_id));
    }
    if cli.install {
        if let Err(e) = commands::install::install(cli.with_subagent_statusline) {
            eprintln!("claude-statusline: install failed: {e}");
            std::process::exit(1);
        }
        if cli.with_update_check {
            let path = schema::home_dir()
                .unwrap_or_default()
                .join(".claude")
                .join("claude-statusline.json");
            if let Err(e) = commands::setup::enable_update_check(&path) {
                eprintln!("claude-statusline: enabling the update check failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if cli.uninstall {
        if let Err(e) = commands::install::uninstall() {
            eprintln!("claude-statusline: uninstall failed: {e}");
            std::process::exit(1);
        }
        return;
    }
    if cli.setup {
        if let Err(e) = commands::setup::run() {
            eprintln!("claude-statusline: setup failed: {e}");
            std::process::exit(1);
        }
        return;
    }
    let mut raw_bytes = Vec::new();
    if std::io::stdin().read_to_end(&mut raw_bytes).is_err() {
        return;
    }
    let raw = String::from_utf8_lossy(&raw_bytes);
    if raw.trim().is_empty() {
        return;
    }
    // The statusline must never surface a crash: a panic would splat a
    // backtrace into the Claude Code footer on every render tick. The
    // silent hook keeps the default handler from writing its own
    // multi-line message; the payload text is folded into our single
    // stderr line instead.
    std::panic::set_hook(Box::new(|_| {}));
    let subagent_mode = cli.subagent_statusline;
    match std::panic::catch_unwind(|| {
        if subagent_mode {
            render_subagent(&raw)
        } else {
            render(&raw)
        }
    }) {
        Ok(Some(output)) if !output.is_empty() => println!("{output}"),
        Ok(_) => {}
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            eprintln!("claude-statusline: render error: {msg}");
        }
    }
}

fn terminal_width() -> usize {
    let parse = |key: &str| {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|w| (20..=4000).contains(w))
    };
    parse("CLAUDE_STATUSLINE_WIDTH")
        .or_else(|| parse("COLUMNS"))
        .unwrap_or(100)
}

fn render_subagent(raw: &str) -> Option<String> {
    let config = schema::home_dir()
        .map(|h| schema::load_config(&h.join(".claude").join("claude-statusline.json")))
        .unwrap_or_default();
    let style = theme::Style::from_env(config.clickable_links);
    subagent::render(raw, &config, &style, terminal_width())
}

fn render(raw: &str) -> Option<String> {
    let Some(payload) = schema::parse_payload(raw) else {
        eprintln!("claude-statusline: undecodable stdin payload");
        return Some("?".to_string());
    };

    let config = schema::home_dir()
        .map(|h| schema::load_config(&h.join(".claude").join("claude-statusline.json")))
        .unwrap_or_default();
    let style = theme::Style::from_env(config.clickable_links);
    let width = terminal_width();

    // A missing working directory only loses the git chips; Line 1 must
    // still render, so this never aborts the whole pipeline.
    let git_dir = payload
        .workspace
        .as_ref()
        .and_then(|w| w.current_dir.clone())
        .or_else(|| payload.cwd.clone())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let git_info = match git_dir {
        Some(dir) => git::collect(&dir),
        None => git::GitInfo::default(),
    };

    let cache_age_ms = payload.transcript_path.as_deref().and_then(|p| {
        let ts = transcript::last_assistant_timestamp_ms(p)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis() as i64;
        Some(now - ts)
    });
    let cache_ttl_ms = payload
        .transcript_path
        .as_deref()
        .and_then(transcript::last_cache_ttl_ms);

    let ctx = sections::Ctx {
        payload: &payload,
        git: &git_info,
        cache_age_ms,
        cache_ttl_ms,
        style: &style,
    };
    let sep = style.paint(SEP, theme::COMMENT);
    let sep_width = fit::visible_width(&sep);

    let disabled = &config.disabled_sections;
    let compose =
        |chips: Vec<(&'static str, String)>, drop: &[&str], glyph: bool| -> Option<String> {
            let chips: Vec<(&'static str, String)> = chips
                .into_iter()
                .filter(|(name, _)| !disabled.iter().any(|d| d == name))
                .collect();
            let max_width = if glyph { width - GLYPH_WIDTH } else { width };
            let fitted = fit::fit_line(chips, sep_width, max_width, drop);
            if fitted.is_empty() {
                return None;
            }
            // The glyph marks the line, not a chip, so it is attached only now,
            // to whichever chip the filter and the fit left first.
            let fitted = if glyph {
                sections::with_line_glyph(fitted, &style)
            } else {
                fitted
            };
            Some(
                fitted
                    .into_iter()
                    .map(|(_, r)| r)
                    .collect::<Vec<_>>()
                    .join(&sep),
            )
        };

    // The flag check comes first so the default path never pays the extra
    // ~/.claude.json read on a render tick.
    let usage_rows: Vec<String> = if config.advanced_usage_limits_enabled {
        let account = schema::home_dir()
            .map(|h| schema::load_account_info(&h.join(".claude.json")))
            .unwrap_or_default();
        let endpoint = usage::EndpointEnv::from_env();
        let proxy = proxy_status(&config, &payload, &endpoint);
        if !config.cli_proxy_usage_enabled {
            proxy::remove_session_caches();
        }
        if usage_line_enabled(&config, &payload, &account, &endpoint, proxy.is_some()) {
            let now_epoch_s = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            match &proxy {
                // The local token belongs to whichever account is logged in
                // locally, not to the credential the proxy picked, so neither
                // the claude.ai fetch nor its cache may backfill anything here.
                Some(status) => status
                    .accounts
                    .iter()
                    .take(config.proxy_max_accounts())
                    .filter_map(|account| {
                        let limits = proxy::limits(account, now_epoch_s);
                        compose(
                            sections::line3(
                                &limits,
                                account.plan.as_deref(),
                                account.email.as_deref(),
                                account.last_model(),
                                &style,
                                now_epoch_s,
                            ),
                            LINE3_DROP,
                            true,
                        )
                    })
                    .collect(),
                None if config.cli_proxy_usage_enabled && endpoint.custom_base_url().is_some() => {
                    // The route answered nothing this tick, but the session is still a
                    // proxied one: the local login is not the account behind the proxy, so
                    // the payload windows are all that may render, and the local cache and
                    // fetch child stay untouched.
                    let limits = usage::merge(payload.rate_limits.as_ref(), None, now_epoch_s);
                    compose(
                        sections::line3(&limits, None, None, None, &style, now_epoch_s),
                        LINE3_DROP,
                        true,
                    )
                    .into_iter()
                    .collect()
                }
                None => {
                    // The cache belongs to the fetch: with the fetch off it
                    // would show numbers that never refresh again.
                    if config.usage_fetch_interval_seconds == 0 {
                        usage::remove_cache();
                    } else {
                        usage::spawn_fetch_if_stale(&config);
                    }
                    let snapshot = usage::cache_path()
                        .and_then(|p| usage::load_snapshot(&p, account.account_uuid.as_deref()));
                    let limits = usage::merge(
                        payload.rate_limits.as_ref(),
                        snapshot.as_ref().map(|s| &s.utilization),
                        now_epoch_s,
                    );
                    let (email, plan) = native_account(snapshot.as_ref(), &account);
                    compose(
                        sections::line3(
                            &limits,
                            plan.as_deref(),
                            email.as_deref(),
                            None,
                            &style,
                            now_epoch_s,
                        ),
                        LINE3_DROP,
                        true,
                    )
                    .into_iter()
                    .collect()
                }
            }
        } else {
            Vec::new()
        }
    } else {
        // With the line off, a cache from an earlier opt-in would only go
        // stale on disk.
        usage::remove_cache();
        proxy::remove_session_caches();
        Vec::new()
    };

    let mut line1_chips = sections::line1(&ctx);
    // The interval gates the spawn and the chip; disabled_sections only
    // hides the chip (compose filters it), so the cache stays warm for a
    // later re-enable.
    if config.update_check_interval_minutes > 0 {
        update::spawn_check_if_stale(&config);
        if let Some(chip) = update::cache_path()
            .and_then(|p| update::load_snapshot(&p))
            .and_then(|s| update::available_update(&s, update::CURRENT_VERSION))
            .map(|(version, url)| sections::update_chip(&version, url.as_deref(), &style))
        {
            line1_chips.push(chip);
        }
    }

    let lines: Vec<String> = [
        compose(line1_chips, LINE1_DROP, false),
        compose(sections::line2(&ctx), LINE2_DROP, false),
    ]
    .into_iter()
    .flatten()
    .chain(usage_rows)
    .collect();
    Some(lines.join("\n"))
}

/// The usage line is for native Anthropic subscriptions only. Enterprise
/// seats may receive no payload rate_limits at all, so the account type
/// from ~/.claude.json is the alternate signal that keeps the line alive
/// for them. That fallback is a login artifact though: it stays true when
/// the session talks to a gateway, Bedrock, or Vertex, so a custom
/// endpoint disables it. Payload rate_limits win unconditionally because
/// they only appear when Anthropic subscription headers actually flow, and
/// a status from the CLIProxyAPI plugin route is the same proof for a
/// proxied session: the proxy only answers for a subscription it serves.
fn usage_line_enabled(
    config: &schema::Config,
    payload: &schema::Payload,
    account: &schema::AccountInfo,
    endpoint: &usage::EndpointEnv,
    proxy_status: bool,
) -> bool {
    config.advanced_usage_limits_enabled
        && (payload.rate_limits.is_some()
            || proxy_status
            || (!endpoint.is_custom()
                && account
                    .organization_type
                    .as_deref()
                    .is_some_and(|t| t.starts_with("claude_"))))
}

/// Behind CLIProxyAPI the plugin route is the only source that names the serving accounts. The
/// render tick reads the child's last answer and spawns the child when the poll is due; it
/// never waits on the network itself. Every precondition errs toward None, which leaves the
/// official path untouched.
fn proxy_status(
    config: &schema::Config,
    payload: &schema::Payload,
    endpoint: &usage::EndpointEnv,
) -> Option<proxy::ProxyStatus> {
    if !config.cli_proxy_usage_enabled {
        return None;
    }
    let base = endpoint.custom_base_url()?;
    let session_id = payload.session_id.as_deref()?;
    let path = proxy::session_cache_path(session_id)?;
    let now = proxy::now_ms();
    let cache = proxy::load_session_cache(&path);
    if proxy::fetch_due(cache.as_ref(), &base, config.proxy_refresh_seconds(), now) {
        proxy::spawn_fetch_child(session_id);
    }
    proxy::cached_status(cache.as_ref()?, &base, now)
}

/// The fetched profile names the account the numbers belong to. The local
/// file fills each gap: before the first child run, when the fetch is off,
/// and for a field the endpoint left empty.
fn native_account(
    snapshot: Option<&usage::Snapshot>,
    account: &schema::AccountInfo,
) -> (Option<String>, Option<String>) {
    let profile = snapshot.and_then(|s| s.profile.as_ref());
    let email = profile
        .and_then(|p| p.email.clone())
        .or_else(|| account.email.clone());
    let plan = profile
        .and_then(|p| p.plan.clone())
        .or_else(|| plan::derive(account.organization_type.as_deref(), None, None, None));
    (email, plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_line_requires_the_config_flag() {
        let payload = schema::parse_payload(r#"{"rate_limits": {}}"#).unwrap();
        let account = schema::AccountInfo {
            organization_type: Some("claude_max".to_string()),
            account_uuid: None,
            email: None,
        };
        let official = usage::EndpointEnv::default();
        assert!(!usage_line_enabled(
            &schema::Config::default(),
            &payload,
            &account,
            &official,
            false
        ));
        let enabled = schema::Config {
            advanced_usage_limits_enabled: true,
            ..schema::Config::default()
        };
        assert!(usage_line_enabled(
            &enabled, &payload, &account, &official, false
        ));
    }

    #[test]
    fn usage_line_needs_a_payload_or_account_subscription_signal() {
        let enabled = schema::Config {
            advanced_usage_limits_enabled: true,
            ..schema::Config::default()
        };
        let with_limits = schema::parse_payload(r#"{"rate_limits": {}}"#).unwrap();
        let without = schema::parse_payload("{}").unwrap();
        let native = schema::AccountInfo {
            organization_type: Some("claude_enterprise".to_string()),
            account_uuid: None,
            email: None,
        };
        let external = schema::AccountInfo {
            organization_type: Some("external".to_string()),
            account_uuid: None,
            email: None,
        };
        let unknown = schema::AccountInfo::default();
        let official = usage::EndpointEnv::default();

        assert!(usage_line_enabled(
            &enabled,
            &with_limits,
            &unknown,
            &official,
            false
        ));
        assert!(usage_line_enabled(
            &enabled, &without, &native, &official, false
        ));
        assert!(!usage_line_enabled(
            &enabled, &without, &external, &official, false
        ));
        assert!(!usage_line_enabled(
            &enabled, &without, &unknown, &official, false
        ));
    }

    #[test]
    fn usage_line_custom_endpoint_disables_only_the_account_fallback() {
        let enabled = schema::Config {
            advanced_usage_limits_enabled: true,
            ..schema::Config::default()
        };
        let with_limits = schema::parse_payload(r#"{"rate_limits": {}}"#).unwrap();
        let without = schema::parse_payload("{}").unwrap();
        let native = schema::AccountInfo {
            organization_type: Some("claude_max".to_string()),
            account_uuid: None,
            email: None,
        };
        let custom = usage::EndpointEnv {
            base_url: Some("https://gateway.example.com".to_string()),
            ..usage::EndpointEnv::default()
        };

        // The login artifact alone no longer keeps the line alive.
        assert!(!usage_line_enabled(
            &enabled, &without, &native, &custom, false
        ));
        // Payload rate limits stay ground truth even behind a gateway.
        assert!(usage_line_enabled(
            &enabled,
            &with_limits,
            &native,
            &custom,
            false
        ));
    }

    #[test]
    fn glyph_width_matches_the_painted_glyph() {
        let style = theme::Style {
            colors: true,
            links: false,
        };
        let chips = sections::with_line_glyph(vec![("usage_session", String::new())], &style);
        assert_eq!(fit::visible_width(&chips[0].1), GLYPH_WIDTH);
    }

    #[test]
    fn usage_line_accepts_a_proxy_status_behind_a_custom_endpoint() {
        let enabled = schema::Config {
            advanced_usage_limits_enabled: true,
            ..schema::Config::default()
        };
        let without = schema::parse_payload("{}").unwrap();
        let unknown = schema::AccountInfo::default();
        let custom = usage::EndpointEnv {
            base_url: Some("http://127.0.0.1:8317".to_string()),
            ..usage::EndpointEnv::default()
        };
        assert!(usage_line_enabled(
            &enabled, &without, &unknown, &custom, true
        ));
        assert!(!usage_line_enabled(
            &enabled, &without, &unknown, &custom, false
        ));
    }

    #[test]
    fn native_account_prefers_the_snapshot_profile_per_field() {
        let account = schema::AccountInfo {
            organization_type: Some("claude_max".to_string()),
            account_uuid: None,
            email: Some("local@example.com".to_string()),
        };
        assert_eq!(
            native_account(None, &account),
            (
                Some("local@example.com".to_string()),
                Some("max".to_string())
            )
        );
        let fetched = usage::Snapshot {
            profile: Some(usage::Profile {
                email: Some("fetched@example.com".to_string()),
                plan: Some("team".to_string()),
            }),
            ..usage::Snapshot::default()
        };
        assert_eq!(
            native_account(Some(&fetched), &account),
            (
                Some("fetched@example.com".to_string()),
                Some("team".to_string())
            )
        );
        let partial = usage::Snapshot {
            profile: Some(usage::Profile::default()),
            ..usage::Snapshot::default()
        };
        assert_eq!(
            native_account(Some(&partial), &account),
            (
                Some("local@example.com".to_string()),
                Some("max".to_string())
            )
        );
    }
}
