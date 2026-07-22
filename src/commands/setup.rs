use anyhow::Result;
use std::io::{BufRead, Write};

pub fn run() -> Result<()> {
    println!(
        "claude-statusline v{} setup wizard",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("Preview:");
    let clickable = crate::schema::home_dir()
        .map(|h| {
            crate::schema::load_config(&h.join(".claude").join("claude-statusline.json"))
                .clickable_links
        })
        .unwrap_or(true);
    let style = crate::theme::Style::from_env(clickable);
    for line in crate::sections::preview(&style).lines() {
        println!("  {line}");
    }
    println!();
    print!("Install into {}? [Y/n]: ", super::settings_path().display());
    std::io::stdout().flush()?;

    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).unwrap_or(0) == 0 {
        println!();
        println!("Setup cancelled.");
        return Ok(());
    }
    let answer = answer.trim().to_lowercase();
    if !answer.is_empty() && answer != "y" && answer != "yes" {
        println!("Setup cancelled.");
        return Ok(());
    }

    print!("Also install the subagent status line (one row per running agent task)? [y/N]: ");
    std::io::stdout().flush()?;
    let mut sub_answer = String::new();
    // EOF counts as the default "no": the main install must still happen.
    let _ = std::io::stdin().lock().read_line(&mut sub_answer);
    let with_subagent = matches!(sub_answer.trim().to_lowercase().as_str(), "y" | "yes");
    if with_subagent {
        println!();
        println!("Subagent row preview:");
        for line in crate::subagent::preview(&style).lines() {
            println!("  {line}");
        }
    }

    println!();
    super::install::install(with_subagent)?;
    println!();
    println!("Setup complete.");
    println!("Uninstall any time with: claude-statusline --uninstall");
    Ok(())
}
