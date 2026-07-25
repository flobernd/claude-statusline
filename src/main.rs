mod bar;
mod commands;
mod fit;
mod format;
mod git;
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
// usage_session is intentionally absent so it never drops: the five-hour
// window is the limit users hit first.
const LINE3_DROP: &[&str] = &["usage_plan", "usage_spend", "usage_fable", "usage_week"];
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

    let ctx = sections::Ctx {
        payload: &payload,
        git: &git_info,
        cache_age_ms,
        style: &style,
    };
    let sep = style.paint(SEP, theme::COMMENT);
    let sep_width = fit::visible_width(&sep);

    let disabled = &config.disabled_sections;
    let compose = |chips: Vec<(&'static str, String)>, drop: &[&str]| -> Option<String> {
        let chips: Vec<(&'static str, String)> = chips
            .into_iter()
            .filter(|(name, _)| !disabled.iter().any(|d| d == name))
            .collect();
        let fitted = fit::fit_line(chips, sep_width, width, drop);
        if fitted.is_empty() {
            return None;
        }
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
    let line3 = if config.advanced_usage_limits_enabled {
        let account = schema::home_dir()
            .map(|h| schema::load_account_info(&h.join(".claude.json")))
            .unwrap_or_default();
        if usage_line_enabled(&config, &payload, &account, &usage::EndpointEnv::from_env()) {
            usage::spawn_fetch_if_stale(&config);
            let snapshot = usage::cache_path()
                .and_then(|p| usage::load_snapshot(&p, account.account_uuid.as_deref()));
            let now_epoch_s = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let limits = usage::merge(
                payload.rate_limits.as_ref(),
                snapshot.as_ref().map(|s| &s.utilization),
                now_epoch_s,
            );
            let plan = account
                .organization_type
                .as_deref()
                .and_then(|t| t.strip_prefix("claude_"));
            compose(
                sections::line3(&limits, plan, &style, now_epoch_s),
                LINE3_DROP,
            )
        } else {
            None
        }
    } else {
        None
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
        compose(line1_chips, LINE1_DROP),
        compose(sections::line2(&ctx), LINE2_DROP),
        line3,
    ]
    .into_iter()
    .flatten()
    .collect();
    Some(lines.join("\n"))
}

/// The usage line is for native Anthropic subscriptions only. Enterprise
/// seats may receive no payload rate_limits at all, so the account type
/// from ~/.claude.json is the alternate signal that keeps the line alive
/// for them. That fallback is a login artifact though: it stays true when
/// the session talks to a gateway, Bedrock, or Vertex, so a custom
/// endpoint disables it. Payload rate_limits win unconditionally because
/// they only appear when Anthropic subscription headers actually flow.
fn usage_line_enabled(
    config: &schema::Config,
    payload: &schema::Payload,
    account: &schema::AccountInfo,
    endpoint: &usage::EndpointEnv,
) -> bool {
    config.advanced_usage_limits_enabled
        && (payload.rate_limits.is_some()
            || (!endpoint.is_custom()
                && account
                    .organization_type
                    .as_deref()
                    .is_some_and(|t| t.starts_with("claude_"))))
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
        };
        let official = usage::EndpointEnv::default();
        assert!(!usage_line_enabled(
            &schema::Config::default(),
            &payload,
            &account,
            &official
        ));
        let enabled = schema::Config {
            advanced_usage_limits_enabled: true,
            ..schema::Config::default()
        };
        assert!(usage_line_enabled(&enabled, &payload, &account, &official));
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
        };
        let external = schema::AccountInfo {
            organization_type: Some("external".to_string()),
            account_uuid: None,
        };
        let unknown = schema::AccountInfo::default();
        let official = usage::EndpointEnv::default();

        assert!(usage_line_enabled(
            &enabled,
            &with_limits,
            &unknown,
            &official
        ));
        assert!(usage_line_enabled(&enabled, &without, &native, &official));
        assert!(!usage_line_enabled(
            &enabled, &without, &external, &official
        ));
        assert!(!usage_line_enabled(&enabled, &without, &unknown, &official));
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
        };
        let custom = usage::EndpointEnv {
            base_url: Some("https://gateway.example.com".to_string()),
            ..usage::EndpointEnv::default()
        };

        // The login artifact alone no longer keeps the line alive.
        assert!(!usage_line_enabled(&enabled, &without, &native, &custom));
        // Payload rate limits stay ground truth even behind a gateway.
        assert!(usage_line_enabled(&enabled, &with_limits, &native, &custom));
    }
}
