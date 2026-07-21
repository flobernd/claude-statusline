use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

pub fn install() -> Result<()> {
    let path = super::settings_path();
    let mut settings = Map::new();
    if path.exists() {
        if let Err(e) = std::fs::copy(&path, bak_path(&path)) {
            eprintln!("Warning: could not create backup: {e}");
        }
        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
            Some(Value::Object(existing)) => settings = existing,
            _ => println!(
                "Warning: could not parse existing settings.json; writing new settings with statusLine only (backup at {}).",
                bak_path(&path).display()
            ),
        }
    }

    let exe = std::env::current_exe()
        .context("cannot resolve the path of this binary")?
        .display()
        .to_string();
    // refreshInterval keeps cache_age live between assistant messages.
    settings.insert(
        "statusLine".to_string(),
        json!({"type": "command", "command": exe, "refreshInterval": 10}),
    );
    write_atomic(&path, &Value::Object(settings))?;

    println!("Installed claude-statusline into {}", path.display());
    println!("Restart Claude Code to see your new status line.");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let path = super::settings_path();
    if !path.exists() {
        println!("No settings file found at {}", path.display());
        return Ok(());
    }
    let parsed = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let Some(Value::Object(mut settings)) = parsed else {
        println!("Error: could not read {}", path.display());
        return Ok(());
    };
    if !settings.contains_key("statusLine") {
        println!("claude-statusline is not installed (no statusLine in settings).");
        return Ok(());
    }

    let removed = settings.remove("statusLine");
    let mut restored = false;
    if let Some(previous) = std::fs::read_to_string(bak_path(&path))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("statusLine").cloned())
    {
        if Some(&previous) != removed.as_ref() {
            settings.insert("statusLine".to_string(), previous);
            restored = true;
        }
    }
    write_atomic(&path, &Value::Object(settings))?;

    if restored {
        println!("Restored previous statusLine config from backup.");
    } else {
        println!("Removed statusLine from {}", path.display());
    }
    println!("Restart Claude Code for the change to take effect.");
    Ok(())
}

fn bak_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.bak", path.display()))
}

/// Temp file plus rename: a crash mid-write must never leave the user's
/// Claude Code settings truncated.
fn write_atomic(path: &Path, value: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}
