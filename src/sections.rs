use crate::format::{fmt_duration, fmt_tokens};
use crate::git::GitInfo;
use crate::schema::Payload;
use crate::theme::{BLUE, COMMENT, CYAN, GREEN, MAGENTA, RED, Style, YELLOW};

/// Display heuristic: past five minutes the prompt cache is likely cold.
pub const CACHE_AGE_WARN_MS: i64 = 5 * 60 * 1000;
/// The one-hour cache TTL is the hard ceiling: past it the prompt cache has
/// certainly expired, so the age is flagged more severely than "cold".
pub const CACHE_AGE_EXPIRE_MS: i64 = 60 * 60 * 1000;

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
                s.paint("\u{2338} ", COMMENT),
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
        let color = if age >= CACHE_AGE_EXPIRE_MS {
            RED
        } else if age >= CACHE_AGE_WARN_MS {
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
            out.push(("effort", s.paint_bold(level, color)));
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

    // The branch chip already carries the repo name and location, so the
    // cwd chip only earns its space outside a git repo, where none renders.
    let cwd = ws
        .and_then(|w| w.current_dir.as_deref())
        .or(c.payload.cwd.as_deref())
        .or_else(|| ws.and_then(|w| w.project_dir.as_deref()))
        .filter(|p| !p.is_empty());
    if c.git.branch.is_none()
        && let Some(path) = cwd
    {
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

pub fn line3(
    limits: &crate::usage::Limits,
    style: &Style,
    now_epoch_s: i64,
) -> Vec<(&'static str, String)> {
    let s = style;
    let mut out: Vec<(&'static str, String)> = Vec::new();
    if let Some(w) = &limits.session {
        out.push(("usage_session", window_chip(s, "session", w, now_epoch_s)));
    }
    if let Some(w) = &limits.week {
        out.push(("usage_week", window_chip(s, "week", w, now_epoch_s)));
    }
    if let Some(w) = &limits.fable {
        out.push(("usage_fable", window_chip(s, "fable", w, now_epoch_s)));
    }
    if let Some(spend) = &limits.spend
        && let Some(chip) = spend_chip(s, spend, now_epoch_s)
    {
        out.push(("usage_spend", chip));
    }
    // The glyph marks the line, not a specific chip, so it rides on
    // whichever chip happens to render first.
    if let Some(first) = out.first_mut() {
        first.1 = format!("{}{}", s.paint("\u{2301} ", COMMENT), first.1);
    }
    out
}

fn window_chip(s: &Style, label: &str, w: &crate::usage::Window, now_epoch_s: i64) -> String {
    format!(
        "{}{}{}",
        s.paint(&format!("{label}:"), COMMENT),
        s.paint(
            &format!("{}%", w.pct.round() as u64),
            crate::bar::bar_color(w.pct)
        ),
        countdown(s, w.resets_at, now_epoch_s),
    )
}

fn countdown(s: &Style, resets_at: Option<i64>, now_epoch_s: i64) -> String {
    match resets_at {
        // Strictly future only: a reset at or before now has nothing left
        // to count down.
        Some(at) if at > now_epoch_s => s.paint(
            &format!(" \u{b7}{}", fmt_duration((at - now_epoch_s) as u64 * 1_000)),
            COMMENT,
        ),
        _ => String::new(),
    }
}

/// A spend entry without a percentage (a zero limit with no reported
/// utilization) has no meaningful meter and no color, so no chip.
fn spend_chip(s: &Style, spend: &crate::usage::Spend, now_epoch_s: i64) -> Option<String> {
    let pct = spend.pct?;
    let color = crate::bar::bar_color(pct);
    let pct_text = format!("{}%", pct.round() as u64);
    let meter = match (spend.used_cents, spend.limit_cents) {
        (Some(used), Some(limit)) => format!(
            "{}{}{}{}{}{}",
            s.paint(&format!("${}", dollars(used)), color),
            s.paint("/", COMMENT),
            s.paint(&format!("${}", dollars(limit)), color),
            s.paint(" (", COMMENT),
            s.paint(&pct_text, color),
            s.paint(")", COMMENT),
        ),
        _ => s.paint(&pct_text, color),
    };
    Some(format!(
        "{}{}{}",
        s.paint("spend:", COMMENT),
        meter,
        countdown(s, spend.resets_at, now_epoch_s),
    ))
}

/// Endpoint amounts arrive in cents.
fn dollars(cents: f64) -> u64 {
    (cents / 100.0).round() as u64
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

/// Fixed now for the wizard preview so the sample countdowns are stable.
const USAGE_SAMPLE_NOW_S: i64 = 1_784_829_600;

fn sample_limits() -> crate::usage::Limits {
    crate::usage::Limits {
        session: Some(crate::usage::Window {
            pct: 42.0,
            resets_at: Some(USAGE_SAMPLE_NOW_S + 7_800),
        }),
        week: Some(crate::usage::Window {
            pct: 63.0,
            resets_at: Some(USAGE_SAMPLE_NOW_S + 259_200),
        }),
        fable: Some(crate::usage::Window {
            pct: 81.0,
            resets_at: Some(USAGE_SAMPLE_NOW_S + 432_000),
        }),
        spend: Some(crate::usage::Spend {
            used_cents: Some(100_200.0),
            limit_cents: Some(100_000.0),
            pct: Some(100.2),
            resets_at: Some(USAGE_SAMPLE_NOW_S + 691_200),
        }),
    }
}

/// One-line sample of the usage limits line for the wizard's opt-in
/// branch; preview() stays two lines because the main install preview
/// must not advertise a line that is off by default.
pub fn usage_preview(style: &Style) -> String {
    let sep = style.paint(" \u{2502} ", COMMENT);
    line3(&sample_limits(), style, USAGE_SAMPLE_NOW_S)
        .into_iter()
        .map(|(_, r)| r)
        .collect::<Vec<_>>()
        .join(&sep)
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
        assert_eq!(text_of(&chips, "context_tokens"), "\u{2338} 420K/1M (42%)");
        assert_eq!(text_of(&chips, "cache"), "cache:46%"); // 365000 / 789000
        assert_eq!(text_of(&chips, "cache_age"), "cache_age:1m12s");
        assert_eq!(text_of(&chips, "model"), "Sonnet 5");
        assert_eq!(text_of(&chips, "effort"), "xhigh");
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
            "\x1b[38;2;86;95;137m\u{2338} \x1b[0m\
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
            assert_eq!(text_of(&chips, "effort"), level);
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
        assert!(!names(&line1(&c)).contains(&"cache_age"));

        let colored = Style {
            colors: true,
            links: false,
        };
        let mut c = ctx_of(&payload, &git);
        c.style = &colored;
        c.cache_age_ms = Some(CACHE_AGE_WARN_MS);
        let chips = line1(&c);
        assert!(text_of(&chips, "cache_age").contains("\x1b[38;2;224;175;104m")); // yellow

        c.cache_age_ms = Some(CACHE_AGE_EXPIRE_MS);
        let chips = line1(&c);
        assert!(text_of(&chips, "cache_age").contains("\x1b[38;2;247;118;142m")); // red past 1h
    }

    #[test]
    fn percentage_clamped_before_ctx_derivation() {
        let payload = parse_payload(
            r#"{"context_window": {"used_percentage": 400, "context_window_size": 1000000}}"#,
        )
        .unwrap();
        let git = GitInfo::default();
        let chips = line1(&ctx_of(&payload, &git));
        assert_eq!(text_of(&chips, "context_tokens"), "\u{2338} 1M/1M (100%)");
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
    fn cwd_chip_suppressed_inside_git_repo() {
        let payload = payload_with_repo();
        let git = git_on("main");
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(names_of(&chips), vec!["branch"]);
        assert_eq!(chips[0].1, "\u{2387} myapp/main");
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
        let git = GitInfo::default();
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(names_of(&chips), vec!["cwd"]);
        assert_eq!(chips[0].1, "\u{2302} /home/u/myapp/src");
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
        // The sample sits on a branch, so the cwd chip is suppressed.
        assert!(!lines[1].contains("\u{2302} "));
        assert!(lines[1].contains("\u{2387} myapp/feat/statusline"));
        assert!(lines[1].contains("+3 -1 ~7"));
        assert!(lines[1].contains("PR#1234 ok"));
    }

    const USAGE_NOW_S: i64 = 1_784_829_600;

    fn window(pct: f64, resets_at: Option<i64>) -> crate::usage::Window {
        crate::usage::Window { pct, resets_at }
    }

    fn full_limits() -> crate::usage::Limits {
        crate::usage::Limits {
            session: Some(window(42.0, Some(USAGE_NOW_S + 7_800))),
            week: Some(window(63.0, Some(USAGE_NOW_S + 259_200))),
            fable: Some(window(81.0, Some(USAGE_NOW_S + 432_000))),
            spend: Some(crate::usage::Spend {
                used_cents: Some(100_200.0),
                limit_cents: Some(100_000.0),
                pct: Some(100.2),
                resets_at: Some(USAGE_NOW_S + 691_200),
            }),
        }
    }

    #[test]
    fn full_line3_renders_all_chips_in_order() {
        let chips = line3(&full_limits(), &PLAIN, USAGE_NOW_S);
        assert_eq!(
            names(&chips),
            vec!["usage_session", "usage_week", "usage_fable", "usage_spend"]
        );
        assert_eq!(
            text_of(&chips, "usage_session"),
            "\u{2301} session:42% \u{b7}2h10m"
        );
        assert_eq!(text_of(&chips, "usage_week"), "week:63% \u{b7}3d");
        assert_eq!(text_of(&chips, "usage_fable"), "fable:81% \u{b7}5d");
        assert_eq!(
            text_of(&chips, "usage_spend"),
            "spend:$1002/$1000 (100%) \u{b7}8d"
        );
    }

    #[test]
    fn line3_glyph_moves_to_the_first_present_chip() {
        let limits = crate::usage::Limits {
            week: Some(window(63.0, None)),
            ..crate::usage::Limits::default()
        };
        let chips = line3(&limits, &PLAIN, USAGE_NOW_S);
        assert_eq!(names(&chips), vec!["usage_week"]);
        assert_eq!(chips[0].1, "\u{2301} week:63%");
    }

    #[test]
    fn empty_limits_render_nothing() {
        assert!(line3(&crate::usage::Limits::default(), &PLAIN, USAGE_NOW_S).is_empty());
    }

    #[test]
    fn past_or_missing_resets_omit_the_countdown() {
        let limits = crate::usage::Limits {
            session: Some(window(42.0, Some(USAGE_NOW_S))),
            week: Some(window(63.0, Some(USAGE_NOW_S - 5))),
            fable: Some(window(81.0, None)),
            ..crate::usage::Limits::default()
        };
        let chips = line3(&limits, &PLAIN, USAGE_NOW_S);
        assert_eq!(text_of(&chips, "usage_session"), "\u{2301} session:42%");
        assert_eq!(text_of(&chips, "usage_week"), "week:63%");
        assert_eq!(text_of(&chips, "usage_fable"), "fable:81%");
    }

    #[test]
    fn spend_falls_back_to_percent_only() {
        let limits = crate::usage::Limits {
            spend: Some(crate::usage::Spend {
                used_cents: None,
                limit_cents: None,
                pct: Some(37.0),
                resets_at: None,
            }),
            ..crate::usage::Limits::default()
        };
        let chips = line3(&limits, &PLAIN, USAGE_NOW_S);
        assert_eq!(chips[0].1, "\u{2301} spend:37%");
    }

    #[test]
    fn spend_without_a_percentage_hides_the_chip() {
        let limits = crate::usage::Limits {
            spend: Some(crate::usage::Spend {
                used_cents: Some(100.0),
                limit_cents: Some(0.0),
                pct: None,
                resets_at: None,
            }),
            ..crate::usage::Limits::default()
        };
        assert!(line3(&limits, &PLAIN, USAGE_NOW_S).is_empty());
    }

    #[test]
    fn line3_percent_paints_via_bar_color_and_labels_dim() {
        let colored = Style {
            colors: true,
            links: false,
        };
        let limits = crate::usage::Limits {
            session: Some(window(42.0, None)),
            week: Some(window(90.0, None)),
            ..crate::usage::Limits::default()
        };
        let chips = line3(&limits, &colored, USAGE_NOW_S);
        assert_eq!(
            text_of(&chips, "usage_session"),
            "\x1b[38;2;86;95;137m\u{2301} \x1b[0m\
             \x1b[38;2;86;95;137msession:\x1b[0m\
             \x1b[38;2;158;206;106m42%\x1b[0m"
        );
        assert!(text_of(&chips, "usage_week").contains("\x1b[38;2;247;118;142m90%\x1b[0m"));
    }

    #[test]
    fn line3_countdown_paints_dim() {
        let colored = Style {
            colors: true,
            links: false,
        };
        let limits = crate::usage::Limits {
            session: Some(window(42.0, Some(USAGE_NOW_S + 7_800))),
            ..crate::usage::Limits::default()
        };
        let chips = line3(&limits, &colored, USAGE_NOW_S);
        assert!(
            text_of(&chips, "usage_session").ends_with("\x1b[38;2;86;95;137m \u{b7}2h10m\x1b[0m")
        );
    }

    #[test]
    fn spend_amounts_share_the_percent_color() {
        let colored = Style {
            colors: true,
            links: false,
        };
        let limits = crate::usage::Limits {
            spend: Some(crate::usage::Spend {
                used_cents: Some(100_200.0),
                limit_cents: Some(100_000.0),
                pct: Some(100.2),
                resets_at: None,
            }),
            ..crate::usage::Limits::default()
        };
        let chips = line3(&limits, &colored, USAGE_NOW_S);
        let text = text_of(&chips, "usage_spend");
        assert!(text.contains("\x1b[38;2;247;118;142m$1002\x1b[0m"));
        assert!(text.contains("\x1b[38;2;247;118;142m$1000\x1b[0m"));
        assert!(text.contains("\x1b[38;2;247;118;142m100%\x1b[0m"));
    }

    #[test]
    fn usage_preview_renders_the_sample_line() {
        let text = usage_preview(&PLAIN);
        assert!(text.starts_with("\u{2301} session:42%"));
        assert!(text.contains(" \u{2502} "));
        assert!(text.contains("spend:$1002/$1000 (100%)"));
    }
}
