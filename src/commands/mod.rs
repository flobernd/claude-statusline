pub mod install;
pub mod print_config;
pub mod setup;

use std::path::PathBuf;

/// Single chokepoint for the settings location so tests (and CI) can
/// redirect all settings I/O away from the real ~/.claude/settings.json.
pub fn settings_path() -> PathBuf {
    if let Some(v) = std::env::var_os("CLAUDE_STATUSLINE_SETTINGS_PATH") {
        let s = v.to_string_lossy();
        if !s.trim().is_empty() {
            return PathBuf::from(s.into_owned());
        }
    }
    crate::schema::home_dir()
        .unwrap_or_default()
        .join(".claude")
        .join("settings.json")
}
