use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

pub fn install(with_subagent: bool) -> Result<()> {
    let path = super::settings_path();
    let mut settings = Map::new();
    if path.exists() {
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok());

        // The .bak must hold the user's pre-install state, not our own
        // previous entries: skip it only when everything we may overwrite
        // is already ours.
        let entry_is_ours = |key: &str| {
            parsed
                .as_ref()
                .and_then(|v| v.get(key))
                .and_then(|sl| sl.get("command"))
                .and_then(|c| c.as_str())
                .map(super::print_config::is_our_command)
        };
        let current_is_ours = entry_is_ours("statusLine") == Some(true)
            && (!with_subagent || entry_is_ours("subagentStatusLine").unwrap_or(true));
        if !current_is_ours && let Err(e) = std::fs::copy(&path, bak_path(&path)) {
            eprintln!("Warning: could not create backup: {e}");
        }

        match parsed {
            Some(Value::Object(existing)) => settings = existing,
            _ => eprintln!(
                "Warning: could not parse existing settings.json; writing new settings with statusLine only (backup at {}).",
                bak_path(&path).display()
            ),
        }
    }

    let exe = std::env::current_exe()
        .context("cannot resolve the path of this binary")?
        .display()
        .to_string();
    // Claude Code runs statusLine commands through Git Bash on Windows,
    // which strips unquoted backslashes, so the written path must use
    // forward slashes (Windows accepts them).
    #[cfg(windows)]
    let exe = exe.replace('\\', "/");
    // refreshInterval keeps cache_age live between assistant messages.
    settings.insert(
        "statusLine".to_string(),
        json!({"type": "command", "command": command_string(&exe), "refreshInterval": 10}),
    );
    if with_subagent {
        settings.insert(
            "subagentStatusLine".to_string(),
            json!({
                "type": "command",
                "command": format!("{} --subagent-statusline", command_string(&exe)),
                "refreshInterval": 5
            }),
        );
    }
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
        anyhow::bail!("could not read {}", path.display());
    };

    let entry_is_ours = |settings: &Map<String, Value>, key: &str| {
        settings
            .get(key)
            .and_then(|sl| sl.get("command"))
            .and_then(|c| c.as_str())
            .is_some_and(super::print_config::is_our_command)
    };
    // Only take an entry that is ours: a foreign statusLine/subagentStatusLine
    // written by another tool must survive uninstall untouched.
    let mut removed: Vec<(&str, Value)> = Vec::new();
    for key in ["statusLine", "subagentStatusLine"] {
        if entry_is_ours(&settings, key)
            && let Some(value) = settings.remove(key)
        {
            removed.push((key, value));
        }
    }
    if removed.is_empty() {
        println!("claude-statusline is not installed (no claude-statusline entries in settings).");
        return Ok(());
    }

    let backup: Option<Value> = std::fs::read_to_string(bak_path(&path))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let mut restored = false;
    for (key, removed_entry) in removed {
        if let Some(previous) = backup.as_ref().and_then(|v| v.get(key).cloned())
            && previous != removed_entry
        {
            // Never resurrect our own stale entry: a backup written by an
            // earlier claude-statusline install is not the user's original
            // config.
            let previous_is_ours = previous
                .get("command")
                .and_then(|c| c.as_str())
                .is_some_and(super::print_config::is_our_command);
            if !previous_is_ours {
                settings.insert(key.to_string(), previous);
                restored = true;
            }
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

/// Claude Code passes the command through a shell, so a path containing
/// whitespace must be quoted; print_config's first_token already parses
/// the quoted form back.
fn command_string(exe: &str) -> String {
    if exe.contains(char::is_whitespace) {
        format!("\"{exe}\"")
    } else {
        exe.to_string()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_path_is_unchanged() {
        assert_eq!(
            command_string("/usr/local/bin/claude-statusline"),
            "/usr/local/bin/claude-statusline"
        );
    }

    #[test]
    fn path_with_space_is_quoted() {
        assert_eq!(
            command_string("C:\\Program Files\\claude-statusline.exe"),
            "\"C:\\Program Files\\claude-statusline.exe\""
        );
    }
}
