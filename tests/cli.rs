use std::process::Command;

#[test]
fn version_flag_prints_name_and_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_claude-statusline"))
        .arg("--version")
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("claude-statusline"));
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn empty_stdin_prints_nothing() {
    let out = Command::new(env!("CARGO_BIN_EXE_claude-statusline"))
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
}

use std::io::Write;
use std::process::Stdio;

/// Run the binary with controlled env: NO_COLOR output, fixed width,
/// HOME pointed at a temp dir so no real config or transcript leaks in.
/// The working directory is also pinned to that temp dir: without it the
/// child inherits cargo test's cwd (this crate's own git checkout), and a
/// payload with no explicit workspace would pick up its branch as a false
/// line2 chip.
fn run_statusline(stdin_data: &str, width: &str, home: &std::path::Path) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_claude-statusline"))
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .env("CLAUDE_STATUSLINE_WIDTH", width)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary runs");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_data.as_bytes())
        .unwrap();
    child.wait_with_output().expect("binary exits")
}

const SAMPLE: &str = r#"{
    "model": {"display_name": "Sonnet 5"},
    "effort": {"level": "xhigh"},
    "context_window": {
        "used_percentage": 42, "context_window_size": 1000000,
        "current_usage": {"input_tokens": 412000, "output_tokens": 18500,
            "cache_creation_input_tokens": 12000, "cache_read_input_tokens": 365000}
    }
}"#;

#[test]
fn renders_line1_from_sample_payload() {
    let home = tempfile::tempdir().unwrap();
    let out = run_statusline(SAMPLE, "200", home.path());
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("420K/1M (42%)"), "stdout: {stdout}");
    assert!(stdout.contains("cache:46%"));
    assert!(stdout.contains("Sonnet 5"));
    assert!(stdout.contains("\u{2502} xhigh"), "stdout: {stdout}");
    assert!(stdout.contains(" \u{2502} "));
}

#[test]
fn undecodable_stdin_prints_question_mark() {
    let home = tempfile::tempdir().unwrap();
    let out = run_statusline("{definitely not json", "200", home.path());
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "?");
    assert!(!out.stderr.is_empty());
}

#[test]
fn non_utf8_stdin_prints_question_mark() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_claude-statusline"))
        .env("NO_COLOR", "1")
        .env("CLAUDE_STATUSLINE_WIDTH", "200")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary runs");
    use std::io::Write as _;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"\xff\xfe{")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "?");
}

#[test]
fn payload_control_characters_never_reach_stdout() {
    let home = tempfile::tempdir().unwrap();
    let payload = r#"{"model": {"display_name": "evil[2Jwiped\nsecond"}}"#;
    let out = run_statusline(payload, "200", home.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains('\u{1b}'));
    assert_eq!(stdout.trim_end_matches('\n').lines().count(), 1);
    assert!(stdout.contains("wiped"));
}

#[test]
fn narrow_width_drops_cache_but_keeps_protected_chips() {
    let home = tempfile::tempdir().unwrap();
    let out = run_statusline(SAMPLE, "45", home.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("420K/1M (42%)"), "stdout: {stdout}"); // context_tokens protected
    assert!(stdout.contains("Sonnet 5"), "stdout: {stdout}"); // model protected
    assert!(!stdout.contains("cache:46%"), "stdout: {stdout}"); // cache drops first
}

#[test]
fn disabled_sections_config_hides_chips() {
    let home = tempfile::tempdir().unwrap();
    let claude_dir = home.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("claude-statusline.json"),
        r#"{"disabled_sections": ["cache", "effort"]}"#,
    )
    .unwrap();
    let out = run_statusline(SAMPLE, "200", home.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("cache:46%"));
    assert!(!stdout.contains("xhigh"));
    assert!(stdout.contains("420K/1M"));
}

#[test]
fn line2_renders_from_temp_git_repo() {
    let home = tempfile::tempdir().unwrap();
    let repo = home.path().join("myrepo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok);
    };
    git(&["init", "-b", "main"]);
    std::fs::write(repo.join("f.txt"), "x\n").unwrap();
    git(&["add", "f.txt"]);
    git(&["commit", "-m", "init"]);

    let payload = format!(
        r#"{{"workspace": {{"current_dir": {dir:?}, "project_dir": {dir:?}}}}}"#,
        dir = repo.to_string_lossy()
    );
    let out = run_statusline(&payload, "200", home.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\u{2387} myrepo/main"), "stdout: {stdout}");
    // Inside a git repo the branch chip carries the location, so the cwd
    // chip is suppressed.
    assert!(!stdout.contains('\u{2302}'), "stdout: {stdout}");
}

#[test]
fn cache_age_renders_from_transcript_under_home() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".claude").join("projects").join("p");
    std::fs::create_dir_all(&dir).unwrap();
    let transcript = dir.join("s.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"assistant","timestamp":"2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    let payload = format!(
        r#"{{"transcript_path": {p:?}}}"#,
        p = transcript.to_string_lossy()
    );
    let out = run_statusline(&payload, "200", home.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("cache_age:"), "stdout: {stdout}");
}

fn print_config(settings: Option<&str>) -> (std::process::Output, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    if let Some(content) = settings {
        std::fs::write(&path, content).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_claude-statusline"))
        .arg("--print-config")
        .env("CLAUDE_STATUSLINE_SETTINGS_PATH", &path)
        .output()
        .expect("binary runs");
    (out, dir)
}

#[test]
fn print_config_missing_settings_reports_not_installed() {
    let (out, _dir) = print_config(None);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("installed=false"));
    assert!(stdout.contains("settings_state=missing"));
}

#[test]
fn print_config_installed_reports_zero() {
    let (out, _dir) = print_config(Some(
        r#"{"statusLine": {"type": "command", "command": "/opt/claude-statusline", "refreshInterval": 10}}"#,
    ));
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("installed=true"));
    assert!(stdout.contains("command=/opt/claude-statusline"));
    assert!(stdout.contains("refreshInterval=10"));
    assert!(stdout.contains("settings_state=ok"));
}

#[test]
fn print_config_corrupt_settings_exits_two() {
    let (out, _dir) = print_config(Some("{broken"));
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("settings_state=unreadable"));
}

fn run_with_settings(args: &[&str], path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_claude-statusline"))
        .args(args)
        .env("CLAUDE_STATUSLINE_SETTINGS_PATH", path)
        .output()
        .expect("binary runs")
}

#[test]
fn install_preserves_other_keys_and_backs_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"model": "opus", "statusLine": {"type": "command", "command": "other-tool"}}"#,
    )
    .unwrap();

    let out = run_with_settings(&["--install"], &path);
    assert!(out.status.success());

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["model"], "opus");
    let cmd = v["statusLine"]["command"].as_str().unwrap();
    assert!(cmd.contains("claude-statusline"));
    assert_eq!(v["statusLine"]["refreshInterval"], 10);
    // Backup holds the pre-install state.
    let bak: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{}.bak", path.display())).unwrap())
            .unwrap();
    assert_eq!(bak["statusLine"]["command"], "other-tool");
}

#[test]
fn reinstall_preserves_original_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"statusLine": {"type": "command", "command": "other-tool"}}"#,
    )
    .unwrap();
    assert!(run_with_settings(&["--install"], &path).status.success());
    assert!(run_with_settings(&["--install"], &path).status.success());
    let bak: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{}.bak", path.display())).unwrap())
            .unwrap();
    assert_eq!(bak["statusLine"]["command"], "other-tool");
}

#[test]
fn uninstall_restores_previous_statusline_from_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"statusLine": {"type": "command", "command": "other-tool"}}"#,
    )
    .unwrap();
    assert!(run_with_settings(&["--install"], &path).status.success());
    assert!(run_with_settings(&["--uninstall"], &path).status.success());

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["statusLine"]["command"], "other-tool");
}

#[test]
fn uninstall_never_restores_our_own_stale_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"statusLine": {"type": "command", "command": "/old/claude-statusline"}}"#,
    )
    .unwrap();
    std::fs::write(
        format!("{}.bak", path.display()),
        r#"{"statusLine": {"type": "command", "command": "/older/claude-statusline"}}"#,
    )
    .unwrap();
    assert!(run_with_settings(&["--uninstall"], &path).status.success());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(v.get("statusLine").is_none());
}

#[test]
fn uninstall_without_backup_removes_statusline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"keep": 1, "statusLine": {"type": "command", "command": "claude-statusline"}}"#,
    )
    .unwrap();
    assert!(run_with_settings(&["--uninstall"], &path).status.success());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(v.get("statusLine").is_none());
    assert_eq!(v["keep"], 1);
}

#[test]
fn install_then_print_config_reports_installed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    assert!(run_with_settings(&["--install"], &path).status.success());
    let out = run_with_settings(&["--print-config"], &path);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("installed=true"));
}

/// HOME/USERPROFILE are pinned to the settings dir: setup's preview now
/// reads ~/.claude/claude-statusline.json for the clickable-links config,
/// and without an override that would fall through to the real user home.
fn run_setup(answer: &str, path: &std::path::Path) -> std::process::Output {
    let home = path.parent().expect("settings path has a parent dir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_claude-statusline"))
        .arg("--setup")
        .env("CLAUDE_STATUSLINE_SETTINGS_PATH", path)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary runs");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(answer.as_bytes())
        .unwrap();
    child.wait_with_output().expect("binary exits")
}

#[test]
fn setup_confirm_installs_and_shows_preview() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let out = run_setup("y\n", &path);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Preview:"));
    assert!(stdout.contains("420K/1M"));
    assert!(stdout.contains("\u{2387} myapp/feat/statusline"));
    assert!(stdout.contains("Setup complete."));
    assert!(path.exists());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(v.get("subagentStatusLine").is_none());
}

#[test]
fn setup_decline_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let out = run_setup("n\n", &path);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Setup cancelled."));
    assert!(!path.exists());
}

#[test]
fn setup_eof_cancels_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let out = run_setup("", &path);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Setup cancelled."));
    assert!(!path.exists());
}

fn run_subagent(stdin_data: &str, home: &std::path::Path, colors: bool) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_claude-statusline"));
    cmd.arg("--subagent-statusline")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if colors {
        cmd.env("FORCE_COLOR", "1");
    } else {
        cmd.env("NO_COLOR", "1").env_remove("FORCE_COLOR");
    }
    let mut child = cmd.spawn().expect("binary runs");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_data.as_bytes())
        .unwrap();
    child.wait_with_output().expect("binary exits")
}

const SUBAGENT_SAMPLE: &str = r#"{
    "columns": 200,
    "tasks": [
        {"id": "t1", "type": "local_agent", "name": "Explore",
         "label": "Searching for callers", "model": "claude-sonnet-5",
         "contextWindowSize": 200000, "tokenCount": 82000},
        {"id": "t2", "type": "local_bash", "label": "cargo build --release"},
        {"type": "local_agent", "name": "NoId", "label": "ignored"}
    ]
}"#;

#[test]
fn subagent_mode_emits_one_json_line_per_agent_task() {
    let home = tempfile::tempdir().unwrap();
    let out = run_subagent(SUBAGENT_SAMPLE, home.path(), false);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout: {stdout}");
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(v["id"], "t1");
    let content = v["content"].as_str().unwrap();
    assert!(
        content.contains("Explore \u{2502} Searching for callers"),
        "content: {content}"
    );
    assert!(content.contains("82K/200K (41%)"));
    assert!(content.contains("claude-sonnet-5"));
    assert!(!content.contains('\u{1b}'));
}

#[test]
fn subagent_content_carries_ansi_when_colors_are_on() {
    let home = tempfile::tempdir().unwrap();
    let out = run_subagent(SUBAGENT_SAMPLE, home.path(), true);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
    assert!(v["content"].as_str().unwrap().contains("\u{1b}[38;2;"));
}

#[test]
fn subagent_payload_columns_bound_each_row() {
    let home = tempfile::tempdir().unwrap();
    let narrow = SUBAGENT_SAMPLE.replace("\"columns\": 200", "\"columns\": 30");
    let out = run_subagent(&narrow, home.path(), false);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
    // At 30 columns the activity goes first, then the model chip.
    let content = v["content"].as_str().unwrap();
    assert_eq!(content, "Explore \u{2502} 82K/200K (41%)");
}

#[test]
fn subagent_disabled_sections_hide_chips_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    let claude_dir = home.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("claude-statusline.json"),
        r#"{"subagent_disabled_sections": ["activity", "model"]}"#,
    )
    .unwrap();
    let out = run_subagent(SUBAGENT_SAMPLE, home.path(), false);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
    assert_eq!(v["content"], "Explore \u{2502} 82K/200K (41%)");
}

#[test]
fn subagent_location_chip_for_worktree_task() {
    let home = tempfile::tempdir().unwrap();
    let repo = home.path().join("myrepo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok);
    };
    git(&["init", "-b", "main"]);
    std::fs::write(repo.join("f.txt"), "x\n").unwrap();
    git(&["add", "f.txt"]);
    git(&["commit", "-m", "init"]);
    let wt = home.path().join("wt-fix");
    git(&["worktree", "add", &wt.to_string_lossy(), "-b", "fix-1"]);

    let payload = format!(
        r#"{{"columns": 200, "cwd": {repo:?}, "tasks": [
            {{"id": "t1", "type": "local_agent", "name": "Explore", "cwd": {wt:?}}},
            {{"id": "t2", "type": "local_agent", "name": "Local", "cwd": {repo:?}}}
        ]}}"#,
        repo = repo.to_string_lossy(),
        wt = wt.to_string_lossy()
    );
    let out = run_subagent(&payload, home.path(), false);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows[0]["content"], "Explore \u{2502} \u{2387} myrepo/fix-1");
    assert_eq!(rows[1]["content"], "Local"); // same location: no chip
}

#[test]
fn subagent_location_chip_for_plain_directory_task() {
    let home = tempfile::tempdir().unwrap();
    let scratch = home.path().join("scratch-dir");
    std::fs::create_dir_all(&scratch).unwrap();
    let payload = format!(
        r#"{{"columns": 200, "cwd": {home:?}, "tasks": [
            {{"id": "t1", "type": "local_agent", "name": "builder", "cwd": {scratch:?}}}
        ]}}"#,
        home = home.path().to_string_lossy(),
        scratch = scratch.to_string_lossy()
    );
    let out = run_subagent(&payload, home.path(), false);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
    assert_eq!(v["content"], "builder \u{2502} \u{2302} scratch-dir");
}

#[test]
fn subagent_undecodable_payload_emits_nothing_but_logs() {
    let home = tempfile::tempdir().unwrap();
    let out = run_subagent("{definitely not json", home.path(), false);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}

#[test]
fn install_with_subagent_writes_both_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let out = run_with_settings(&["--install", "--with-subagent-statusline"], &path);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        v["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("claude-statusline")
    );
    let sub = &v["subagentStatusLine"];
    assert_eq!(sub["type"], "command");
    assert_eq!(sub["refreshInterval"], 5);
    let cmd = sub["command"].as_str().unwrap();
    assert!(cmd.contains("claude-statusline"));
    assert!(cmd.ends_with(" --subagent-statusline"), "command: {cmd}");
}

#[test]
fn install_writes_forward_slash_command_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let out = run_with_settings(&["--install", "--with-subagent-statusline"], &path);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    // Git Bash runs statusLine commands on Windows and strips unquoted
    // backslashes, so a backslash path breaks silently.
    for key in ["statusLine", "subagentStatusLine"] {
        let cmd = v[key]["command"].as_str().unwrap();
        assert!(!cmd.contains('\\'), "{key} command: {cmd}");
    }
}

#[test]
fn plain_install_leaves_foreign_subagent_entry_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"subagentStatusLine": {"type": "command", "command": "other-tool"}}"#,
    )
    .unwrap();
    assert!(run_with_settings(&["--install"], &path).status.success());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["subagentStatusLine"]["command"], "other-tool");
}

#[test]
fn uninstall_removes_subagent_entry_and_restores_foreign_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"subagentStatusLine": {"type": "command", "command": "other-sub"}}"#,
    )
    .unwrap();
    assert!(
        run_with_settings(&["--install", "--with-subagent-statusline"], &path)
            .status
            .success()
    );
    assert!(run_with_settings(&["--uninstall"], &path).status.success());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["subagentStatusLine"]["command"], "other-sub");
    assert!(v.get("statusLine").is_none());
}

#[test]
fn foreign_subagent_entry_survives_plain_install_and_uninstall() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"subagentStatusLine": {"type": "command", "command": "other-sub-tool"}}"#,
    )
    .unwrap();
    assert!(run_with_settings(&["--install"], &path).status.success());
    assert!(run_with_settings(&["--uninstall"], &path).status.success());

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["subagentStatusLine"]["command"], "other-sub-tool");
    assert!(v.get("statusLine").is_none());
}

#[test]
fn plain_reinstall_preserves_original_backup_despite_foreign_subagent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"statusLine": {"type": "command", "command": "old-tool"}}"#,
    )
    .unwrap();
    assert!(run_with_settings(&["--install"], &path).status.success());

    // A foreign tool adds its own subagentStatusLine entry after our install.
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    v["subagentStatusLine"] = serde_json::json!({"type": "command", "command": "other-tool"});
    std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();

    assert!(run_with_settings(&["--install"], &path).status.success());

    let bak: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{}.bak", path.display())).unwrap())
            .unwrap();
    assert_eq!(bak["statusLine"]["command"], "old-tool");
}

#[test]
fn uninstall_with_only_foreign_entries_reports_not_installed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"statusLine": {"type": "command", "command": "other-tool"}}"#,
    )
    .unwrap();
    let out = run_with_settings(&["--uninstall"], &path);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("not installed"));

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["statusLine"]["command"], "other-tool");
}

#[test]
fn print_config_reports_subagent_entry() {
    let (out, _dir) = print_config(Some(
        r#"{"subagentStatusLine": {"type": "command", "command": "/opt/claude-statusline --subagent-statusline", "refreshInterval": 5}}"#,
    ));
    // Exit code stays keyed to the main entry, which is absent here.
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("subagent_installed=true"));
    assert!(stdout.contains("subagent_command=/opt/claude-statusline --subagent-statusline"));
    assert!(stdout.contains("subagent_type=command"));
    assert!(stdout.contains("subagent_refreshInterval=5"));
    assert!(stdout.contains("installed=false"));
}

#[test]
fn setup_with_subagent_installs_both_and_previews_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let out = run_setup("y\ny\n", &path);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Subagent row preview:"), "stdout: {stdout}");
    assert!(stdout.contains("82K/200K (41%)"));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(v.get("statusLine").is_some());
    assert!(v.get("subagentStatusLine").is_some());
}

#[test]
fn setup_declining_subagent_installs_main_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let out = run_setup("y\nn\n", &path);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(v.get("statusLine").is_some());
    assert!(v.get("subagentStatusLine").is_none());
}

#[test]
fn fetch_update_flag_is_a_silent_no_op_when_disabled() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_claude-statusline"))
        .arg("--fetch-update")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    // Default interval 0: no snapshot may appear and no network runs.
    assert!(
        !home
            .path()
            .join(".claude")
            .join("claude-statusline-update.json")
            .exists()
    );
}

/// Config with the check enabled plus a fresh snapshot. The fresh
/// fetched_at_ms keeps the render tick from spawning a real network fetch
/// child during the test.
fn write_update_snapshot(home: &std::path::Path, latest: &str) {
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("claude-statusline.json"),
        r#"{"update_check_interval_minutes": 1440}"#,
    )
    .unwrap();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    std::fs::write(
        claude_dir.join("claude-statusline-update.json"),
        format!(
            r#"{{"fetched_at_ms": {now_ms}, "latest_version": {latest:?}, "release_url": "https://github.com/flobernd/claude-statusline/releases/tag/{latest}"}}"#
        ),
    )
    .unwrap();
}

#[test]
fn update_chip_renders_from_a_seeded_snapshot() {
    let home = tempfile::tempdir().unwrap();
    write_update_snapshot(home.path(), "99.0.0");
    let out = run_statusline(SAMPLE, "200", home.path());
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\u{2B06} 99.0.0"), "stdout: {stdout}");
    // The chip sits on line 1, after the effort chip.
    let line1 = stdout.lines().next().unwrap();
    assert!(
        line1.contains("xhigh \u{2502} \u{2B06} 99.0.0"),
        "line1: {line1}"
    );
}

#[test]
fn update_chip_stays_hidden_without_opt_in() {
    let home = tempfile::tempdir().unwrap();
    write_update_snapshot(home.path(), "99.0.0");
    // Interval 0 (the default) disables the feature even with a snapshot.
    std::fs::write(
        home.path().join(".claude").join("claude-statusline.json"),
        "{}",
    )
    .unwrap();
    let out = run_statusline(SAMPLE, "200", home.path());
    assert!(!String::from_utf8_lossy(&out.stdout).contains('\u{2B06}'));
}

#[test]
fn update_chip_respects_disabled_sections() {
    let home = tempfile::tempdir().unwrap();
    write_update_snapshot(home.path(), "99.0.0");
    std::fs::write(
        home.path().join(".claude").join("claude-statusline.json"),
        r#"{"update_check_interval_minutes": 1440, "disabled_sections": ["update"]}"#,
    )
    .unwrap();
    let out = run_statusline(SAMPLE, "200", home.path());
    assert!(!String::from_utf8_lossy(&out.stdout).contains('\u{2B06}'));
}

#[test]
fn update_chip_absent_when_not_newer() {
    let home = tempfile::tempdir().unwrap();
    write_update_snapshot(home.path(), env!("CARGO_PKG_VERSION"));
    let out = run_statusline(SAMPLE, "200", home.path());
    assert!(!String::from_utf8_lossy(&out.stdout).contains('\u{2B06}'));
}

#[test]
fn narrow_width_drops_the_update_chip_first() {
    let home = tempfile::tempdir().unwrap();
    write_update_snapshot(home.path(), "99.0.0");
    // At 50 columns dropping the 8-cell update chip alone makes the line
    // fit, so every other chip must survive.
    let out = run_statusline(SAMPLE, "50", home.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains('\u{2B06}'), "stdout: {stdout}");
    assert!(stdout.contains("cache:46%"), "stdout: {stdout}");
    assert!(stdout.contains("Sonnet 5"), "stdout: {stdout}");
}

#[test]
fn install_with_update_check_writes_the_interval() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let out = Command::new(env!("CARGO_BIN_EXE_claude-statusline"))
        .args(["--install", "--with-update-check"])
        .env("CLAUDE_STATUSLINE_SETTINGS_PATH", &path)
        .env("HOME", dir.path())
        .env("USERPROFILE", dir.path())
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(".claude").join("claude-statusline.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(config["update_check_interval_minutes"], 1440);
}

#[test]
fn setup_opting_into_update_check_writes_the_interval() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    // Answers: install yes, subagent no, usage limits no, update check yes.
    let out = run_setup("y\nn\nn\ny\n", &path);
    assert!(out.status.success());
    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(".claude").join("claude-statusline.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(config["update_check_interval_minutes"], 1440);
}

#[test]
fn setup_declining_update_check_leaves_the_config_absent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let out = run_setup("y\nn\nn\nn\n", &path);
    assert!(out.status.success());
    assert!(
        !dir.path()
            .join(".claude")
            .join("claude-statusline.json")
            .exists()
    );
}

/// Like run_statusline but with colors forced on: a detected TTL shows up
/// only in the cache_age chip's color.
fn run_statusline_colored(stdin_data: &str, home: &std::path::Path) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_claude-statusline"))
        .env("FORCE_COLOR", "1")
        .env_remove("NO_COLOR")
        .env("CLAUDE_STATUSLINE_WIDTH", "200")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary runs");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_data.as_bytes())
        .unwrap();
    child.wait_with_output().expect("binary exits")
}

/// One assistant entry stamped ten minutes ago: inside the default 1h
/// window but past a 5m TTL, so the two cases color differently.
fn write_usage_transcript(home: &std::path::Path, usage: &str) -> String {
    let dir = home.join(".claude").join("projects").join("p");
    std::fs::create_dir_all(&dir).unwrap();
    let ts = (chrono::Utc::now() - chrono::Duration::minutes(10)).format("%Y-%m-%dT%H:%M:%SZ");
    let path = dir.join("s.jsonl");
    std::fs::write(
        &path,
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","message":{{"role":"assistant","usage":{usage}}}}}"#
        ),
    )
    .unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn cache_age_expires_early_under_a_detected_5m_ttl() {
    let home = tempfile::tempdir().unwrap();
    let path = write_usage_transcript(
        home.path(),
        r#"{"cache_creation":{"ephemeral_5m_input_tokens":700,"ephemeral_1h_input_tokens":0}}"#,
    );
    let payload = format!(r#"{{"transcript_path": {path:?}}}"#);
    let out = run_statusline_colored(&payload, home.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Ten minutes past a 5m write: expired, red.
    assert!(
        stdout.contains("\x1b[38;2;247;118;142m10m"),
        "stdout: {stdout:?}"
    );
}

#[test]
fn cache_age_keeps_the_1h_ceiling_for_1h_or_unknown_ttl() {
    for usage in [
        r#"{"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":900}}"#,
        // No breakdown at all: the default thresholds stay in force.
        r#"{"input_tokens":5}"#,
    ] {
        let home = tempfile::tempdir().unwrap();
        let path = write_usage_transcript(home.path(), usage);
        let payload = format!(r#"{{"transcript_path": {path:?}}}"#);
        let out = run_statusline_colored(&payload, home.path());
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Ten minutes into a 1h window: amber, exactly as before.
        assert!(
            stdout.contains("\x1b[38;2;224;175;104m10m"),
            "usage {usage}: stdout: {stdout:?}"
        );
    }
}
