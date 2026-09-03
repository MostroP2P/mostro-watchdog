use std::process::Command;

/// Environment variable that lets a build without a git checkout (Docker, tarball
/// releases) inject the commit hash explicitly.
const COMMIT_OVERRIDE_ENV: &str = "MOSTRO_WATCHDOG_GIT_COMMIT";

/// Reported when the commit cannot be determined at build time.
const UNKNOWN_COMMIT: &str = "unknown";

fn main() {
    // Rebuild when the checked-out commit changes, so the embedded hash stays accurate.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-env-changed={COMMIT_OVERRIDE_ENV}");

    let commit = commit_override()
        .or_else(git_short_commit)
        .unwrap_or_else(|| UNKNOWN_COMMIT.to_string());

    println!("cargo:rustc-env={COMMIT_OVERRIDE_ENV}={commit}");
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
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let hash = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if hash.is_empty() {
        None
    } else {
        Some(hash)
    }
}
