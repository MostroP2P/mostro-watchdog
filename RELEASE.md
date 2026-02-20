# Release Process

This document describes the automated release process for mostro-watchdog.

## Overview

The project uses a fully automated CI/CD pipeline that:
1. **Creates new versions** using `cargo release`
2. **Builds cross-platform binaries** for major architectures
3. **Generates changelog** automatically from git commits
4. **Creates manifest** with SHA256 checksums for all binaries
5. **Publishes GitHub release** with all assets

## Quick Release

### Using GitHub Actions (Recommended)

1. Go to **Actions** tab in the GitHub repository
2. Select **"Cargo Release"** workflow
3. Click **"Run workflow"**
4. Choose release level:
   - **patch** (0.1.0 → 0.1.1) - Bug fixes
   - **minor** (0.1.0 → 0.2.0) - New features  
   - **major** (0.1.0 → 1.0.0) - Breaking changes
5. Click **"Run workflow"**

The system will:
- ✅ Run tests
- ✅ Bump version in `Cargo.toml`
- ✅ Create and push git tag
- ✅ Trigger release workflow automatically
- ✅ Build binaries for all platforms
- ✅ Update `CHANGELOG.md`
- ✅ Create GitHub release with assets

### Using Command Line

```bash
# Install cargo-release if not already installed
cargo install cargo-release

# Patch release (0.1.0 → 0.1.1)
cargo release patch --execute

# Minor release (0.1.0 → 0.2.0)  
cargo release minor --execute

# Major release (0.1.0 → 1.0.0)
cargo release major --execute

# Dry run (preview changes)
cargo release patch --dry-run
```

## Supported Architectures

The automated build creates binaries for:

- **Linux x86_64**: `mostro-watchdog-linux-x86_64`
- **Linux ARM64**: `mostro-watchdog-linux-aarch64` 
- **macOS x86_64**: `mostro-watchdog-macos-x86_64`
- **macOS ARM64**: `mostro-watchdog-macos-aarch64`
- **Windows x86_64**: `mostro-watchdog-windows-x86_64.exe`

## Release Assets

Each release includes:

- **Binaries**: Cross-compiled for all supported platforms
- **Checksums**: Individual `.sha256` files for each binary
- **Manifest**: `manifest.txt` with all SHA256 checksums
- **Changelog**: Automatically updated with commit history

## Verification

Users can verify downloaded binaries:

```bash
# Download the manifest (replace {VERSION} with desired release, e.g., v0.1.1)
curl -LO https://github.com/MostroP2P/mostro-watchdog/releases/download/{VERSION}/manifest.txt

# Or get the latest release programmatically:
# LATEST=$(curl -s https://api.github.com/repos/MostroP2P/mostro-watchdog/releases/latest | grep -o '"tag_name": "[^"]*"' | cut -d'"' -f4)
# curl -LO https://github.com/MostroP2P/mostro-watchdog/releases/download/$LATEST/manifest.txt

# Verify your binary (example for Linux x86_64)
# Linux/WSL
sha256sum -c manifest.txt --ignore-missing

# macOS  
shasum -a 256 -c manifest.txt
```

## Changelog

The `CHANGELOG.md` file is automatically updated with:
- New version number and date
- List of commits since last release
- Proper Markdown formatting
- Links to GitHub releases

## Configuration

Release behavior is configured in `Cargo.toml`:

```toml
[package.metadata.release]
tag = true              # Create git tag
push = true            # Push to origin  
publish = false        # Don't publish to crates.io
pre-release-hook = ["cargo", "test"]  # Run tests before release
```

## Troubleshooting

### Failed Release
- Check that all tests pass locally: `cargo test`
- Ensure clean git state: `git status`
- Verify GitHub token has proper permissions

### Missing Binaries
- Check the "Release" workflow in GitHub Actions
- Cross-compilation failures are logged in workflow runs
- Some platforms may fail independently

### Invalid Checksums
- Re-download the `manifest.txt` file
- Ensure binary wasn't corrupted during download
- Compare individual `.sha256` files if needed

## Manual Process (Fallback)

If automated release fails:

1. Manually bump version in `Cargo.toml`
2. Commit changes: `git commit -am "Bump version to X.Y.Z"`
3. Create tag: `git tag vX.Y.Z`
4. Push commit: `git push`
5. Push tag: `git push origin vX.Y.Z`
6. The tag push will trigger the release workflow