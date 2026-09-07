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
        let keys: &[&str] = if with_subagent {
            &["statusLine", "subagentStatusLine"]
        } else {
            &["statusLine"]
        };
        match parsed {
            Some(Value::Object(existing)) => {
                if let Err(e) = refresh_backup(&path, &existing, keys) {
                    eprintln!("Warning: could not update backup: {e}");
                }
                settings = existing;
            }
            _ => {
                // Unparseable settings cannot be merged key by key, so the
                // raw bytes are kept verbatim; they are the user's data.
                if let Err(e) = std::fs::copy(&path, bak_path(&path)) {
                    eprintln!("Warning: could not create backup: {e}");
                }
                eprintln!(
                    "Warning: could not parse existing settings.json; writing new settings with statusLine only (backup at {}).",
                    bak_path(&path).display()
                );
            }
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

/// The .bak must hold the user's pre-install state for each entry we
/// write, decided per key: a foreign entry is saved, an entry that is
/// already ours keeps whatever the backup saved before it, and an entry
/// the user removed is removed from the backup so uninstall cannot bring
/// it back. Without a backup yet, the whole file is the starting point so
/// a first install still snapshots everything. A backup that does not
/// parse is the raw copy of settings that did not parse either: the only
/// recovery path the user has, and not ours to rewrite.
fn refresh_backup(path: &Path, current: &Map<String, Value>, keys: &[&str]) -> Result<()> {
    let bak = bak_path(path);
    let mut backup = match std::fs::read_to_string(&bak) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => map,
            _ => return Ok(()),
        },
        Err(_) => current.clone(),
    };
    for key in keys {
        let ours = current
            .get(*key)
            .and_then(|sl| sl.get("command"))
            .and_then(|c| c.as_str())
            .is_some_and(super::print_config::is_our_command);
        match current.get(*key) {
            Some(entry) if !ours => {
                backup.insert((*key).to_string(), entry.clone());
            }
            Some(_) => {}
            None => {
                backup.remove(*key);
            }
        }
    }
    write_atomic(&bak, &Value::Object(backup))
}

/// Claude Code hands the command to a POSIX shell (sh on Unix, Git Bash on
/// Windows), so the path is written as one shell word. Single quotes are
/// the only quoting that shell reads literally: double quotes still expand
/// `$`, backticks and backslashes. print_config's first_token parses the
/// quoted form back.
fn command_string(exe: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._:+=@%-".contains(c);
    if !exe.is_empty() && exe.chars().all(safe) {
        return exe.to_string();
    }
    format!("'{}'", exe.replace('\'', "'\\''"))
}

/// Temp file plus rename: a crash mid-write must never leave the user's
/// Claude Code settings truncated. The temp name carries the process id
/// and a per-process counter, so every write owns a file of its own and
/// none can publish another's unfinished write, whether the other runs
/// in a second install or on a second thread. Settings may carry
/// credentials in an `env` block, so on Unix the file is created private
/// and exclusively, and only then takes the destination's own mode; the
/// rename must never widen what was there.
fn write_atomic(path: &Path, value: &Value) -> Result<()> {
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let serial = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = PathBuf::from(format!(
        "{}.{}.{serial}.tmp",
        path.display(),
        std::process::id()
    ));
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) = write_private(&tmp, &text) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    #[cfg(unix)]
    if let Ok(existing) = std::fs::metadata(path) {
        std::fs::set_permissions(&tmp, existing.permissions()).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(text.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text)
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
    fn safe_paths_stay_bare() {
        assert_eq!(
            command_string("/usr/local/bin/claude-statusline"),
            "/usr/local/bin/claude-statusline"
        );
        assert_eq!(
            command_string("C:/tools/claude-statusline.exe"),
            "C:/tools/claude-statusline.exe"
        );
    }

    #[test]
    fn unsafe_paths_become_one_single_quoted_word() {
        assert_eq!(
            command_string("C:/Program Files/claude-statusline.exe"),
            "'C:/Program Files/claude-statusline.exe'"
        );
        assert_eq!(
            command_string("/opt/O'Connor/claude-statusline"),
            "'/opt/O'\\''Connor/claude-statusline'"
        );
        assert_eq!(
            command_string("/tmp/q$(echo x)/claude-statusline"),
            "'/tmp/q$(echo x)/claude-statusline'"
        );
        assert_eq!(
            command_string("/tmp/`id`/claude-statusline"),
            "'/tmp/`id`/claude-statusline'"
        );
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_keeps_a_restrictive_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&path, &serde_json::json!({"a": 1})).unwrap();
        assert_eq!(mode_of(&path), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_keeps_a_permissive_mode_and_creates_private_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic(&path, &serde_json::json!({"a": 1})).unwrap();
        assert_eq!(mode_of(&path), 0o644, "an open mode is the user's choice");

        let fresh = dir.path().join("new").join("settings.json");
        write_atomic(&fresh, &serde_json::json!({"a": 1})).unwrap();
        assert_eq!(mode_of(&fresh), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_leftover_permissive_temp_file_is_never_written_through() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let stale = dir.path().join("settings.json.1.0.tmp");
        std::fs::write(&stale, "old").unwrap();
        std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic(&path, &serde_json::json!({"a": 1})).unwrap();
        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            "{\n  \"a\": 1\n}"
        );
        assert_eq!(
            std::fs::read_to_string(&stale).unwrap(),
            "old",
            "a stranger's file is not ours to touch"
        );
    }

    #[test]
    fn concurrent_writers_each_publish_a_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::thread::scope(|s| {
            for i in 0..8 {
                let path = path.clone();
                s.spawn(move || {
                    for round in 0..20 {
                        write_atomic(&path, &serde_json::json!({"writer": i, "round": round}))
                            .unwrap();
                    }
                });
            }
        });
        let text = std::fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(&text).expect("the published file is whole");
        assert!(v.get("writer").is_some() && v.get("round").is_some());
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
            .count();
        assert_eq!(leftovers, 0, "every writer removes its own temp file");
    }
}
