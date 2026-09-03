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
    // The usage line glyph is attached after the fit, so the fit must leave its
    // columns free.
    let glyph_width = sections::line_glyph_width();

    let disabled = &config.disabled_sections;
    let compose =
        |chips: Vec<(&'static str, String)>, drop: &[&str], glyph: bool| -> Option<String> {
            let chips: Vec<(&'static str, String)> = chips
                .into_iter()
                .filter(|(name, _)| !disabled.iter().any(|d| d == name))
                .collect();
            let max_width = if glyph { width - glyph_width } else { width };
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
        let endpoint = usage::EndpointEnv::from_env();
        let proxy = proxy_status(&config, &payload, &endpoint);
        let snapshot = if config.usage_fetch_interval_seconds == 0 {
            // The cache belongs to the fetch on every endpoint: with the fetch off it would
            // show numbers that never refresh again, and a custom endpoint reaches this line
            // whenever the base URL changed after the cache was written.
            usage::remove_cache();
            None
        } else if endpoint.is_custom() {
            // The local login is the account behind the numbers on the official endpoint
            // only. A custom endpoint or a bearer token serves some other account, so such a
            // session neither reads the local cache nor spawns the child, whatever the proxy
            // route said.
            None
        } else {
            spawn_then_load_snapshot(&config)
        };
        if !config.cli_proxy_usage_enabled {
            proxy::remove_session_caches();
        }
        if usage_line_enabled(
            &config,
            &payload,
            &endpoint,
            proxy.is_some(),
            snapshot.as_ref(),
        ) {
            let now_epoch_s = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            match &proxy {
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
                None => {
                    let limits = usage::merge(
                        payload.rate_limits.as_ref(),
                        snapshot.as_ref().map(|s| &s.utilization),
                        now_epoch_s,
                    );
                    let (email, plan) = native_account(snapshot.as_ref());
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

/// The usage line is for native Anthropic subscriptions only. Payload
/// rate_limits prove one unconditionally, because they only appear when
/// Anthropic subscription headers actually flow, and a status from the
/// CLIProxyAPI plugin route is the same proof for a proxied session: the
/// proxy only answers for a subscription it serves. An enterprise seat may
/// receive no payload rate_limits at all, so on the official endpoint a
/// snapshot with fetched content keeps the line alive for it: the child
/// fetched it with the local login, which is the account behind the numbers
/// there and nowhere else.
fn usage_line_enabled(
    config: &schema::Config,
    payload: &schema::Payload,
    endpoint: &usage::EndpointEnv,
    proxy_status: bool,
    snapshot: Option<&usage::Snapshot>,
) -> bool {
    config.advanced_usage_limits_enabled
        && (payload.rate_limits.is_some()
            || proxy_status
            || (!endpoint.is_custom() && snapshot.is_some_and(usage::Snapshot::has_fetched_data)))
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

/// Spawns the fetch child when the schedule is due, then loads the snapshot.
/// The cache and the child that fills it belong to the local login, so the
/// snapshot is matched against it. The caller handles a disabled fetch. The
/// spawn precedes the gate because a seat whose payload carries no rate
/// limits needs that first fetch before the line can open at all.
fn spawn_then_load_snapshot(config: &schema::Config) -> Option<usage::Snapshot> {
    usage::spawn_fetch_if_stale(config);
    let account_uuid = schema::home_dir()
        .map(|h| schema::load_account_info(&h.join(".claude.json")))
        .unwrap_or_default()
        .account_uuid;
    usage::cache_path().and_then(|p| usage::load_snapshot(&p, account_uuid.as_deref()))
}

/// The fetched profile names the account the numbers belong to, and nothing
/// else may: the local login is a substitute the chips would misattribute.
/// Before the first profile fetch both chips stay absent. The plan carries
/// its rate-limit tier as one label, so the chip reads "Max 20x".
fn native_account(snapshot: Option<&usage::Snapshot>) -> (Option<String>, Option<String>) {
    let Some(profile) = snapshot.and_then(|s| s.profile.as_ref()) else {
        return (None, None);
    };
    let plan = profile
        .plan
        .as_deref()
        .map(|plan| plan::label(plan, profile.tier.as_deref()));
    (profile.email.clone(), plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> schema::Config {
        schema::Config {
            advanced_usage_limits_enabled: true,
            ..schema::Config::default()
        }
    }

    fn fetched_profile() -> usage::Snapshot {
        usage::Snapshot {
            profile: Some(usage::Profile {
                email: Some("fetched@example.com".to_string()),
                ..usage::Profile::default()
            }),
            ..usage::Snapshot::default()
        }
    }

    #[test]
    fn usage_line_requires_the_config_flag() {
        let payload = schema::parse_payload(r#"{"rate_limits": {}}"#).unwrap();
        let official = usage::EndpointEnv::default();
        assert!(!usage_line_enabled(
            &schema::Config::default(),
            &payload,
            &official,
            false,
            None
        ));
        assert!(usage_line_enabled(
            &enabled(),
            &payload,
            &official,
            false,
            None
        ));
    }

    #[test]
    fn usage_line_needs_a_payload_proxy_or_snapshot_signal() {
        let with_limits = schema::parse_payload(r#"{"rate_limits": {}}"#).unwrap();
        let without = schema::parse_payload("{}").unwrap();
        let official = usage::EndpointEnv::default();
        let windows = usage::Snapshot {
            utilization: usage::EndpointUtilization {
                five_hour: Some(usage::EndpointWindow {
                    utilization: Some(1.0),
                    resets_at: None,
                }),
                ..usage::EndpointUtilization::default()
            },
            ..usage::Snapshot::default()
        };
        // The child writes the file as soon as it books its first retry, so a schedule alone
        // proves nothing.
        let booked = usage::Snapshot {
            usage_next_at_ms: Some(1),
            ..usage::Snapshot::default()
        };

        assert!(usage_line_enabled(
            &enabled(),
            &with_limits,
            &official,
            false,
            None
        ));
        assert!(usage_line_enabled(
            &enabled(),
            &without,
            &official,
            false,
            Some(&fetched_profile())
        ));
        assert!(usage_line_enabled(
            &enabled(),
            &without,
            &official,
            false,
            Some(&windows)
        ));
        assert!(!usage_line_enabled(
            &enabled(),
            &without,
            &official,
            false,
            Some(&booked)
        ));
        assert!(!usage_line_enabled(
            &enabled(),
            &without,
            &official,
            false,
            None
        ));
    }

    #[test]
    fn usage_line_custom_endpoint_ignores_the_snapshot() {
        let with_limits = schema::parse_payload(r#"{"rate_limits": {}}"#).unwrap();
        let without = schema::parse_payload("{}").unwrap();
        let custom = usage::EndpointEnv {
            base_url: Some("https://gateway.example.com".to_string()),
            ..usage::EndpointEnv::default()
        };
        // The local login's snapshot cannot vouch for a session another account serves.
        assert!(!usage_line_enabled(
            &enabled(),
            &without,
            &custom,
            false,
            Some(&fetched_profile())
        ));
        // Payload rate limits stay ground truth even behind a gateway.
        assert!(usage_line_enabled(
            &enabled(),
            &with_limits,
            &custom,
            false,
            None
        ));
    }

    #[test]
    fn usage_line_accepts_a_proxy_status_behind_a_custom_endpoint() {
        let without = schema::parse_payload("{}").unwrap();
        let custom = usage::EndpointEnv {
            base_url: Some("http://127.0.0.1:8317".to_string()),
            ..usage::EndpointEnv::default()
        };
        assert!(usage_line_enabled(
            &enabled(),
            &without,
            &custom,
            true,
            None
        ));
        assert!(!usage_line_enabled(
            &enabled(),
            &without,
            &custom,
            false,
            None
        ));
    }

    #[test]
    fn native_account_reads_the_snapshot_profile_only() {
        assert_eq!(native_account(None), (None, None));
        let booked = usage::Snapshot {
            usage_next_at_ms: Some(1),
            ..usage::Snapshot::default()
        };
        assert_eq!(native_account(Some(&booked)), (None, None));
        let labeled = usage::Snapshot {
            profile: Some(usage::Profile {
                email: Some("fetched@example.com".to_string()),
                plan: Some("max".to_string()),
                tier: Some("default_claude_max_20x".to_string()),
            }),
            ..usage::Snapshot::default()
        };
        assert_eq!(
            native_account(Some(&labeled)),
            (
                Some("fetched@example.com".to_string()),
                Some("Max 20x".to_string())
            )
        );
        let family_only = usage::Snapshot {
            profile: Some(usage::Profile {
                plan: Some("team".to_string()),
                ..usage::Profile::default()
            }),
            ..usage::Snapshot::default()
        };
        assert_eq!(
            native_account(Some(&family_only)),
            (None, Some("Team".to_string()))
        );
    }
}
