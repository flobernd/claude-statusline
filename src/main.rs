mod theme;
mod format;
mod bar;
mod fit;
mod schema;
mod git;
mod transcript;
mod sections;

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

fn main() {
    let cli = Cli::parse();
    if cli.setup || cli.install || cli.uninstall || cli.print_config {
        // Lifecycle commands are wired up in later commits.
        return;
    }
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return;
    }
    if raw.trim().is_empty() {}
}
