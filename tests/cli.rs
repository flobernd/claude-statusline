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
    assert!(stdout.contains("0.1.0"));
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
    assert!(stdout.contains("ctx:420K/1M"), "stdout: {stdout}");
    assert!(stdout.contains("in:412K out:18K"));
    assert!(stdout.contains("cache:46%"));
    assert!(stdout.contains("Sonnet 5"));
    assert!(stdout.contains("effort:xhigh"));
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
fn narrow_width_drops_low_priority_chips_but_keeps_bar_and_tokens() {
    let home = tempfile::tempdir().unwrap();
    let out = run_statusline(SAMPLE, "45", home.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("in:412K out:18K"));
    assert!(stdout.contains('['));
    assert!(!stdout.contains("cache_age"));
    assert!(!stdout.contains("Sonnet 5"));
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
    assert!(!stdout.contains("effort:"));
    assert!(stdout.contains("in:412K out:18K"));
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
    // Folder name equals the repo name, so the project chip auto-hides.
    let line2 = stdout.lines().next().unwrap();
    assert!(!line2.trim_start().starts_with("myrepo \u{2502}"));
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

fn run_setup(answer: &str, path: &std::path::Path) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_claude-statusline"))
        .arg("--setup")
        .env("CLAUDE_STATUSLINE_SETTINGS_PATH", path)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
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
    assert!(stdout.contains("ctx:420K/1M"));
    assert!(stdout.contains("\u{2387} myapp/feat/statusline"));
    assert!(stdout.contains("Setup complete."));
    assert!(path.exists());
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
