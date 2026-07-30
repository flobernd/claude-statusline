use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A render must not stall on a hung repository, so every git call gets
/// this budget. Raisable through CLAUDE_STATUSLINE_GIT_TIMEOUT_MS for
/// machines where process spawning alone can approach it, such as a busy
/// CI runner, where the budget would otherwise fail healthy repositories.
const GIT_TIMEOUT_DEFAULT: Duration = Duration::from_millis(500);
const GIT_TIMEOUT_VAR: &str = "CLAUDE_STATUSLINE_GIT_TIMEOUT_MS";

/// Anything unparseable, zero, or negative keeps the default: a broken
/// value must not silently disable the budget.
fn timeout_from(raw: Option<String>) -> Duration {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(GIT_TIMEOUT_DEFAULT, Duration::from_millis)
}

fn git_timeout() -> Duration {
    static TIMEOUT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *TIMEOUT.get_or_init(|| timeout_from(std::env::var(GIT_TIMEOUT_VAR).ok()))
}

#[derive(Debug, Default, Clone)]
pub struct GitInfo {
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub stash: u32,
    pub files_added: u32,
    pub files_removed: u32,
    pub files_changed: u32,
    pub state: Option<GitState>,
    pub linked_worktree: bool,
    pub on_default_branch: bool,
    pub repo_name_fallback: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitState {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Conflict,
}

impl GitState {
    pub fn label(self) -> &'static str {
        match self {
            GitState::Merge => "merge",
            GitState::Rebase => "rebase",
            GitState::CherryPick => "cherry-pick",
            GitState::Revert => "revert",
            GitState::Conflict => "conflict",
        }
    }
}

/// Run git with a hard timeout so a hung repository (network FS, huge
/// object store) can never stall the statusline render.
fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let timeout = git_timeout();
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        // A statusline must never take even optional locks in the repo it
        // observes: that would contend with the user's own git commands.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Drain stdout concurrently: a child writing more than the OS pipe
    // buffer would otherwise block forever and burn the whole timeout.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        stdout.read_to_string(&mut out).ok().map(|_| out)
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok().flatten();
                if !status.success() {
                    return None;
                }
                return out;
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}

fn resolve(dir: &Path, p: &str) -> PathBuf {
    let pb = PathBuf::from(p);
    let joined = if pb.is_absolute() { pb } else { dir.join(pb) };
    joined.canonicalize().unwrap_or(joined)
}

struct HeadInfo {
    branch: String,
    git_dir: PathBuf,
    common_dir: PathBuf,
}

fn head_info(dir: &Path, out: Option<String>) -> Option<HeadInfo> {
    let out = out?;
    let lines: Vec<&str> = out.lines().collect();
    if lines.len() < 3 || lines[0].is_empty() {
        return None;
    }
    Some(HeadInfo {
        branch: lines[0].to_string(),
        git_dir: resolve(dir, lines[1]),
        common_dir: resolve(dir, lines[2]),
    })
}

const HEAD_ARGS: &[&str] = &[
    "rev-parse",
    "--abbrev-ref",
    "HEAD",
    "--git-dir",
    "--git-common-dir",
];

/// Trunk names to fall back on when no remote publishes a default branch:
/// a repo with no remote, one where `git remote set-head` was never run, or
/// a ref backend that does not expose the symref as a file.
const FALLBACK_DEFAULT_BRANCHES: &[&str] = &["main", "master"];

/// Remote names, read from the repository config.
///
/// The directory tree under refs/remotes cannot answer this on its own:
/// both remote names and branch names may contain slashes, so a path like
/// refs/remotes/a/b/HEAD is equally the HEAD of remote `a/b` and a branch
/// `b/HEAD` of remote `a`. Walking the tree would also visit every loose
/// remote-tracking ref, turning a per-remote lookup into a per-ref one on
/// a path that runs on every render.
fn remote_names(common_dir: &Path) -> Vec<String> {
    let Ok(config) = std::fs::read_to_string(common_dir.join("config")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for line in config.lines() {
        let Some(section) = line
            .trim()
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
        else {
            continue;
        };
        let Some(name) = section
            .trim()
            .strip_prefix("remote ")
            .and_then(|s| unquote(s.trim()))
        else {
            continue;
        };
        // A name is pasted into a path below, so keep traversal out of it.
        if name.is_empty() || name.split('/').any(|c| c == "..") || names.contains(&name) {
            continue;
        }
        names.push(name);
    }
    names
}

/// Git config subsection names are double quoted, with backslash escapes
/// for a literal quote or backslash.
fn unquote(raw: &str) -> Option<String> {
    let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' { chars.next()? } else { c });
    }
    Some(out)
}

/// Whether `branch` is a default branch of the repository. Every remote's
/// HEAD symref counts, so a single remote named something other than
/// `origin` still resolves, and a fork whose upstream trunk differs from
/// its own keeps both marked.
///
/// Ref packing never touches symrefs, so the pointers stay readable as
/// plain files and this costs no git subprocess.
fn on_default_branch(common_dir: &Path, branch: &str) -> bool {
    let remotes = common_dir.join("refs").join("remotes");
    let mut published = false;
    let mut matched = false;
    for remote in remote_names(common_dir) {
        let Ok(head) = std::fs::read_to_string(remotes.join(&remote).join("HEAD")) else {
            continue;
        };
        // Matching against the remote's own prefix keeps branch names
        // that contain slashes intact.
        let Some(name) = head
            .trim()
            .strip_prefix(&format!("ref: refs/remotes/{remote}/"))
        else {
            continue;
        };
        published = true;
        matched |= name == branch;
    }
    if published {
        matched
    } else {
        FALLBACK_DEFAULT_BRANCHES.contains(&branch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchLocation {
    pub repo: String,
    pub branch: String,
    pub on_default_branch: bool,
}

/// Repo name comes from the common dir's parent, not the checkout directory,
/// so a linked worktree reports the main repository rather than its own
/// checkout folder.
pub fn branch_location(dir: &Path) -> Option<BranchLocation> {
    let info = head_info(dir, run_git(dir, HEAD_ARGS))?;
    let repo = info
        .common_dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())?;
    Some(BranchLocation {
        on_default_branch: on_default_branch(&info.common_dir, &info.branch),
        repo,
        branch: info.branch,
    })
}

pub fn collect(dir: &Path) -> GitInfo {
    let mut info = GitInfo::default();
    let (head, sync, stash, status) = std::thread::scope(|s| {
        let head = s.spawn(|| run_git(dir, HEAD_ARGS));
        let sync =
            s.spawn(|| run_git(dir, &["rev-list", "--count", "--left-right", "HEAD...@{u}"]));
        let stash = s.spawn(|| {
            run_git(
                dir,
                &["rev-list", "--walk-reflogs", "--count", "refs/stash"],
            )
        });
        let status = s.spawn(|| run_git(dir, &["status", "--porcelain"]));
        (
            head.join().unwrap_or(None),
            sync.join().unwrap_or(None),
            stash.join().unwrap_or(None),
            status.join().unwrap_or(None),
        )
    });

    let mut git_dir: Option<PathBuf> = None;
    if let Some(h) = head_info(dir, head) {
        info.on_default_branch = on_default_branch(&h.common_dir, &h.branch);
        info.branch = Some(h.branch);
        info.linked_worktree = h.git_dir != h.common_dir;
        info.repo_name_fallback = h
            .common_dir
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        git_dir = Some(h.git_dir);
    }
    if let Some(out) = sync {
        let mut parts = out.split_whitespace();
        if let (Some(a), Some(b)) = (parts.next(), parts.next()) {
            info.ahead = a.parse().unwrap_or(0);
            info.behind = b.parse().unwrap_or(0);
        }
    }
    if let Some(out) = stash {
        info.stash = out.trim().parse().unwrap_or(0);
    }
    if let Some(out) = status {
        (info.files_added, info.files_removed, info.files_changed) = parse_status_counts(&out);
    }
    if let Some(gd) = git_dir {
        info.state = detect_state(dir, &gd);
    }
    info
}

/// Working-tree file counts from porcelain status lines. Each entry is
/// classified once by its two-letter XY code: new files (untracked or
/// staged adds) count as added, deletions as removed, everything else
/// (modified, renamed, type change, unmerged) as changed.
fn parse_status_counts(out: &str) -> (u32, u32, u32) {
    let (mut added, mut removed, mut changed) = (0, 0, 0);
    for line in out.lines() {
        let Some(code) = line.get(..2) else { continue };
        if code == "??" || code.contains('A') {
            added += 1;
        } else if code.contains('D') {
            removed += 1;
        } else {
            changed += 1;
        }
    }
    (added, removed, changed)
}

/// Operation state from git-dir markers; any operation with unmerged
/// paths reports as Conflict instead of the operation name.
fn detect_state(dir: &Path, git_dir: &Path) -> Option<GitState> {
    let op = if git_dir.join("MERGE_HEAD").is_file() {
        GitState::Merge
    } else if git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir() {
        GitState::Rebase
    } else if git_dir.join("CHERRY_PICK_HEAD").is_file() {
        GitState::CherryPick
    } else if git_dir.join("REVERT_HEAD").is_file() {
        GitState::Revert
    } else {
        return None;
    };
    match run_git(dir, &["diff", "--name-only", "--diff-filter=U"]) {
        Some(out) if !out.trim().is_empty() => Some(GitState::Conflict),
        _ => Some(op),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: run git configured for hermetic operation (no user or
    /// system config, fixed identity).
    pub(crate) fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    pub(crate) fn init_repo(dir: &Path) {
        git(dir, &["init", "-b", "main"]);
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        git(dir, &["add", "f.txt"]);
        git(dir, &["commit", "-m", "init"]);
    }

    #[test]
    fn git_timeout_override_ignores_unusable_values() {
        assert_eq!(
            timeout_from(Some(" 10000 ".into())),
            Duration::from_secs(10)
        );
        for bad in [None, Some("".into()), Some("0".into()), Some("-1".into())] {
            assert_eq!(timeout_from(bad), GIT_TIMEOUT_DEFAULT);
        }
    }

    #[test]
    fn non_repo_dir_yields_default() {
        let dir = tempfile::tempdir().unwrap();
        let info = collect(dir.path());
        assert!(info.branch.is_none());
        assert_eq!(info.stash, 0);
        assert!(!info.linked_worktree);
    }

    #[test]
    fn branch_and_repo_fallback_name() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("myrepo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        let info = collect(&repo);
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert_eq!(info.repo_name_fallback.as_deref(), Some("myrepo"));
        assert!(!info.linked_worktree);
        assert_eq!(info.state, None);
    }

    #[test]
    fn stash_count() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("f.txt"), "two\n").unwrap();
        git(dir.path(), &["stash", "push", "-m", "wip"]);
        let info = collect(dir.path());
        assert_eq!(info.stash, 1);
    }

    #[test]
    fn ahead_behind_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        init_repo(&origin);
        let clone = dir.path().join("clone");
        git(
            dir.path(),
            &["clone", origin.to_str().unwrap(), clone.to_str().unwrap()],
        );
        std::fs::write(clone.join("g.txt"), "x\n").unwrap();
        git(&clone, &["add", "g.txt"]);
        git(&clone, &["commit", "-m", "local work"]);
        let info = collect(&clone);
        assert_eq!(info.ahead, 1);
        assert_eq!(info.behind, 0);
    }

    #[test]
    fn linked_worktree_detected() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        let wt = dir.path().join("wt");
        git(
            &repo,
            &["worktree", "add", wt.to_str().unwrap(), "-b", "feat/x"],
        );
        let info = collect(&wt);
        assert!(info.linked_worktree);
        assert_eq!(info.branch.as_deref(), Some("feat/x"));
        assert_eq!(info.repo_name_fallback.as_deref(), Some("repo"));
    }

    #[test]
    fn status_counts_classification() {
        let out = "?? new.txt\nA  staged.txt\n M mod.rs\nD  gone.rs\nR  a -> b\nMM both.rs\n";
        assert_eq!(parse_status_counts(out), (2, 1, 3));
        assert_eq!(parse_status_counts(""), (0, 0, 0));
    }

    #[test]
    fn dirty_worktree_file_counts() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("new.txt"), "n\n").unwrap();
        std::fs::write(dir.path().join("f.txt"), "changed\n").unwrap();
        let info = collect(dir.path());
        assert_eq!(info.files_added, 1);
        assert_eq!(info.files_changed, 1);
        assert_eq!(info.files_removed, 0);
    }

    #[test]
    fn merge_conflict_reports_conflict() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        git(dir.path(), &["checkout", "-b", "other"]);
        std::fs::write(dir.path().join("f.txt"), "theirs\n").unwrap();
        git(dir.path(), &["commit", "-am", "theirs"]);
        git(dir.path(), &["checkout", "main"]);
        std::fs::write(dir.path().join("f.txt"), "ours\n").unwrap();
        git(dir.path(), &["commit", "-am", "ours"]);
        // The merge fails with conflicts; run without asserting success.
        let _ = Command::new("git")
            .args(["merge", "other"])
            .current_dir(dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let info = collect(dir.path());
        assert_eq!(info.state, Some(GitState::Conflict));
    }

    #[test]
    fn merge_marker_without_conflicts_reports_merge() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let out = run_git(dir.path(), &["rev-parse", "--git-dir"]).unwrap();
        let git_dir = resolve(dir.path(), out.trim());
        std::fs::write(git_dir.join("MERGE_HEAD"), "0000\n").unwrap();
        let info = collect(dir.path());
        assert_eq!(info.state, Some(GitState::Merge));
    }

    #[test]
    fn rebase_marker_reports_rebase() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let out = run_git(dir.path(), &["rev-parse", "--git-dir"]).unwrap();
        let git_dir = resolve(dir.path(), out.trim());
        std::fs::create_dir(git_dir.join("rebase-merge")).unwrap();
        let info = collect(dir.path());
        assert_eq!(info.state, Some(GitState::Rebase));
    }

    #[test]
    fn branch_location_in_repo_and_non_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("myrepo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        assert_eq!(
            branch_location(&repo),
            Some(BranchLocation {
                repo: "myrepo".to_string(),
                branch: "main".to_string(),
                on_default_branch: true,
            })
        );
        let plain = dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert_eq!(branch_location(&plain), None);
    }

    #[test]
    fn branch_location_in_linked_worktree_names_main_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        let wt = dir.path().join("wt");
        git(
            &repo,
            &["worktree", "add", wt.to_str().unwrap(), "-b", "feat/x"],
        );
        assert_eq!(
            branch_location(&wt),
            Some(BranchLocation {
                repo: "repo".to_string(),
                branch: "feat/x".to_string(),
                on_default_branch: false,
            })
        );
    }

    /// Clone `origin` into `dir`/work with `origin`'s default branch named
    /// `default_branch`, which the clone records in refs/remotes/origin/HEAD.
    fn clone_with_default(dir: &Path, default_branch: &str) -> PathBuf {
        let origin = dir.join("origin");
        std::fs::create_dir(&origin).unwrap();
        git(&origin, &["init", "-b", default_branch]);
        std::fs::write(origin.join("f.txt"), "one\n").unwrap();
        git(&origin, &["add", "f.txt"]);
        git(&origin, &["commit", "-m", "init"]);
        let work = dir.join("work");
        git(
            dir,
            &["clone", origin.to_str().unwrap(), work.to_str().unwrap()],
        );
        work
    }

    /// Trunk resolution is a filesystem read over the common dir, so these
    /// tests ask it directly rather than through `collect`, which spawns
    /// four git processes per call and would only re-test the wiring that
    /// `remote_head_read_from_the_common_dir_of_a_worktree` already covers.
    fn common_dir(repo: &Path) -> PathBuf {
        repo.join(".git")
    }

    #[test]
    fn remote_head_marks_a_trunk_named_neither_main_nor_master() {
        let dir = tempfile::tempdir().unwrap();
        let work = common_dir(&clone_with_default(dir.path(), "trunk"));
        assert!(on_default_branch(&work, "trunk"));
        // main is not this repo's trunk, so it reads as a feature branch.
        assert!(!on_default_branch(&work, "main"));
        assert!(!on_default_branch(&work, "master"));
    }

    #[test]
    fn remote_head_read_from_the_common_dir_of_a_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let work = clone_with_default(dir.path(), "trunk");
        // Free up trunk so a linked worktree can check it out.
        git(&work, &["switch", "-c", "feat/x"]);
        let wt = dir.path().join("wt");
        git(&work, &["worktree", "add", wt.to_str().unwrap(), "trunk"]);

        // The symref lives in the common dir, which the worktree shares.
        let info = collect(&wt);
        assert!(info.linked_worktree);
        assert!(info.on_default_branch);
        assert!(!collect(&work).on_default_branch);
    }

    #[test]
    fn every_remote_head_counts_as_a_default() {
        let dir = tempfile::tempdir().unwrap();
        let work = clone_with_default(dir.path(), "main");
        let upstream = dir.path().join("upstream");
        std::fs::create_dir(&upstream).unwrap();
        init_repo(&upstream);
        git(&upstream, &["branch", "-m", "master"]);
        git(
            &work,
            &["remote", "add", "upstream", upstream.to_str().unwrap()],
        );
        git(&work, &["fetch", "upstream"]);
        git(&work, &["remote", "set-head", "upstream", "-a"]);

        // origin publishes main, upstream publishes master: both are trunks.
        let work = common_dir(&work);
        assert!(on_default_branch(&work, "main"));
        assert!(on_default_branch(&work, "master"));
        assert!(!on_default_branch(&work, "feat/x"));
    }

    #[test]
    fn no_remote_head_falls_back_to_main_and_master() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let repo = common_dir(dir.path());
        assert!(on_default_branch(&repo, "main"));
        assert!(on_default_branch(&repo, "master"));
        assert!(!on_default_branch(&repo, "trunk"));
    }

    #[test]
    fn remote_without_a_head_symref_keeps_the_fallback() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        // `remote add` alone writes no refs/remotes/<name>/HEAD.
        git(dir.path(), &["remote", "add", "origin", "/nonexistent"]);
        let repo = common_dir(dir.path());
        assert!(on_default_branch(&repo, "main"));
        assert!(!on_default_branch(&repo, "trunk"));
    }

    #[test]
    fn remote_name_containing_a_slash_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let work = clone_with_default(dir.path(), "main");
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        git(&nested, &["init", "-b", "trunk"]);
        std::fs::write(nested.join("f.txt"), "one\n").unwrap();
        git(&nested, &["add", "f.txt"]);
        git(&nested, &["commit", "-m", "init"]);
        git(
            &work,
            &["remote", "add", "grp/sub", nested.to_str().unwrap()],
        );
        git(&work, &["fetch", "grp/sub"]);
        git(&work, &["remote", "set-head", "grp/sub", "-a"]);

        // refs/remotes/grp/sub/HEAD names remote "grp/sub", not a branch
        // "sub/HEAD" of a remote "grp".
        let work = common_dir(&work);
        assert!(on_default_branch(&work, "trunk"));
        assert!(on_default_branch(&work, "main"), "origin publishes main");
        assert!(!on_default_branch(&work, "sub/HEAD"));
        assert!(!on_default_branch(&work, "feat/x"));
    }

    #[test]
    fn remote_names_parses_quoted_subsections() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config"),
            concat!(
                "[core]\n\trepositoryformatversion = 0\n",
                "[remote \"origin\"]\n\turl = /a\n",
                "  [remote \"grp/sub\"]  \n\turl = /b\n",
                "[remote \"quo\\\"ted\"]\n\turl = /c\n",
                "[branch \"remote\"]\n\tremote = origin\n",
                "[remote \"origin\"]\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
            ),
        )
        .unwrap();
        assert_eq!(
            remote_names(dir.path()),
            vec![
                "origin".to_string(),
                "grp/sub".to_string(),
                "quo\"ted".to_string()
            ]
        );
    }

    #[test]
    fn remote_names_rejects_traversal_and_missing_config() {
        let dir = tempfile::tempdir().unwrap();
        assert!(remote_names(dir.path()).is_empty());
        std::fs::write(
            dir.path().join("config"),
            "[remote \"../../escape\"]\n\turl = /a\n[remote \"..\"]\n\turl = /b\n",
        )
        .unwrap();
        assert!(remote_names(dir.path()).is_empty());
    }

    #[test]
    fn remote_head_that_is_not_a_symref_keeps_the_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let work = clone_with_default(dir.path(), "trunk");
        let sha = run_git(&work, &["rev-parse", "HEAD"]).unwrap();
        // A HEAD holding a raw object id names no branch, so it publishes
        // no default rather than being read as one.
        git(
            &work,
            &[
                "update-ref",
                "--no-deref",
                "refs/remotes/origin/HEAD",
                sha.trim(),
            ],
        );
        let work = common_dir(&work);
        assert!(!on_default_branch(&work, "trunk"));
        assert!(on_default_branch(&work, "main"));
    }
}
