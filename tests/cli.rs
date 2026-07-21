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
fn run_statusline(stdin_data: &str, width: &str, home: &std::path::Path) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_claude-statusline"))
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .env("CLAUDE_STATUSLINE_WIDTH", width)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary runs");
    child.stdin.as_mut().unwrap().write_all(stdin_data.as_bytes()).unwrap();
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
