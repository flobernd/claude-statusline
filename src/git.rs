use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const GIT_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Default, Clone)]
pub struct GitInfo {
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub stash: u32,
    pub state: Option<GitState>,
    pub linked_worktree: bool,
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
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
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
                if start.elapsed() > GIT_TIMEOUT {
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

pub fn collect(dir: &Path) -> GitInfo {
    let mut info = GitInfo::default();
    let (head, sync, stash) = std::thread::scope(|s| {
        let head = s.spawn(|| {
            run_git(dir, &["rev-parse", "--abbrev-ref", "HEAD", "--git-dir", "--git-common-dir"])
        });
        let sync = s.spawn(|| run_git(dir, &["rev-list", "--count", "--left-right", "HEAD...@{u}"]));
        let stash = s.spawn(|| run_git(dir, &["rev-list", "--walk-reflogs", "--count", "refs/stash"]));
        (
            head.join().unwrap_or(None),
            sync.join().unwrap_or(None),
            stash.join().unwrap_or(None),
        )
    });

    let mut git_dir: Option<PathBuf> = None;
    if let Some(out) = head {
        let lines: Vec<&str> = out.lines().collect();
        if lines.len() >= 3 && !lines[0].is_empty() {
            info.branch = Some(lines[0].to_string());
            let gd = resolve(dir, lines[1]);
            let common = resolve(dir, lines[2]);
            info.linked_worktree = gd != common;
            info.repo_name_fallback = common
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned());
            git_dir = Some(gd);
        }
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
    if let Some(gd) = git_dir {
        info.state = detect_state(dir, &gd);
    }
    info
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
        git(dir.path(), &["clone", origin.to_str().unwrap(), clone.to_str().unwrap()]);
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
        git(&repo, &["worktree", "add", wt.to_str().unwrap(), "-b", "feat/x"]);
        let info = collect(&wt);
        assert!(info.linked_worktree);
        assert_eq!(info.branch.as_deref(), Some("feat/x"));
        assert_eq!(info.repo_name_fallback.as_deref(), Some("repo"));
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
}
