use crate::format::{fmt_duration, fmt_tokens};
use crate::git::GitInfo;
use crate::schema::Payload;
use crate::theme::{AMBER, BLUE, COMMENT, CYAN, GREEN, MAGENTA, RED, Style};

/// Display heuristic for an unknown TTL: past five minutes the prompt cache
/// is likely cold.
pub const CACHE_AGE_WARN_MS: i64 = 5 * 60 * 1000;
/// Fallback expiry when the transcript does not reveal the session's cache
/// TTL: one hour is the longest TTL on offer, so past it the prompt cache
/// has certainly expired.
pub const CACHE_AGE_EXPIRE_MS: i64 = 60 * 60 * 1000;
/// How far amber leads a known expiry. Proportionally tighter for the short
/// TTL, where a ten minute lead would cover the whole window.
const CACHE_AGE_WARN_5M_MS: i64 = 4 * 60 * 1000;
const CACHE_AGE_WARN_1H_MS: i64 = 50 * 60 * 1000;

/// Amber and red thresholds for the session's cache TTL. A known TTL puts
/// amber just ahead of the real expiry; without one there is no expiry to
/// lead, so the wide "likely cold" warning stands in for the uncertainty.
fn cache_age_thresholds(ttl_ms: Option<i64>) -> (i64, i64) {
    match ttl_ms {
        Some(ttl) if ttl == crate::transcript::TTL_5M_MS => (CACHE_AGE_WARN_5M_MS, ttl),
        Some(ttl) if ttl == crate::transcript::TTL_1H_MS => (CACHE_AGE_WARN_1H_MS, ttl),
        _ => (CACHE_AGE_WARN_MS, CACHE_AGE_EXPIRE_MS),
    }
}

pub struct Ctx<'a> {
    pub payload: &'a Payload,
    pub git: &'a GitInfo,
    pub cache_age_ms: Option<i64>,
    pub cache_ttl_ms: Option<i64>,
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
        let (warn, expire) = cache_age_thresholds(c.cache_ttl_ms);
        let color = if age >= expire {
            RED
        } else if age >= warn {
            AMBER
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

/// The update chip: glyph plus the newer version, linked to its release
/// notes. Amber so it stands out from the metrics without reading as an
/// error. The bare U+2B06 (no variation selector) stays single-width,
/// which the fitting logic relies on.
pub fn update_chip(version: &str, url: Option<&str>, s: &Style) -> (&'static str, String) {
    let text = s.paint(&format!("\u{2B06} {version}"), AMBER);
    (
        "update",
        match url {
            Some(u) => s.link(u, &text),
            None => text,
        },
    )
}

pub fn line2(c: &Ctx) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let s = c.style;
    let ws = c.payload.workspace.as_ref();
    let repo = ws.and_then(|w| w.repo.as_ref());
    let repo_name: Option<String> = repo
        .and_then(|r| r.name.clone())
        .or_else(|| c.git.repo_name_fallback.clone());

    // When the directory is gone git answers nothing, so the payload's
    // native worktree fields stand in for the branch identity.
    let missing_branch = if c.git.missing_dir {
        c.payload.worktree.as_ref().and_then(|wt| {
            wt.branch
                .as_deref()
                .filter(|b| !b.is_empty())
                .or_else(|| wt.name.as_deref().filter(|n| !n.is_empty()))
        })
    } else {
        None
    };

    // The branch chip already carries the repo name and location, so the
    // cwd chip only earns its space outside a git repo, where none renders.
    let cwd = ws
        .and_then(|w| w.current_dir.as_deref())
        .or(c.payload.cwd.as_deref())
        .or_else(|| ws.and_then(|w| w.project_dir.as_deref()))
        .filter(|p| !p.is_empty());
    if c.git.branch.is_none()
        && missing_branch.is_none()
        && let Some(path) = cwd
    {
        let color = if c.git.missing_dir { RED } else { CYAN };
        out.push((
            "cwd",
            s.paint(&format!("\u{2302} {}", display_path(path)), color),
        ));
    }

    let branch_text = if let Some(branch) = c.git.branch.as_deref() {
        let branch_color = if c.git.on_default_branch {
            GREEN
        } else {
            MAGENTA
        };
        Some(match repo_name.as_deref() {
            Some(r) => format!(
                "{}{}",
                s.paint(&format!("\u{2387} {r}"), CYAN),
                s.paint(&format!("/{branch}"), branch_color)
            ),
            None => s.paint(&format!("\u{2387} {branch}"), branch_color),
        })
    } else {
        // Entirely red: the location itself is dead, not just the branch.
        missing_branch.map(|branch| match repo_name.as_deref() {
            Some(r) => s.paint(&format!("\u{2387} {r}/{branch}"), RED),
            None => s.paint(&format!("\u{2387} {branch}"), RED),
        })
    };
    if let Some(text) = branch_text {
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
            parts.push(s.paint(&format!("~{}", c.git.files_changed), AMBER));
        }
        out.push(("git_files", parts.join(" ")));
    }

    if c.git.stash > 0 {
        out.push((
            "git_stash",
            s.paint(&format!("stash:{}", c.git.stash), AMBER),
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
            AMBER
        };
        out.push(("git_state", s.paint_bold(state.label(), color)));
    }

    if ws.is_some_and(|w| w.git_worktree_present()) || c.git.linked_worktree {
        out.push(("git_worktree", s.paint("gwt", AMBER)));
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
        out.push(("worktree", s.paint(&format!("wt:{branch}"), AMBER)));
    }

    out
}

pub fn line3(
    limits: &crate::usage::Limits,
    plan: Option<&str>,
    account: Option<&str>,
    model: Option<&str>,
    style: &Style,
    now_epoch_s: i64,
) -> Vec<(&'static str, String)> {
    let s = style;
    let mut out: Vec<(&'static str, String)> = Vec::new();
    if let Some(account) = account.map(str::trim).filter(|a| !a.is_empty()) {
        // An email can run long; 32 characters keeps a personal address whole and trims a
        // corporate one before it crowds the windows out.
        let text: String = account.chars().take(32).collect();
        push_visible(&mut out, "usage_account", s.paint(&text, MAGENTA));
    }
    if let Some(plan) = plan
        && let Some(first) = plan.chars().next()
    {
        // Plan names arrive lowercase (max, pro); render them title-cased.
        let text: String = first.to_uppercase().chain(plan.chars().skip(1)).collect();
        push_visible(&mut out, "usage_plan", s.paint(&text, MAGENTA));
    }
    if let Some(w) = &limits.session {
        out.push(("usage_session", window_chip(s, "5h", w, now_epoch_s)));
    }
    if let Some(w) = &limits.week {
        out.push(("usage_week", window_chip(s, "7d", w, now_epoch_s)));
    }
    if let Some(w) = &limits.fable {
        out.push(("usage_fable", window_chip(s, "fable", w, now_epoch_s)));
    }
    if let Some(spend) = &limits.spend
        && let Some(chip) = spend_chip(s, spend, now_epoch_s)
    {
        out.push(("usage_spend", chip));
    }
    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        // Shown whole: shortening the id would hide the alias suffix that tells a 1M session apart.
        push_visible(&mut out, "usage_model", s.paint(model, MAGENTA));
    }
    out
}

/// Painting strips control characters, so free text made of them would leave an empty chip
/// that still claims a separator and the line glyph.
fn push_visible(out: &mut Vec<(&'static str, String)>, name: &'static str, painted: String) {
    if crate::fit::visible_width(&painted) > 0 {
        out.push((name, painted));
    }
}

/// Prefix the line glyph to the first chip. The glyph marks the line, not a chip, so it must ride
/// on whichever chip survives the filter and the fit.
pub fn with_line_glyph(
    mut chips: Vec<(&'static str, String)>,
    s: &Style,
) -> Vec<(&'static str, String)> {
    if let Some(first) = chips.first_mut() {
        first.1 = format!("{}{}", s.paint("\u{2301} ", COMMENT), first.1);
    }
    chips
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
            &format!(" ({})", fmt_duration((at - now_epoch_s) as u64 * 1_000)),
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
        "pending" => Some(("rev", AMBER)),
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
        cache_ttl_ms: None,
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
    let chips = line3(
        &sample_limits(),
        Some("max"),
        Some("user@example.com"),
        None,
        style,
        USAGE_SAMPLE_NOW_S,
    );
    with_line_glyph(chips, style)
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
            cache_ttl_ms: None,
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
        assert!(text_of(&chips, "cache_age").contains("\x1b[38;2;224;175;104m")); // amber

        c.cache_age_ms = Some(CACHE_AGE_EXPIRE_MS);
        let chips = line1(&c);
        assert!(text_of(&chips, "cache_age").contains("\x1b[38;2;247;118;142m")); // red past 1h
    }

    #[test]
    fn cache_age_bands_lead_the_detected_ttl() {
        const COMMENT_SEQ: &str = "\x1b[38;2;86;95;137m";
        const AMBER_SEQ: &str = "\x1b[38;2;224;175;104m";
        const RED_SEQ: &str = "\x1b[38;2;247;118;142m";
        let mins = |m: i64| m * 60 * 1000;

        let colored = Style {
            colors: true,
            links: false,
        };
        let payload = parse_payload("{}").unwrap();
        let git = GitInfo::default();
        let mut c = ctx_of(&payload, &git);
        c.style = &colored;

        let cases: &[(Option<i64>, i64, &str)] = &[
            // A 5m TTL: amber leads the expiry by one minute.
            (Some(mins(5)), mins(4) - 1, COMMENT_SEQ),
            (Some(mins(5)), mins(4), AMBER_SEQ),
            (Some(mins(5)), mins(5) - 1, AMBER_SEQ),
            (Some(mins(5)), mins(5), RED_SEQ),
            // A 1h TTL: amber leads by ten minutes, so five minutes in the
            // cache still reads fresh.
            (Some(mins(60)), mins(5), COMMENT_SEQ),
            (Some(mins(60)), mins(50) - 1, COMMENT_SEQ),
            (Some(mins(60)), mins(50), AMBER_SEQ),
            (Some(mins(60)), mins(60) - 1, AMBER_SEQ),
            (Some(mins(60)), mins(60), RED_SEQ),
            // An unknown TTL keeps the wide warning from five minutes.
            (None, CACHE_AGE_WARN_MS - 1, COMMENT_SEQ),
            (None, CACHE_AGE_WARN_MS, AMBER_SEQ),
            (None, CACHE_AGE_EXPIRE_MS - 1, AMBER_SEQ),
            (None, CACHE_AGE_EXPIRE_MS, RED_SEQ),
        ];

        for (ttl, age, want) in cases {
            c.cache_ttl_ms = *ttl;
            c.cache_age_ms = Some(*age);
            let chips = line1(&c);
            let text = text_of(&chips, "cache_age");
            assert!(text.contains(want), "ttl {ttl:?} at {age}ms: {text:?}");
        }
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
    fn branch_color_tracks_the_default_branch_flag() {
        let colored = Style {
            colors: true,
            links: false,
        };
        let payload = parse_payload("{}").unwrap();
        for (branch, default, rgb) in [
            ("main", true, "158;206;106"),
            ("master", true, "158;206;106"),
            // A trunk named neither main nor master still reads as one.
            ("trunk", true, "158;206;106"),
            ("feat/x", false, "187;154;247"),
            // main is just a feature branch where it is not the trunk.
            ("main", false, "187;154;247"),
        ] {
            let git = GitInfo {
                on_default_branch: default,
                ..git_on(branch)
            };
            let mut c = ctx_of(&payload, &git);
            c.style = &colored;
            let chips = line2(&c);
            assert!(
                chips[0].1.contains(&format!("\x1b[38;2;{rgb}m")),
                "branch {branch} (default: {default})"
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
        let chips = line3(&full_limits(), None, None, None, &PLAIN, USAGE_NOW_S);
        assert_eq!(
            names(&chips),
            vec!["usage_session", "usage_week", "usage_fable", "usage_spend"]
        );
        assert_eq!(text_of(&chips, "usage_session"), "5h:42% (2h10m)");
        assert_eq!(text_of(&chips, "usage_week"), "7d:63% (3d)");
        assert_eq!(text_of(&chips, "usage_fable"), "fable:81% (5d)");
        assert_eq!(
            text_of(&chips, "usage_spend"),
            "spend:$1002/$1000 (100%) (8d)"
        );
    }

    #[test]
    fn line3_returns_bare_chips() {
        let limits = crate::usage::Limits {
            week: Some(window(63.0, None)),
            ..crate::usage::Limits::default()
        };
        let chips = line3(&limits, None, None, None, &PLAIN, USAGE_NOW_S);
        assert_eq!(names(&chips), vec!["usage_week"]);
        assert_eq!(chips[0].1, "7d:63%");
    }

    #[test]
    fn with_line_glyph_prefixes_the_first_chip_only() {
        let chips = with_line_glyph(
            vec![
                ("usage_plan", "Max".to_string()),
                ("usage_session", "5h:42%".to_string()),
            ],
            &PLAIN,
        );
        assert_eq!(chips[0].1, "\u{2301} Max");
        assert_eq!(chips[1].1, "5h:42%");

        let colored = Style {
            colors: true,
            links: false,
        };
        let chips = with_line_glyph(vec![("usage_session", "5h:42%".to_string())], &colored);
        assert_eq!(chips[0].1, "\x1b[38;2;86;95;137m\u{2301} \x1b[0m5h:42%");
    }

    #[test]
    fn with_line_glyph_leaves_an_empty_vector_alone() {
        assert!(with_line_glyph(Vec::new(), &PLAIN).is_empty());
    }

    #[test]
    fn plan_chip_renders_first_and_title_cased() {
        let limits = crate::usage::Limits {
            session: Some(window(42.0, None)),
            ..crate::usage::Limits::default()
        };
        let chips = line3(&limits, Some("max"), None, None, &PLAIN, USAGE_NOW_S);
        assert_eq!(names(&chips), vec!["usage_plan", "usage_session"]);
        assert_eq!(chips[0].1, "Max");
        assert_eq!(chips[1].1, "5h:42%");

        let colored = Style {
            colors: true,
            links: false,
        };
        let chips = line3(
            &limits,
            Some("enterprise"),
            None,
            None,
            &colored,
            USAGE_NOW_S,
        );
        assert!(
            chips[0]
                .1
                .contains("\x1b[38;2;187;154;247mEnterprise\x1b[0m")
        );
    }

    #[test]
    fn empty_limits_render_nothing() {
        assert!(
            line3(
                &crate::usage::Limits::default(),
                None,
                None,
                None,
                &PLAIN,
                USAGE_NOW_S
            )
            .is_empty()
        );
    }

    #[test]
    fn past_or_missing_resets_omit_the_countdown() {
        let limits = crate::usage::Limits {
            session: Some(window(42.0, Some(USAGE_NOW_S))),
            week: Some(window(63.0, Some(USAGE_NOW_S - 5))),
            fable: Some(window(81.0, None)),
            ..crate::usage::Limits::default()
        };
        let chips = line3(&limits, None, None, None, &PLAIN, USAGE_NOW_S);
        assert_eq!(text_of(&chips, "usage_session"), "5h:42%");
        assert_eq!(text_of(&chips, "usage_week"), "7d:63%");
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
        let chips = line3(&limits, None, None, None, &PLAIN, USAGE_NOW_S);
        assert_eq!(chips[0].1, "spend:37%");
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
        assert!(line3(&limits, None, None, None, &PLAIN, USAGE_NOW_S).is_empty());
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
        let chips = line3(&limits, None, None, None, &colored, USAGE_NOW_S);
        assert_eq!(
            text_of(&chips, "usage_session"),
            "\x1b[38;2;86;95;137m5h:\x1b[0m\
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
        let chips = line3(&limits, None, None, None, &colored, USAGE_NOW_S);
        assert!(text_of(&chips, "usage_session").ends_with("\x1b[38;2;86;95;137m (2h10m)\x1b[0m"));
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
        let chips = line3(&limits, None, None, None, &colored, USAGE_NOW_S);
        let text = text_of(&chips, "usage_spend");
        assert!(text.contains("\x1b[38;2;247;118;142m$1002\x1b[0m"));
        assert!(text.contains("\x1b[38;2;247;118;142m$1000\x1b[0m"));
        assert!(text.contains("\x1b[38;2;247;118;142m100%\x1b[0m"));
    }

    #[test]
    fn usage_preview_renders_the_sample_line() {
        let text = usage_preview(&PLAIN);
        assert!(text.starts_with("\u{2301} user@example.com \u{2502} Max \u{2502} 5h:42%"));
        assert!(text.contains(" \u{2502} "));
        assert!(text.contains("spend:$1002/$1000 (100%)"));
    }

    #[test]
    fn account_chip_renders_first() {
        let limits = crate::usage::Limits {
            session: Some(window(42.0, None)),
            ..crate::usage::Limits::default()
        };
        let chips = line3(
            &limits,
            Some("max"),
            Some("biz@example.com"),
            None,
            &PLAIN,
            USAGE_NOW_S,
        );
        assert_eq!(
            names(&chips),
            vec!["usage_account", "usage_plan", "usage_session"]
        );
        assert_eq!(chips[0].1, "biz@example.com");
        assert_eq!(chips[1].1, "Max");
    }

    #[test]
    fn account_chip_is_truncated_and_a_blank_one_is_skipped() {
        let limits = crate::usage::Limits {
            session: Some(window(1.0, None)),
            ..crate::usage::Limits::default()
        };
        let long = "abcdefghijklmnopqrstuvwxyz0123456789@example.com";
        let chips = line3(&limits, None, Some(long), None, &PLAIN, USAGE_NOW_S);
        assert_eq!(chips[0].1, &long[..32]);
        let chips = line3(&limits, None, Some("  "), None, &PLAIN, USAGE_NOW_S);
        assert_eq!(names(&chips), vec!["usage_session"]);
    }

    #[test]
    fn control_character_account_and_plan_yield_no_chip() {
        let limits = crate::usage::Limits {
            session: Some(window(1.0, None)),
            ..crate::usage::Limits::default()
        };
        let controls = "\u{1}".repeat(32);
        let chips = line3(
            &limits,
            Some(&controls),
            Some(&controls),
            None,
            &PLAIN,
            USAGE_NOW_S,
        );
        assert_eq!(names(&chips), vec!["usage_session"]);
    }

    #[test]
    fn model_chip_shows_the_text_verbatim_after_the_spend() {
        let chips = line3(
            &full_limits(),
            Some("Max 5x"),
            Some("git@example.com"),
            Some("claude-fable-5-1[1m]"),
            &PLAIN,
            USAGE_NOW_S,
        );
        assert_eq!(
            names(&chips),
            vec![
                "usage_account",
                "usage_plan",
                "usage_session",
                "usage_week",
                "usage_fable",
                "usage_spend",
                "usage_model"
            ]
        );
        assert_eq!(text_of(&chips, "usage_model"), "claude-fable-5-1[1m]");
        let named = line3(
            &full_limits(),
            None,
            None,
            Some("Claude Fable 5.1 (1M)"),
            &PLAIN,
            USAGE_NOW_S,
        );
        assert_eq!(text_of(&named, "usage_model"), "Claude Fable 5.1 (1M)");
        let codex = line3(
            &full_limits(),
            None,
            None,
            Some("gpt-5.4-codex"),
            &PLAIN,
            USAGE_NOW_S,
        );
        assert_eq!(text_of(&codex, "usage_model"), "gpt-5.4-codex");
    }

    #[test]
    fn model_chip_is_absent_without_a_model_or_with_a_blank_one() {
        let none = line3(&full_limits(), None, None, None, &PLAIN, USAGE_NOW_S);
        assert!(!names(&none).contains(&"usage_model"));
        let blank = line3(&full_limits(), None, None, Some("  "), &PLAIN, USAGE_NOW_S);
        assert!(!names(&blank).contains(&"usage_model"));
    }

    fn missing_git() -> GitInfo {
        GitInfo {
            missing_dir: true,
            ..GitInfo::default()
        }
    }

    #[test]
    fn missing_dir_renders_branch_chip_from_payload_worktree() {
        let payload = parse_payload(
            r#"{
        "workspace": {"current_dir": "/gone/wt", "repo": {"name": "myrepo"}},
        "worktree": {"name": "fix", "branch": "feat/x"}
    }"#,
        )
        .unwrap();
        let git = missing_git();
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(chips[0].0, "branch");
        assert_eq!(chips[0].1, "\u{2387} myrepo/feat/x");
        // The branch chip carries the location, so the cwd chip stays out.
        assert!(!names_of(&chips).contains(&"cwd"));
    }

    #[test]
    fn missing_dir_branch_chip_is_entirely_red() {
        let colored = Style {
            colors: true,
            links: false,
        };
        let payload = parse_payload(
            r#"{
        "workspace": {"current_dir": "/gone/wt", "repo": {"name": "myrepo"}},
        "worktree": {"branch": "feat/x"}
    }"#,
        )
        .unwrap();
        let git = missing_git();
        let mut c = ctx_of(&payload, &git);
        c.style = &colored;
        let chips = line2(&c);
        assert_eq!(
            chips[0].1,
            "\x1b[38;2;247;118;142m\u{2387} myrepo/feat/x\x1b[0m"
        );
    }

    #[test]
    fn missing_dir_branch_falls_back_to_worktree_name() {
        let payload = parse_payload(
            r#"{"workspace": {"current_dir": "/gone"}, "worktree": {"name": "fix"}}"#,
        )
        .unwrap();
        let git = missing_git();
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(chips[0].1, "\u{2387} fix");
    }

    #[test]
    fn missing_dir_branch_chip_keeps_the_repo_link() {
        let linked = Style {
            colors: true,
            links: true,
        };
        let payload = parse_payload(
            r#"{
        "workspace": {"current_dir": "/gone",
            "repo": {"host": "github.com", "owner": "u", "name": "myapp"}},
        "worktree": {"branch": "feat/x"}
    }"#,
        )
        .unwrap();
        let git = missing_git();
        let mut c = ctx_of(&payload, &git);
        c.style = &linked;
        let chips = line2(&c);
        assert!(
            chips[0]
                .1
                .contains("\x1b]8;;https://github.com/u/myapp\x1b\\"),
            "chip: {}",
            chips[0].1
        );
    }

    #[test]
    fn missing_dir_without_payload_worktree_paints_the_cwd_chip_red() {
        let colored = Style {
            colors: true,
            links: false,
        };
        let payload = parse_payload(r#"{"workspace": {"current_dir": "/gone/dir"}}"#).unwrap();
        let git = missing_git();
        let mut c = ctx_of(&payload, &git);
        c.style = &colored;
        let chips = line2(&c);
        assert_eq!(chips[0].0, "cwd");
        assert_eq!(
            chips[0].1,
            "\x1b[38;2;247;118;142m\u{2302} /gone/dir\x1b[0m"
        );
    }

    #[test]
    fn missing_dir_empty_worktree_fields_fall_back_gracefully() {
        // An empty branch is no identity; the name still is.
        let payload = parse_payload(
            r#"{"workspace": {"current_dir": "/gone"}, "worktree": {"branch": "", "name": "fix"}}"#,
        )
        .unwrap();
        let git = missing_git();
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(chips[0].1, "\u{2387} fix");

        // Both empty: no identity at all, so the red cwd chip renders.
        let payload = parse_payload(
            r#"{"workspace": {"current_dir": "/gone"}, "worktree": {"branch": "", "name": ""}}"#,
        )
        .unwrap();
        let chips = line2(&ctx_of(&payload, &git));
        assert_eq!(chips[0].0, "cwd");
    }
}
