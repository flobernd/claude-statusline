use serde_json::Value;

pub fn run() -> i32 {
    let path = super::settings_path();
    let mut installed = false;
    let mut command = String::new();
    let mut sl_type = String::new();
    let mut refresh = String::new();
    let mut sub_installed = false;
    let mut sub_command = String::new();
    let mut sub_type = String::new();
    let mut sub_refresh = String::new();
    let mut state = "missing";

    if path.exists() {
        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
            Some(v) => {
                state = "ok";
                let read_entry = |key: &str| {
                    let Some(sl) = v.get(key).and_then(|s| s.as_object()) else {
                        return (String::new(), String::new(), String::new(), false);
                    };
                    let sl_type = sl
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string();
                    let command = sl
                        .get("command")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    let refresh = sl
                        .get("refreshInterval")
                        .and_then(|r| r.as_f64())
                        .filter(|r| *r >= 0.0)
                        .map(|r| (r as u64).to_string())
                        .unwrap_or_default();
                    let installed = is_our_command(&command);
                    (command, sl_type, refresh, installed)
                };
                (command, sl_type, refresh, installed) = read_entry("statusLine");
                (sub_command, sub_type, sub_refresh, sub_installed) =
                    read_entry("subagentStatusLine");
            }
            None => state = "unreadable",
        }
    }

    // Newlines are stripped so the output keeps its fixed line count for
    // parsers no matter what the settings file contains.
    let clean = |s: &str| s.replace(['\r', '\n'], " ");
    println!("installed={installed}");
    println!("command={}", clean(&command));
    println!("type={}", clean(&sl_type));
    println!("refreshInterval={refresh}");
    println!("version={}", env!("CARGO_PKG_VERSION"));
    println!("settings_path={}", clean(&path.display().to_string()));
    println!("settings_state={state}");
    println!("subagent_installed={sub_installed}");
    println!("subagent_command={}", clean(&sub_command));
    println!("subagent_type={}", clean(&sub_type));
    println!("subagent_refreshInterval={sub_refresh}");

    if state == "unreadable" {
        2
    } else if installed {
        0
    } else {
        1
    }
}

/// True when the configured command launches this binary, matching by
/// basename so absolute paths and bare names both count.
pub fn is_our_command(command: &str) -> bool {
    let first = first_token(command);
    let base = first.split(['/', '\\']).next_back().unwrap_or("");
    base.strip_suffix(".exe").unwrap_or(base) == "claude-statusline"
}

/// First token of a command line, honoring a leading double-quoted path
/// (Windows install paths contain spaces).
fn first_token(command: &str) -> &str {
    let c = command.trim();
    match c.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        None => c.split_whitespace().next().unwrap_or(""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_our_binary_in_common_forms() {
        assert!(is_our_command("claude-statusline"));
        assert!(is_our_command("/usr/local/bin/claude-statusline"));
        assert!(is_our_command("C:\\tools\\claude-statusline.exe"));
        assert!(is_our_command(
            "\"C:\\Program Files\\claude-statusline.exe\""
        ));
        assert!(is_our_command("C:/tools/claude-statusline.exe"));
        assert!(is_our_command(
            "\"C:/Program Files/claude-statusline.exe\" --subagent-statusline"
        ));
    }

    #[test]
    fn rejects_other_commands() {
        assert!(!is_our_command(""));
        assert!(!is_our_command("claude-status"));
        assert!(!is_our_command("my-claude-statusline-fork"));
        assert!(!is_our_command("python -m claude_statusline"));
    }
}
