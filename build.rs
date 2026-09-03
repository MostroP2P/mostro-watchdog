use std::path::Path;
use std::process::Command;

/// Environment variable that lets a build without a git checkout (Docker, tarball
/// releases) inject the commit hash explicitly.
const COMMIT_OVERRIDE_ENV: &str = "MOSTRO_WATCHDOG_GIT_COMMIT";

/// Reported when the commit cannot be determined at build time.
const UNKNOWN_COMMIT: &str = "unknown";

fn main() {
    // Rebuild when the checked-out commit changes, so the embedded hash stays accurate.
    for path in git_watch_paths() {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed={COMMIT_OVERRIDE_ENV}");

    let commit = commit_override()
        .or_else(git_short_commit)
        .unwrap_or_else(|| UNKNOWN_COMMIT.to_string());

    println!("cargo:rustc-env={COMMIT_OVERRIDE_ENV}={commit}");
}

/// Files whose contents decide what `git rev-parse HEAD` returns.
///
/// `.git/HEAD` alone is not enough: on a branch it only holds `ref:
/// refs/heads/<name>`, which does not change when a commit lands. The branch ref
/// has to be watched too, and `packed-refs` as well because a packed branch has
/// no loose ref file at all. Paths are resolved with `git rev-parse --git-path`
/// so linked worktrees, where the git directory is not `./.git`, work as well.
///
/// Paths that do not exist are dropped: Cargo treats a missing watched path as
/// "always dirty", which would rerun the build script on every build. The two
/// ref sources cover each other — a branch is either a loose ref file or an
/// entry in `packed-refs`.
fn git_watch_paths() -> Vec<String> {
    ["HEAD".to_string(), "packed-refs".to_string()]
        .into_iter()
        .chain(git_symbolic_head())
        .filter_map(|r| git_path(&r))
        .filter(|p| Path::new(p).exists())
        .collect()
}

/// Resolve a path inside the git directory, e.g. `HEAD` -> `.git/HEAD`.
/// Returns `None` outside a repository or when git is unavailable.
fn git_path(relative: &str) -> Option<String> {
    git_output(&["rev-parse", "--git-path", relative])
}

/// The ref HEAD points at (`refs/heads/main`), or `None` on a detached HEAD.
fn git_symbolic_head() -> Option<String> {
    git_output(&["symbolic-ref", "--quiet", "HEAD"])
}

/// Read the commit hash from the build environment, ignoring empty values.
fn commit_override() -> Option<String> {
    let value = std::env::var(COMMIT_OVERRIDE_ENV).ok()?;
    let trimmed = value.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Ask git for the short hash of HEAD. Returns `None` when git is missing or the
/// source tree is not a repository, which is the normal case in Docker builds.
fn git_short_commit() -> Option<String> {
    git_output(&["rev-parse", "--short", "HEAD"])
}

/// Run a git command and return its trimmed stdout, or `None` when git is
/// missing, the source tree is not a repository, or the output is empty.
fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;

    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}
