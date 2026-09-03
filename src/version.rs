//! Build-time version information reported by the CLI and the `/version` bot command.

use crate::escape_markdown_code;

/// Crate version from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short git commit hash embedded by `build.rs`, or `"unknown"` when the build
/// happened outside a git checkout.
pub const GIT_COMMIT: &str = env!("MOSTRO_WATCHDOG_GIT_COMMIT");

/// Telegram MarkdownV2 answer for the `/version` command.
///
/// Both values go inside code spans, so they are escaped with
/// [`escape_markdown_code`]: a version like `0.3.0` would otherwise break the
/// message with unescaped dots.
pub fn version_message() -> String {
    format!(
        "🐕 *mostro\\-watchdog*\n\n📦 *Version:* `{}`\n🔖 *Commit:* `{}`",
        escape_markdown_code(VERSION),
        escape_markdown_code(GIT_COMMIT)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_the_crate_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn git_commit_is_never_empty() {
        // `build.rs` falls back to "unknown" when git is unavailable.
        assert!(!GIT_COMMIT.is_empty());
    }

    #[test]
    fn version_message_reports_version_and_commit() {
        let message = version_message();

        assert!(message.contains(VERSION), "message was: {message}");
        assert!(message.contains(GIT_COMMIT), "message was: {message}");
    }

    #[test]
    fn version_message_leaves_no_unescaped_backticks_inside_code_spans() {
        // Four backticks: one opening and one closing per code span.
        assert_eq!(version_message().matches('`').count(), 4);
    }
}
