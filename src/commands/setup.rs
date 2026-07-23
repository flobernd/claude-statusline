use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::Path;

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

    print!("Show the subscription usage limits line (session/weekly/model/spend)? [y/N]: ");
    std::io::stdout().flush()?;
    let mut usage_answer = String::new();
    // EOF counts as the default "no": the install must still happen.
    let _ = std::io::stdin().lock().read_line(&mut usage_answer);
    if matches!(usage_answer.trim().to_lowercase().as_str(), "y" | "yes") {
        enable_usage_limits(
            &crate::schema::home_dir()
                .unwrap_or_default()
                .join(".claude")
                .join("claude-statusline.json"),
        )?;
        println!();
        println!("Usage limits line preview:");
        for line in crate::sections::usage_preview(&style).lines() {
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

/// Flip the opt-in flag in the statusline config while keeping every
/// other key intact: the wizard must never clobber hand-edited settings.
pub fn enable_usage_limits(path: &Path) -> Result<()> {
    let mut root: Value = match std::fs::read_to_string(path) {
        // Failing beats overwriting: an unparseable file is the user's
        // data, not ours to discard.
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("cannot parse {}", path.display()))?,
        Err(_) => json!({}),
    };
    let Some(object) = root.as_object_mut() else {
        anyhow::bail!("{} is not a JSON object", path.display());
    };
    object.insert(
        "advanced_usage_limits_enabled".to_string(),
        Value::Bool(true),
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = serde_json::to_string_pretty(&root)?;
    text.push('\n');
    std::fs::write(path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_usage_limits_creates_a_missing_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude").join("claude-statusline.json");
        enable_usage_limits(&path).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["advanced_usage_limits_enabled"], true);
        assert_eq!(value.as_object().unwrap().len(), 1);
    }

    #[test]
    fn enable_usage_limits_preserves_existing_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline.json");
        std::fs::write(
            &path,
            r#"{"advanced_usage_limits_enabled": false, "clickable_links": false, "custom_key": [1, 2]}"#,
        )
        .unwrap();
        enable_usage_limits(&path).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["advanced_usage_limits_enabled"], true);
        assert_eq!(value["clickable_links"], false);
        assert_eq!(value["custom_key"], serde_json::json!([1, 2]));
    }

    #[test]
    fn enable_usage_limits_rejects_a_malformed_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-statusline.json");
        std::fs::write(&path, "{broken").unwrap();
        assert!(enable_usage_limits(&path).is_err());
        // The unparseable file must survive untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{broken");
    }
}
