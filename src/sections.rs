use crate::format::{fmt_duration, fmt_tokens};
use crate::git::GitInfo;
use crate::schema::Payload;
use crate::theme::{BLUE, COMMENT, CYAN, GREEN, MAGENTA, RED, Style, YELLOW};

/// Display heuristic: past five minutes the prompt cache is likely cold.
pub const CACHE_AGE_WARN_MS: i64 = 5 * 60 * 1000;

pub struct Ctx<'a> {
    pub payload: &'a Payload,
    pub git: &'a GitInfo,
    pub cache_age_ms: Option<i64>,
    pub style: &'a Style,
}

pub fn line1(c: &Ctx) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let s = c.style;
    let cw = c.payload.context_window.as_ref();
    let pct = cw.and_then(|w| w.used_percentage);
    let window = cw.and_then(|w| w.context_window_size).filter(|w| *w > 0.0);
    let usage = cw.and_then(|w| w.current_usage.as_ref());

    if let (Some(p), Some(win)) = (pct, window) {
        let p = p.clamp(0.0, 100.0);
        // Derived from the percentage, not the raw token fields, which are
        // only the uncached tail and never the documented context fill.
        let used = (win * p / 100.0).round() as u64;
        out.push((
            "context_tokens",
            format!(
                "{}{}{}{}{}{}{}",
                s.paint("\u{2630} ", COMMENT),
                s.paint(&fmt_tokens(used), BLUE),
                s.paint("/", COMMENT),
                s.paint(&fmt_tokens(win as u64), BLUE),
                s.paint(" (", COMMENT),
                s.paint(&format!("{}%", p.round() as u64), crate::bar::bar_color(p)),
                s.paint(")", COMMENT),
            ),
        ));
    }

    if let Some(u) = usage {
        let read = u.cache_read_input_tokens.unwrap_or(0.0).max(0.0);
        let denom = read
            + u.input_tokens.unwrap_or(0.0).max(0.0)
            + u.cache_creation_input_tokens.unwrap_or(0.0).max(0.0);
        if read > 0.0 && denom > 0.0 {
            let ratio = (read / denom * 100.0) as u64;
            if ratio > 0 {
                out.push((
                    "cache",
                    format!(
                        "{}{}",
                        s.paint("cache:", COMMENT),
                        s.paint(&format!("{ratio}%"), GREEN)
                    ),
                ));
            }
        }
    }

    if let Some(age) = c.cache_age_ms.filter(|a| *a >= 0) {
        let color = if age >= CACHE_AGE_WARN_MS {
            YELLOW
        } else {
            COMMENT
        };
        out.push((
            "cache_age",
            format!(
                "{}{}",
                s.paint("cache_age:", COMMENT),
                s.paint(&fmt_duration(age as u64), color)
            ),
        ));
    }

    if let Some(name) = c
        .payload
        .model
        .as_ref()
        .and_then(|m| m.display_name.as_deref())
    {
        out.push(("model", s.paint(name, MAGENTA)));
    }

    if let Some(level) = c.payload.effort.as_ref().and_then(|e| e.level.as_deref()) {
        let color = match level {
            "high" | "xhigh" | "max" => Some(MAGENTA),
            "medium" | "low" => Some(COMMENT),
            _ => None, // outside the documented enum: hide
        };
        if let Some(color) = color {
            out.push(("effort", s.paint_bold(&format!("effort:{level}"), color)));
        }
    }

    out
}

pub fn line2(c: &Ctx) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let s = c.style;
    let ws = c.payload.workspace.as_ref();
    let repo = ws.and_then(|w| w.repo.as_ref());
    let repo_name: Option<String> = repo
        .and_then(|r| r.name.clone())
        .or_else(|| c.git.repo_name_fallback.clone());

    let cwd = ws
        .and_then(|w| w.current_dir.as_deref())
        .or(c.payload.cwd.as_deref())
        .or_else(|| ws.and_then(|w| w.project_dir.as_deref()))
        .filter(|p| !p.is_empty());
    if let Some(path) = cwd {
        out.push((
            "cwd",
            s.paint(&format!("\u{2302} {}", display_path(path)), CYAN),
        ));
    }

    if let Some(branch) = c.git.branch.as_deref() {
        let branch_color = if branch == "main" || branch == "master" {
            GREEN
        } else {
            MAGENTA
        };
        let text = match repo_name.as_deref() {
            Some(r) => format!(
                "{}{}",
                s.paint(&format!("\u{2387} {r}"), CYAN),
                s.paint(&format!("/{branch}"), branch_color)
            ),
            None => s.paint(&format!("\u{2387} {branch}"), branch_color),
        };
        let url = repo.and_then(|r| match (&r.host, &r.owner, &r.name) {
            (Some(h), Some(o), Some(n)) => Some(format!("https://{h}/{o}/{n}")),
            _ => None,
        });
        out.push((
            "branch",
            match url {
                Some(u) => s.link(&u, &text),
                None => text,
            },
        ));
    }

    if c.git.files_added > 0 || c.git.files_removed > 0 || c.git.files_changed > 0 {
        let mut parts = Vec::new();
        if c.git.files_added > 0 {
            parts.push(s.paint(&format!("+{}", c.git.files_added), GREEN));
        }
        if c.git.files_removed > 0 {
            parts.push(s.paint(&format!("-{}", c.git.files_removed), RED));
        }
        if c.git.files_changed > 0 {
            parts.push(s.paint(&format!("~{}", c.git.files_changed), YELLOW));
        }
        out.push(("git_files", parts.join(" ")));
    }

    if c.git.stash > 0 {
        out.push((
            "git_stash",
            s.paint(&format!("stash:{}", c.git.stash), YELLOW),
        ));
    }

    if c.git.ahead > 0 || c.git.behind > 0 {
        let mut parts: Vec<String> = Vec::new();
        if c.git.ahead > 0 {
            parts.push(format!("+{}", c.git.ahead));
        }
        if c.git.behind > 0 {
            parts.push(format!("-{}", c.git.behind));
        }
        out.push((
            "git_sync",
            s.paint(&format!("sync:{}", parts.join("/")), COMMENT),
        ));
    }

    if let Some(state) = c.git.state {
        let color = if state == crate::git::GitState::Conflict {
            RED
        } else {
            YELLOW
        };
        out.push(("git_state", s.paint_bold(state.label(), color)));
    }

    if ws.is_some_and(|w| w.git_worktree_present()) || c.git.linked_worktree {
        out.push(("git_worktree", s.paint("gwt", YELLOW)));
    }

    if let Some(pr) = c.payload.pr.as_ref()
        && let Some(number) = pr.number
    {
        let mut text = s.paint(&format!("PR#{number}"), CYAN);
        if let Some(url) = pr.url.as_deref() {
            text = s.link(url, &text);
        }
        // The state token stays outside the link so the clickable
        // target is exactly "PR#N".
        if let Some((label, color)) = pr.review_state.as_deref().and_then(review_token) {
            text = format!("{text} {}", s.paint(label, color));
        }
        out.push(("pr", text));
    }

    if let Some(wt) = c.payload.worktree.as_ref()
        && let Some(branch) = wt.branch.as_deref().or(wt.name.as_deref())
    {
        out.push(("worktree", s.paint(&format!("wt:{branch}"), YELLOW)));
    }

    out
}

fn review_token(state: &str) -> Option<(&'static str, crate::theme::Rgb)> {
    match state {
        "approved" => Some(("ok", GREEN)),
        "changes_requested" => Some(("chg", RED)),
        "pending" => Some(("rev", YELLOW)),
        "draft" => Some(("draft", COMMENT)),
        _ => None,
    }
}

/// Home-prefixed paths render with the terminal-conventional tilde; the
/// prefix only collapses at a separator boundary so /home/userx is never
/// mistaken for /home/user.
fn display_path(path: &str) -> String {
    if let Some(home) = crate::schema::home_dir() {
        let home = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home.as_ref()) {
            if rest.is_empty() {
                return "~".to_string();
            }
            if rest.starts_with('/') || rest.starts_with('\\') {
                return format!("~{rest}");
            }
        }
    }
    path.to_string()
}

pub fn sample_payload() -> Payload {
    crate::schema::parse_payload(
        r#"{
        "cwd": "/home/user/projects/myapp",
        "model": {"display_name": "Sonnet 5"},
        "effort": {"level": "high"},
        "workspace": {
            "current_dir": "/home/user/projects/myapp",
            "project_dir": "/home/user/projects/myapp",
            "repo": {"host": "github.com", "owner": "user", "name": "myapp"}
        },
        "context_window": {
            "used_percentage": 42,
            "context_window_size": 1000000,
            "current_usage": {
                "input_tokens": 2, "output_tokens": 18500,
                "cache_creation_input_tokens": 12000,
                "cache_read_input_tokens": 407998
            }
        },
        "pr": {"number": 1234, "url": "https://github.com/user/myapp/pull/1234", "review_state": "approved"}
    }"#,
    )
    .expect("sample payload is valid")
}

pub fn sample_git() -> GitInfo {
    GitInfo {
        branch: Some("feat/statusline".to_string()),
        ahead: 2,
        behind: 1,
        stash: 2,
        files_added: 3,
        files_removed: 1,
        files_changed: 7,
        repo_name_fallback: Some("myapp".to_string()),
        ..GitInfo::default()
    }
}

pub fn preview(style: &Style) -> String {
    let payload = sample_payload();
    let git = sample_git();
    let ctx = Ctx {
        payload: &payload,
        git: &git,
        cache_age_ms: Some(72_000),
        style,
    };
    let sep = style.paint(" \u{2502} ", COMMENT);
    let join = |chips: Vec<(&'static str, String)>| {
        chips
            .into_iter()
            .map(|(_, r)| r)
            .collect::<Vec<_>>()
            .join(&sep)
    };
    format!("{}\n{}", join(line1(&ctx)), join(line2(&ctx)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parse_payload;

    pub(crate) const PLAIN: Style = Style {
        colors: false,
        links: false,
    };

    pub(crate) fn ctx_of<'a>(payload: &'a Payload, git: &'a GitInfo) -> Ctx<'a> {
        Ctx {
            payload,
            git,
            cache_age_ms: None,
            style: &PLAIN,
        }
    }

    fn names(chips: &[(&'static str, String)]) -> Vec<&'static str> {
        chips.iter().map(|(n, _)| *n).collect()
    }

    fn names_of(chips: &[(&'static str, String)]) -> Vec<&'static str> {
        chips.iter().map(|(n, _)| *n).collect()
    }

    fn text_of<'a>(chips: &'a [(&'static str, String)], name: &str) -> &'a str {
        &chips.iter().find(|(n, _)| *n == name).unwrap().1
    }

    #[test]
    fn full_line1_renders_all_chips_in_order() {
        let payload = parse_payload(
            r#"{
            "model": {"display_name": "Sonnet 5"},
            "effort": {"level": "xhigh"},
            "context_window": {
                "used_percentage": 42, "context_window_size": 1000000,
                "current_usage": {"input_tokens": 412000, "output_tokens": 18500,
                    "cache_creation_input_tokens": 12000, "cache_read_input_tokens": 365000}
            }
        }"#,
        )
        .unwrap();
        let git = GitInfo::default();
        let mut c = ctx_of(&payload, &git);
        c.cache_age_ms = Some(72_000);
        let chips = line1(&c);
        assert_eq!(
            names(&chips),
            vec!["context_tokens", "cache", "cache_age", "model", "effort"]
        );
        assert_eq!(text_of(&chips, "context_tokens"), "\u{2630} 420K/1M (42%)");
        assert_eq!(text_of(&chips, "cache"), "cache:46%"); // 365000 / 789000
        assert_eq!(text_of(&chips, "cache_age"), "cache_age:1m12s");
        assert_eq!(text_of(&chips, "model"), "Sonnet 5");
        assert_eq!(text_of(&chips, "effort"), "effort:xhigh");
    }

    #[test]
    fn context_tokens_paint_numbers_blue_and_labels_comment() {
        let colored = Style {
            colors: true,
            links: false,
        };
        let payload = parse_payload(
            r#"{"context_window": {"used_percentage": 42, "context_window_size": 1000000}}"#,
        )
        .unwrap();
        let git = GitInfo::default();
        let mut c = ctx_of(&payload, &git);
        c.style = &colored;
        let chips = line1(&c);
        assert_eq!(
            text_of(&chips, "context_tokens"),
            "\x1b[38;2;86;95;137m\u{2630} \x1b[0m\
             \x1b[38;2;122;162;247m420K\x1b[0m\
             \x1b[38;2;86;95;137m/\x1b[0m\
             \x1b[38;2;122;162;247m1M\x1b[0m\
             \x1b[38;2;86;95;137m (\x1b[0m\
             \x1b[38;2;158;206;106m42%\x1b[0m\
             \x1b[38;2;86;95;137m)\x1b[0m"
        );
    }

    #[test]
    fn empty_payload_renders_nothing() {
        let payload = parse_payload("{}").unwrap();
        let git = GitInfo::default();
        assert!(line1(&ctx_of(&payload, &git)).is_empty());
    }

    #[test]
    fn medium_and_low_effort_render_dim_but_visible() {
        for level in ["medium", "low"] {
            let payload =
                parse_payload(&format!(r#"{{"effort": {{"level": "{level}"}}}}"#)).unwrap();
            let git = GitInfo::default();
            let chips = line1(&ctx_of(&payload, &git));
            assert_eq!(text_of(&chips, "effort"), &format!("effort:{level}"));
        }
    }

    #[test]
    fn unknown_effort_level_hides_chip() {
        let payload = parse_payload(r#"{"effort": {"level": "ultrathink"}}"#).unwrap();
        let git = GitInfo::default();
        assert!(!names(&line1(&ctx_of(&payload, &git))).contains(&"effort"));
    }

    #[test]
    fn zero_cache_read_hides_cache_chip() {
        let payload = parse_payload(
            r#"{"context_window": {"current_usage": {"input_tokens": 5000, "output_tokens": 1, "cache_read_input_tokens": 0}}}"#,
        )
        .unwrap();
        let git = GitInfo::default();
        assert!(!names(&line1(&ctx_of(&payload, &git))).contains(&"cache"));
    }

    #[test]
    fn negative_cache_age_is_hidden_and_warn_color_kicks_in_at_5m() {
        let payload = parse_payload("{}").unwrap();
        let git = GitInfo::default();
        let mut c = ctx_of(&payload, &git);
        c.cache_age_ms = Some(-3_000);
        assert!(line1(&c).is_empty());

        let colored = Style {
            colors: true,
            links: false,
        };
        let mut c = ctx_of(&payload, &git);
        c.style = &colored;
        c.cache_age_ms = Some(CACHE_AGE_WARN_MS);
        let chips = line1(&c);
        assert!(text_of(&chips, "cache_age").contains("\x1b[38;2;224;175;104m")); // yellow
    }

    #[test]
    fn percentage_clamped_before_ctx_derivation() {
        let payload = parse_payload(
            r#"{"context_window": {"used_percentage": 400, "context_window_size": 1000000}}"#,
        )
        .unwrap();
        let git = GitInfo::default();
        let chips = line1(&ctx_of(&payload, &git));
        assert_eq!(text_of(&chips, "context_tokens"), "\u{2630} 1M/1M (100%)");
    }

    fn payload_with_repo() -> Payload {
        parse_payload(
            r#"{
            "workspace": {
                "project_dir": "/home/u/myapp",
                "repo": {"host": "github.com", "owner": "u", "name": "myapp"}
            }
        }"#,
        )
        .unwrap()
    }

    fn git_on(branch: &str) -> GitInfo {
        GitInfo {
            branch: Some(branch.to_string()),
            ..GitInfo::default()
        }
    }

    #[test]
    fn cwd_chip_always_opens_line2_with_full_path() {
        let payload = payload_with_repo();
        let git = git_on("main");
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(names_of(&chips), vec!["cwd", "branch"]);
        assert_eq!(chips[0].1, "\u{2302} /home/u/myapp");
        assert_eq!(chips[1].1, "\u{2387} myapp/main");
    }

    #[test]
    fn cwd_chip_prefers_current_dir_over_project_dir() {
        let payload = parse_payload(
            r#"{
            "workspace": {
                "current_dir": "/home/u/myapp/src",
                "project_dir": "/home/u/myapp",
                "repo": {"name": "myapp"}
            }
        }"#,
        )
        .unwrap();
        let git = git_on("fix/links");
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(names_of(&chips), vec!["cwd", "branch"]);
        assert_eq!(chips[0].1, "\u{2302} /home/u/myapp/src");
        assert_eq!(chips[1].1, "\u{2387} myapp/fix/links");
    }

    #[test]
    fn cwd_chip_shows_without_branch_and_abbreviates_home() {
        let payload = payload_with_repo();
        let git = GitInfo::default();
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(names_of(&chips), vec!["cwd"]);
        assert_eq!(chips[0].1, "\u{2302} /home/u/myapp");

        assert_eq!(display_path("/definitely/not/home"), "/definitely/not/home");
        if let Some(home) = crate::schema::home_dir() {
            let home = home.to_string_lossy().into_owned();
            assert_eq!(display_path(&home), "~");
            assert_eq!(display_path(&format!("{home}/sub/dir")), "~/sub/dir");
            assert_eq!(
                display_path(&format!("{home}x/other")),
                format!("{home}x/other")
            );
        }
    }

    #[test]
    fn branch_falls_back_to_git_repo_name() {
        let payload = parse_payload("{}").unwrap();
        let git = GitInfo {
            branch: Some("main".to_string()),
            repo_name_fallback: Some("localrepo".to_string()),
            ..GitInfo::default()
        };
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(chips[0].1, "\u{2387} localrepo/main");
    }

    #[test]
    fn feature_branch_uses_magenta_and_main_uses_green() {
        let colored = Style {
            colors: true,
            links: false,
        };
        let payload = parse_payload("{}").unwrap();
        for (branch, rgb) in [
            ("main", "158;206;106"),
            ("master", "158;206;106"),
            ("feat/x", "187;154;247"),
        ] {
            let git = git_on(branch);
            let mut c = ctx_of(&payload, &git);
            c.style = &colored;
            let chips = line2(&c);
            assert!(
                chips[0].1.contains(&format!("\x1b[38;2;{rgb}m")),
                "branch {branch}"
            );
        }
    }

    #[test]
    fn stash_sync_state_and_gwt_chips() {
        let payload = parse_payload("{}").unwrap();
        let git = GitInfo {
            branch: Some("main".to_string()),
            ahead: 2,
            behind: 1,
            stash: 3,
            state: Some(crate::git::GitState::Conflict),
            linked_worktree: true,
            ..GitInfo::default()
        };
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(
            names_of(&chips),
            vec![
                "branch",
                "git_stash",
                "git_sync",
                "git_state",
                "git_worktree"
            ]
        );
        assert_eq!(chips[1].1, "stash:3");
        assert_eq!(chips[2].1, "sync:+2/-1");
        assert_eq!(chips[3].1, "conflict");
        assert_eq!(chips[4].1, "gwt");
    }

    #[test]
    fn git_files_chip_after_branch_with_partial_counts() {
        let payload = parse_payload("{}").unwrap();
        let git = GitInfo {
            branch: Some("main".to_string()),
            files_added: 2,
            files_changed: 5,
            ..GitInfo::default()
        };
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(names_of(&chips), vec!["branch", "git_files"]);
        assert_eq!(chips[1].1, "+2 ~5");

        let git = GitInfo {
            files_removed: 4,
            ..GitInfo::default()
        };
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(chips[0].0, "git_files");
        assert_eq!(chips[0].1, "-4");
    }

    #[test]
    fn sync_omits_zero_side() {
        let payload = parse_payload("{}").unwrap();
        let git = GitInfo {
            branch: Some("main".to_string()),
            ahead: 2,
            ..GitInfo::default()
        };
        let chips = line2(&ctx_of(&payload, &git));
        assert!(chips.iter().any(|(_, r)| r == "sync:+2"));
        let git = GitInfo {
            branch: Some("main".to_string()),
            behind: 4,
            ..GitInfo::default()
        };
        let chips = line2(&ctx_of(&payload, &git));
        assert!(chips.iter().any(|(_, r)| r == "sync:-4"));
    }

    #[test]
    fn pr_chip_with_review_state_and_link() {
        let payload = parse_payload(
            r#"{"pr": {"number": 86, "url": "https://github.com/u/r/pull/86", "review_state": "approved"}}"#,
        )
        .unwrap();
        let git = GitInfo::default();
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(chips[0].1, "PR#86 ok");

        let linked = Style {
            colors: true,
            links: true,
        };
        let mut c = ctx_of(&payload, &git);
        c.style = &linked;
        let chips = line2(&c);
        assert!(
            chips[0]
                .1
                .contains("\x1b]8;;https://github.com/u/r/pull/86\x1b\\")
        );
        assert!(chips[0].1.ends_with("\x1b[0m")); // state token outside the link
    }

    #[test]
    fn worktree_chip_prefers_branch_over_name() {
        let payload =
            parse_payload(r#"{"worktree": {"name": "fix", "branch": "fix/bug-123"}}"#).unwrap();
        let git = GitInfo::default();
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(chips[0].1, "wt:fix/bug-123");
        let payload = parse_payload(r#"{"worktree": {"name": "fix"}}"#).unwrap();
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(chips[0].1, "wt:fix");
    }

    #[test]
    fn preview_contains_both_lines() {
        let text = preview(&PLAIN);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("420K/1M"));
        assert!(lines[1].contains("\u{2302} "));
        assert!(lines[1].contains("\u{2387} myapp/feat/statusline"));
        assert!(lines[1].contains("+3 -1 ~7"));
        assert!(lines[1].contains("PR#1234 ok"));
    }
}
