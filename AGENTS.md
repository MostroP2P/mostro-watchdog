# AGENTS.md

Guidance for AI coding agents (Claude Code, Cursor, Codex, Aider, and similar)
working on this repository. Human contributors are welcome to follow it too.

## Language policy

**Everything in this repository is written in English.** No exceptions.

This applies to:

- Source code: identifiers, function names, variable names, module names
- Code comments and doc comments (`//`, `///`, `//!`)
- Log messages, error messages, and any user-facing bot output
- Documentation: `README.md`, `*.md` files, configuration examples
- Commit messages and branch names
- Pull request titles, descriptions, and every comment on a pull request
- Issue titles, descriptions, and comments
- Code review comments and replies, including replies to automated reviewers
- Release notes and changelog entries
- Test names and test fixtures

In short: every word written into GitHub or into the repository is in English.
Discussion threads included — if it is visible to someone browsing the project,
it is in English.

Contributors may speak any language among themselves, in chat or in person; the
rule is about what gets written down where the project lives. This keeps it
readable for the whole Mostro community regardless of where a contributor is
from.

This applies to agents as much as to people: if you are prompted in another
language, answer the person in that language, but write English into the
repository, the pull request, and the issue tracker.

## Project overview

`mostro-watchdog` is a Nostr-to-Telegram notification bot for Mostro
administrators. It subscribes to dispute events (kind 38386) published by a
Mostro daemon and forwards them as formatted alerts to a Telegram group or
channel.

```text
Mostro daemon → Nostr (kind 38386) → mostro-watchdog → Telegram alert
```

Key crates: `nostr-sdk` (Nostr client), `teloxide` (Telegram bot), `tokio`
(async runtime), `sqlx` with SQLite (dispute message store).

## Repository layout

| Path | Purpose |
|---|---|
| `src/main.rs` | Entry point, Nostr subscription, event handling, health tasks, Telegram formatting |
| `src/config.rs` | TOML configuration parsing and validation |
| `src/db.rs` | SQLite store that maps disputes to sent Telegram messages |
| `src/version.rs` | Version and commit reporting for the CLI and the `/version` command |
| `build.rs` | Embeds the git commit hash at build time |
| `config.example.toml` | Documented configuration template |
| `.github/workflows/` | CI, Docker, and release pipelines |
| `*.md` | Feature and operations documentation |

`config.toml` is git-ignored and holds real credentials. Never commit it, and
never paste a bot token or private key into code, tests, logs, or documentation.

## Development workflow

Run all of these before opening a pull request. CI runs the same commands and
will fail the build otherwise:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --verbose
cargo build --release
```

Clippy runs with `-D warnings`, so a warning is a build failure. Do not silence
one with `#[allow(...)]` unless there is a genuine reason, and write that reason
in a comment next to it.

### Test-driven development

Write the test first, watch it fail, then implement. Tests live in
`#[cfg(test)] mod tests` blocks next to the code they exercise. Name them after
the behaviour under test, not the function:

```rust
#[test]
fn escapes_markdown_v2_reserved_characters() { /* ... */ }
```

Prefer arrange-act-assert structure. Anything touching event parsing, Markdown
escaping, or configuration validation needs a test — those are where malformed
remote input reaches the bot.

## Coding conventions

- Follow `rustfmt` defaults; never hand-format around it.
- Handle errors explicitly. Do not swallow them: an ignored `Result` in a
  background task means silent failure in production.
- Log with `tracing` (`info!`, `warn!`, `error!`). Reserve `println!` and
  `eprintln!` for actual CLI output — `--version`, `--help`, and argument
  errors — where the caller expects plain stdout/stderr, not log formatting.
- Keep functions focused and files cohesive. `src/main.rs` is already large;
  prefer extracting a new module over growing it further.
- Prefer returning new values over mutating in place where it does not cost
  performance.
- Use named constants instead of magic numbers, especially for timeouts,
  intervals, and Nostr event kinds.

## Domain rules

These are project-specific and easy to get wrong.

### The Telegram channel is for disputes only

The dispute channel must contain dispute events and nothing else. Infrastructure
noise — relay disconnections, reconnection attempts, relay list changes — is
logged with `tracing`, never sent to Telegram. False positives in that channel
erode the signal admins rely on.

The deliberate exceptions are the startup message, which is sent
unconditionally on every launch, and the heartbeat, which is off by default and
enabled with `heartbeat_enabled`.

### Telegram MarkdownV2 escaping

Every dynamic value interpolated into a Telegram message must be escaped:

- `escape_markdown` for normal text
- `escape_markdown_code` for values inside backticks

An unescaped `.`, `-`, or `_` from a dispute id or pubkey makes the Telegram API
reject the whole message, so the alert is lost. When adding a message template,
add a test covering the escaping.

### Nostr events

- Dispute events are kind 38386, filtered by the configured Mostro pubkey.
- Relay lists are discovered via NIP-65 (kind 10002), with the configured relays
  acting as bootstrap and fallback.
- Treat every field of an incoming event as untrusted input. Parse defensively
  and never index into tag slices without checking the length first.

### Upgrading nostr-sdk

`nostr-sdk` makes breaking API changes between minor versions. When bumping it,
expect to migrate call sites, and check the crate source in
`~/.cargo/registry/src/*/nostr-sdk-<version>/` when the documentation is unclear.
Verify with the full check suite above, not just `cargo build`.

## Dependencies and security

- `cargo audit` runs in CI as the **Security Audit** job and blocks on
  vulnerabilities.
- Prefer fixing an advisory by upgrading. Only add an ID to the `--ignore` list
  in `.github/workflows/ci.yml` when no upgrade path exists, and document why in
  the pull request.
- When adding a dependency, prefer a maintained crate with a small tree over
  hand-rolling. Justify the addition in the pull request.
- The `openssl-vendored` feature exists for ARM64 cross-compilation. Check with
  `cargo tree` that a dependency bump has not changed the TLS backend.

## Commits and pull requests

Conventional commit format:

```text
<type>: <description>

<optional body explaining why, not what>
```

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `ci`.

Pull requests should state what changed and why, list the verification actually
performed, and call out anything left unverified. Do not claim a check passed
without running it. If a CI job fails for reasons unrelated to the change, say
so explicitly rather than leaving it unexplained.

Keep a pull request to one concern. A dependency bump and a feature belong in
separate pull requests, even when they touch the same file.

### Working on someone else's branch

Resolving a conflict on another contributor's pull request is fine, but use a
merge commit rather than rewriting their history, and leave a comment explaining
what was resolved and why. If the resolution requires a decision beyond the
mechanical conflict — dropping a feature, changing behaviour — ask rather than
deciding silently.

## Releases

Releases are automated with `cargo release` via the **Cargo Release** GitHub
Actions workflow, which bumps the version, tags, builds cross-platform binaries,
and publishes the GitHub release. Do not bump the version in `Cargo.toml` by
hand in a feature pull request. See `RELEASE.md` for details.

## Guidance for agents

- Read before you edit. Match the conventions of the surrounding code instead of
  importing habits from other projects.
- Run the verification suite and report the real result. Never state that tests
  pass without having run them.
- Say what you did not verify. A change that compiles is not a change that
  works: the Nostr event loop and Telegram delivery have no automated coverage,
  so changes there need a run against real relays.
- Stay within the scope you were asked for. Surface adjacent problems you notice
  rather than fixing them unprompted.
- Never commit secrets, and never weaken a security check to make CI green.
