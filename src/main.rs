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
    /// Render per-task rows for Claude Code's subagentStatusLine hook
    #[arg(long = "subagent-statusline")]
    subagent_statusline: bool,
    /// With --install: also write the subagentStatusLine entry
    #[arg(long = "with-subagent-statusline")]
    with_subagent_statusline: bool,
}

const LINE1_DROP: &[&str] = &["cache", "cache_age", "effort"];
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
const SEP: &str = " \u{2502} ";

fn main() {
    let cli = Cli::parse();
    if cli.print_config {
        std::process::exit(commands::print_config::run());
    }
    if cli.fetch_usage {
        std::process::exit(usage::run_fetch());
    }
    if cli.install {
        if let Err(e) = commands::install::install(cli.with_subagent_statusline) {
            eprintln!("claude-statusline: install failed: {e}");
            std::process::exit(1);
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

    let lines: Vec<String> = [
        compose(sections::line1(&ctx), LINE1_DROP),
        compose(sections::line2(&ctx), LINE2_DROP),
    ]
    .into_iter()
    .flatten()
    .collect();
    Some(lines.join("\n"))
}
