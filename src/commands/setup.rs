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

    println!();
    super::install::install(false)?;
    println!();
    println!("Setup complete.");
    println!("Uninstall any time with: claude-statusline --uninstall");
    Ok(())
}
