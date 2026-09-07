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
    /// The directory itself is gone (a removed worktree or deleted
    /// checkout): git can say nothing, so the render flags the dead
    /// location instead of silently dropping the git chips.
    pub missing_dir: bool,
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

fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    run_command(Path::new("git"), dir, args, git_timeout())
}

/// Run a command with a hard timeout so a hung repository (network FS,
/// huge object store) can never stall the statusline render.
fn run_command(program: &Path, dir: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
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
                let deadline = start + timeout;
                while !reader.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if !reader.is_finished() {
                    return None;
                }
                let out = reader.join().ok().flatten();
                if !status.success() {
                    return None;
                }
                return out;
            }
            Ok(None) if start.elapsed() > timeout => {
                // The reader is not joined: a grandchild that inherited the
                // pipe keeps it open past the kill, and the render must
                // not wait on it. The thread ends with the process.
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
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
    common_dir: PathBuf,
}

fn head_info(dir: &Path, out: Option<String>) -> Option<HeadInfo> {
    let out = out?;
    let lines: Vec<&str> = out.lines().collect();
    if lines.len() < 2 || lines[0].is_empty() {
        return None;
    }
    Some(HeadInfo {
        branch: lines[0].to_string(),
        common_dir: resolve(dir, lines[1]),
    })
}

const HEAD_ARGS: &[&str] = &["rev-parse", "--abbrev-ref", "HEAD", "--git-common-dir"];

const DIR_ARGS: &[&str] = &["rev-parse", "--git-dir", "--git-common-dir"];

/// The two directories alone: unlike `HEAD_ARGS` this succeeds before the
/// first commit, when HEAD names a branch that has no object yet.
fn dir_info(dir: &Path, out: Option<String>) -> Option<(PathBuf, PathBuf)> {
    let out = out?;
    let mut lines = out.lines();
    let git_dir = resolve(dir, lines.next()?);
    let common_dir = resolve(dir, lines.next()?);
    Some((git_dir, common_dir))
}

/// The branch from the HEAD file, for when the status call ran out of its
/// budget on a slow tree: `ref: refs/heads/<name>` on a branch, unborn or
/// not, a raw object id when detached. The reftable backend keeps a
/// placeholder here and gets no fallback.
fn head_from_file(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(name) = head.strip_prefix("ref: refs/heads/") {
        return (!name.is_empty() && name != ".invalid").then(|| name.to_string());
    }
    (head.len() >= 40 && head.bytes().all(|b| b.is_ascii_hexdigit())).then(|| "HEAD".to_string())
}

/// Porcelain v2 with `--branch --untracked-files=all`: the branch header
/// resolves on an unborn HEAD, every untracked file has its own `?` line,
/// and every unmerged path has a `u` line whether or not an operation
/// marker exists, which `git stash pop` never writes.
const STATUS_ARGS: &[&str] = &[
    "status",
    "--porcelain=v2",
    "--branch",
    "--untracked-files=all",
];

#[derive(Debug, Default, PartialEq, Eq)]
struct StatusInfo {
    branch: Option<String>,
    added: u32,
    removed: u32,
    changed: u32,
    unmerged: bool,
}

/// Each entry is classified once by its XY code: untracked and staged adds
/// count as added, deletions as removed, everything else (modified,
/// renamed, type change, unmerged) as changed. `(detached)` reads as
/// `HEAD`, the name rev-parse gave it before.
fn parse_status(out: &str) -> StatusInfo {
    let mut s = StatusInfo::default();
    for line in out.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            s.branch = Some(if head == "(detached)" {
                "HEAD".to_string()
            } else {
                head.to_string()
            });
            continue;
        }
        let mut fields = line.split(' ');
        let (Some(kind), code) = (fields.next(), fields.next()) else {
            continue;
        };
        match kind {
            "?" => s.added += 1,
            "u" => {
                s.unmerged = true;
                s.changed += 1;
            }
            "1" | "2" => match code {
                Some(c) if c.contains('A') => s.added += 1,
                Some(c) if c.contains('D') => s.removed += 1,
                Some(_) => s.changed += 1,
                None => {}
            },
            _ => {}
        }
    }
    s
}

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

/// A submodule's git dir lives under the `modules` directory of the git
/// dir that owns its checkout: `.git/modules/<name>` from the main
/// checkout, `<common>/worktrees/<wt>/modules/<name>` from a linked
/// worktree or a checkout of a bare common repository. Nested once more
/// per level for a submodule of a submodule.
fn submodule_common_dir(common_dir: &Path) -> bool {
    let is_named = |p: &Path, s: &str| p.file_name().is_some_and(|n| n == s);
    common_dir.ancestors().any(|a| {
        is_named(a, "modules")
            && a.parent().is_some_and(|p| {
                is_named(p, ".git") || p.parent().is_some_and(|g| is_named(g, "worktrees"))
            })
    })
}

/// The common dir's parent names the repository: a linked worktree's
/// common dir is the main repository's `.git`, and a bare common
/// repository beside its checkouts is named by its own parent too. A
/// submodule's parent would say `modules`, so its checkout root names it,
/// looked up only then: `--show-toplevel` aborts the whole rev-parse in a
/// bare repository or inside a `.git` directory, layouts that must keep
/// their chips, so it cannot ride along with the directory lookup.
fn repo_name(dir: &Path, common_dir: &Path) -> Option<String> {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
    if submodule_common_dir(common_dir) {
        let out = run_git(dir, &["rev-parse", "--show-toplevel"])?;
        name(&resolve(dir, out.trim()))
    } else {
        common_dir.parent().and_then(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchLocation {
    pub repo: String,
    pub branch: String,
    pub on_default_branch: bool,
}

pub fn branch_location(dir: &Path) -> Option<BranchLocation> {
    let info = head_info(dir, run_git(dir, HEAD_ARGS))?;
    let repo = repo_name(dir, &info.common_dir)?;
    Some(BranchLocation {
        on_default_branch: on_default_branch(&info.common_dir, &info.branch),
        repo,
        branch: info.branch,
    })
}

pub fn collect(dir: &Path) -> GitInfo {
    // Ok(false) is the only certain "gone" answer; an Err (permissions,
    // unreachable mount) must not paint a healthy repo as deleted.
    if matches!(dir.try_exists(), Ok(false)) {
        return GitInfo {
            missing_dir: true,
            ..GitInfo::default()
        };
    }
    let mut info = GitInfo::default();
    let (dirs, sync, stash, status) = std::thread::scope(|s| {
        let dirs = s.spawn(|| run_git(dir, DIR_ARGS));
        let sync =
            s.spawn(|| run_git(dir, &["rev-list", "--count", "--left-right", "HEAD...@{u}"]));
        let stash = s.spawn(|| {
            run_git(
                dir,
                &["rev-list", "--walk-reflogs", "--count", "refs/stash"],
            )
        });
        let status = s.spawn(|| run_git(dir, STATUS_ARGS));
        (
            dirs.join().unwrap_or(None),
            sync.join().unwrap_or(None),
            stash.join().unwrap_or(None),
            status.join().unwrap_or(None),
        )
    });

    let status = status.map(|out| parse_status(&out));
    if let Some((git_dir, common_dir)) = dir_info(dir, dirs) {
        let branch = match &status {
            Some(s) => s.branch.clone(),
            // A status that ran out of budget must not cost the branch chip.
            None => head_from_file(&git_dir),
        };
        if let Some(branch) = branch {
            info.on_default_branch = on_default_branch(&common_dir, &branch);
            info.branch = Some(branch);
        }
        info.linked_worktree = git_dir != common_dir;
        info.repo_name_fallback = repo_name(dir, &common_dir);
        info.state = detect_state(&git_dir, status.as_ref().is_some_and(|s| s.unmerged));
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
    if let Some(s) = &status {
        (info.files_added, info.files_removed, info.files_changed) =
            (s.added, s.removed, s.changed);
    }
    info
}

/// Unmerged paths mean Conflict whatever operation left them; otherwise
/// the git-dir markers name the operation in progress.
fn detect_state(git_dir: &Path, unmerged: bool) -> Option<GitState> {
    if unmerged {
        return Some(GitState::Conflict);
    }
    if git_dir.join("MERGE_HEAD").is_file() {
        Some(GitState::Merge)
    } else if git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir() {
        Some(GitState::Rebase)
    } else if git_dir.join("CHERRY_PICK_HEAD").is_file() {
        Some(GitState::CherryPick)
    } else if git_dir.join("REVERT_HEAD").is_file() {
        Some(GitState::Revert)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn timeout_returns_even_when_a_grandchild_holds_stdout() {
        use std::os::unix::fs::PermissionsExt;
        // A fake git that spawns a sleeper sharing its stdout and then
        // blocks itself, so both the child and its grandchild outlive the
        // budget.
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("git");
        std::fs::write(&fake, "#!/bin/sh\nsleep 5 &\nsleep 5\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let start = Instant::now();
        assert!(run_command(&fake, dir.path(), &[], Duration::from_millis(200)).is_none());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "took {:?}",
            start.elapsed()
        );
    }

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
        assert!(!info.missing_dir);
    }

    #[test]
    fn missing_dir_sets_the_flag_and_no_git_data() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("removed-worktree");
        let info = collect(&gone);
        assert!(info.missing_dir);
        assert!(info.branch.is_none());
        assert_eq!(info.state, None);
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
    fn status_v2_classifies_entries_and_reads_the_header() {
        let out = concat!(
            "# branch.oid abc\n",
            "# branch.head feat/x\n",
            "# branch.upstream origin/feat/x\n",
            "# branch.ab +1 -0\n",
            "? new.txt\n",
            "1 A. N... 100644 100644 100644 0 0 staged.txt\n",
            "1 .M N... 100644 100644 100644 0 0 mod.rs\n",
            "1 D. N... 100644 000000 000000 0 0 gone.rs\n",
            "2 R. N... 100644 100644 100644 0 0 R100 b\ta\n",
            "1 MM N... 100644 100644 100644 0 0 both.rs\n",
        );
        let s = parse_status(out);
        assert_eq!(s.branch.as_deref(), Some("feat/x"));
        assert_eq!((s.added, s.removed, s.changed), (2, 1, 3));
        assert!(!s.unmerged);

        let s = parse_status("");
        assert!(s.branch.is_none());
        assert_eq!((s.added, s.removed, s.changed), (0, 0, 0));
    }

    #[test]
    fn status_v2_reads_unborn_detached_and_unmerged() {
        let s = parse_status("# branch.oid (initial)\n# branch.head main\n");
        assert_eq!(s.branch.as_deref(), Some("main"));

        let s = parse_status("# branch.oid abc\n# branch.head (detached)\n");
        assert_eq!(s.branch.as_deref(), Some("HEAD"));

        let s = parse_status(concat!(
            "# branch.oid abc\n# branch.head main\n",
            "u UU N... 100644 100644 100644 100644 1 2 3 f\n",
        ));
        assert!(s.unmerged);
        assert_eq!((s.added, s.removed, s.changed), (0, 0, 1));
    }

    #[test]
    fn untracked_directory_counts_each_file() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::create_dir(dir.path().join("nd")).unwrap();
        for name in ["x", "y", "z"] {
            std::fs::write(dir.path().join("nd").join(name), "n\n").unwrap();
        }
        let info = collect(dir.path());
        assert_eq!(info.files_added, 3);
    }

    #[test]
    fn unborn_repo_reports_its_branch_and_repo_name() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("fresh");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        let info = collect(&repo);
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert!(info.on_default_branch);
        assert_eq!(info.repo_name_fallback.as_deref(), Some("fresh"));
        assert!(!info.missing_dir);
    }

    #[test]
    fn detached_head_reads_as_head() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        git(dir.path(), &["checkout", "--detach"]);
        assert_eq!(collect(dir.path()).branch.as_deref(), Some("HEAD"));
    }

    #[test]
    fn stash_pop_conflict_reports_conflict_without_a_marker() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("f.txt"), "two\n").unwrap();
        git(dir.path(), &["stash", "push", "-q"]);
        std::fs::write(dir.path().join("f.txt"), "three\n").unwrap();
        git(dir.path(), &["commit", "-qam", "c"]);
        // The pop fails with a conflict; run without asserting success.
        let _ = Command::new("git")
            .args(["stash", "pop"])
            .current_dir(dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let out = run_git(dir.path(), &["rev-parse", "--git-dir"]).unwrap();
        let git_dir = resolve(dir.path(), out.trim());
        assert!(
            !git_dir.join("MERGE_HEAD").exists(),
            "the pop leaves no marker"
        );
        assert_eq!(collect(dir.path()).state, Some(GitState::Conflict));
    }

    #[test]
    fn head_file_names_the_branch_or_a_detached_head() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("HEAD"), "ref: refs/heads/feat/x\n").unwrap();
        assert_eq!(head_from_file(dir.path()).as_deref(), Some("feat/x"));
        std::fs::write(dir.path().join("HEAD"), format!("{}\n", "a".repeat(40))).unwrap();
        assert_eq!(head_from_file(dir.path()).as_deref(), Some("HEAD"));
        // The reftable backend keeps a placeholder here.
        std::fs::write(dir.path().join("HEAD"), "ref: refs/heads/.invalid\n").unwrap();
        assert!(head_from_file(dir.path()).is_none());
        std::fs::write(dir.path().join("HEAD"), "garbage\n").unwrap();
        assert!(head_from_file(dir.path()).is_none());
        assert!(head_from_file(&dir.path().join("missing")).is_none());
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

    #[test]
    fn submodule_common_dirs_sit_under_a_git_modules_directory() {
        assert!(!submodule_common_dir(Path::new("/w/repo/.git")));
        assert!(!submodule_common_dir(Path::new("/w/project/.bare")));
        assert!(!submodule_common_dir(Path::new("/w/modules/.git")));
        assert!(submodule_common_dir(Path::new("/w/super/.git/modules/lib")));
        assert!(submodule_common_dir(Path::new(
            "/w/super/.git/modules/lib/modules/inner"
        )));
        assert!(submodule_common_dir(Path::new(
            "/w/super/.git/worktrees/wt/modules/lib"
        )));
        assert!(submodule_common_dir(Path::new(
            "/w/project/.bare/worktrees/main/modules/lib"
        )));
        assert!(!submodule_common_dir(Path::new(
            "/w/worktrees/modules/.git"
        )));
    }

    #[test]
    fn repo_name_takes_the_common_dir_parent_outside_a_submodule() {
        let dir = Path::new("/nonexistent");
        assert_eq!(
            repo_name(dir, Path::new("/w/repo/.git")).as_deref(),
            Some("repo")
        );
        // A bare common repository with checkouts beside it: the parent still names it.
        assert_eq!(
            repo_name(dir, Path::new("/w/project/.bare")).as_deref(),
            Some("project")
        );
    }

    #[test]
    fn submodule_is_named_after_its_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        init_repo(&sub);
        let sup = dir.path().join("super");
        std::fs::create_dir(&sup).unwrap();
        init_repo(&sup);
        git(
            &sup,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub.to_str().unwrap(),
                "lib",
            ],
        );
        let checkout = sup.join("lib");
        assert_eq!(
            collect(&checkout).repo_name_fallback.as_deref(),
            Some("lib")
        );
        assert_eq!(
            branch_location(&checkout).map(|l| l.repo).as_deref(),
            Some("lib")
        );
    }

    #[test]
    fn submodule_inside_a_linked_worktree_is_named_after_its_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        init_repo(&sub);
        let sup = dir.path().join("super");
        std::fs::create_dir(&sup).unwrap();
        init_repo(&sup);
        let wt = dir.path().join("wt");
        git(
            &sup,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"],
        );
        git(
            &wt,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub.to_str().unwrap(),
                "lib",
            ],
        );
        let checkout = wt.join("lib");
        assert_eq!(
            collect(&checkout).repo_name_fallback.as_deref(),
            Some("lib")
        );
        assert_eq!(
            branch_location(&checkout).map(|l| l.repo).as_deref(),
            Some("lib")
        );
    }

    #[test]
    fn submodule_inside_a_bare_common_repository_checkout_is_named_after_its_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        init_repo(&sub);
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        init_repo(&src);
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        git(
            &project,
            &["clone", "-q", "--bare", src.to_str().unwrap(), ".bare"],
        );
        git(
            &project,
            &[
                "--git-dir",
                ".bare",
                "worktree",
                "add",
                "-q",
                "main",
                "main",
            ],
        );
        let checkout = project.join("main");
        git(
            &checkout,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub.to_str().unwrap(),
                "lib",
            ],
        );
        let inner = checkout.join("lib");
        assert_eq!(collect(&inner).repo_name_fallback.as_deref(), Some("lib"));
        assert_eq!(
            branch_location(&inner).map(|l| l.repo).as_deref(),
            Some("lib")
        );
    }

    #[test]
    fn bare_repository_keeps_its_branch_and_name() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        init_repo(&src);
        let bare = dir.path().join("bare.git");
        git(
            dir.path(),
            &[
                "clone",
                "-q",
                "--bare",
                src.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        let info = collect(&bare);
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert!(info.repo_name_fallback.is_some());
        assert!(!info.linked_worktree);
        assert_eq!(info.state, None);
        assert_eq!(
            branch_location(&bare).map(|l| l.branch).as_deref(),
            Some("main")
        );
    }

    #[test]
    fn inside_the_git_dir_keeps_its_branch_and_name() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("myrepo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        let inside = repo.join(".git");
        let info = collect(&inside);
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert_eq!(info.repo_name_fallback.as_deref(), Some("myrepo"));
        assert_eq!(
            branch_location(&inside).map(|l| l.repo).as_deref(),
            Some("myrepo")
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
