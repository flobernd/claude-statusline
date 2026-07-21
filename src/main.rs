mod theme;
mod format;
mod bar;
mod fit;
mod schema;
mod git;
mod transcript;
mod sections;
mod commands;

use clap::Parser;
use std::io::Read;

#[derive(Parser)]
#[command(name = "claude-statusline", version, about = "Tokyo Night statusline for Claude Code")]
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
}

const LINE1_DROP: &[&str] = &["cache_age", "cache", "context_tokens", "effort", "model"];
const LINE2_DROP: &[&str] = &["git_worktree", "git_sync", "git_stash", "project", "worktree", "pr", "git_state"];
const SEP: &str = " \u{2502} ";

fn main() {
    let cli = Cli::parse();
    if cli.print_config {
        std::process::exit(commands::print_config::run());
    }
    if cli.setup || cli.install || cli.uninstall {
        // Wired up in later commits.
        return;
    }
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return;
    }
    if raw.trim().is_empty() {
        return;
    }
    // The statusline must never surface a crash: a panic would splat a
    // backtrace into the Claude Code footer on every render tick. The
    // silent hook keeps the default handler from writing its own
    // multi-line message; the payload text is folded into our single
    // stderr line instead.
    std::panic::set_hook(Box::new(|_| {}));
    match std::panic::catch_unwind(|| render(&raw)) {
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
    parse("CLAUDE_STATUSLINE_WIDTH").or_else(|| parse("COLUMNS")).unwrap_or(100)
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

    let ctx = sections::Ctx { payload: &payload, git: &git_info, cache_age_ms, style: &style };
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
        Some(fitted.into_iter().map(|(_, r)| r).collect::<Vec<_>>().join(&sep))
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
