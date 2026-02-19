# GitHub Actions Setup

This document describes the setup required for the CI/CD workflows.

## Required Permissions

The workflows require the following GitHub permissions:
- **Contents: Write** - For creating releases and pushing tags
- **Actions: Read** - For workflow execution

## Secrets Configuration

No additional secrets are required beyond the default `GITHUB_TOKEN` which is automatically provided by GitHub Actions.

The workflows use:
- `GITHUB_TOKEN` - Automatically provided by GitHub for:
  - Creating releases
  - Uploading release assets  
  - Committing changelog updates
  - Pushing tags

## Workflow Files

### 1. `ci.yml` - Continuous Integration
- **Triggers**: Push to main/develop, Pull Requests
- **Functions**:
  - Format checking (`cargo fmt`)
  - Linting (`cargo clippy`)
  - Tests (`cargo test`)
  - Cross-compilation validation
  - Security audit (`cargo audit`)

### 2. `cargo-release.yml` - Version Management  
- **Triggers**: Manual workflow dispatch
- **Functions**:
  - Runs `cargo release` with specified level
  - Creates git tags
  - Triggers release workflow

### 3. `release.yml` - Binary Distribution
- **Triggers**: Tag pushes (v*.*.*)
- **Functions**:
  - Cross-compiles for all platforms
  - Updates `CHANGELOG.md`
  - Creates GitHub release
  - Uploads binaries and checksums
  - Generates `manifest.txt`

## Platform Support

The release workflow builds for:
- Linux x86_64 (GNU)
- Linux ARM64 (GNU) 
- macOS x86_64
- macOS ARM64 (Apple Silicon)
- Windows x86_64 (GNU)

## Usage

### Automated Release Process

1. **Trigger Release**:
   - Go to Actions → "Cargo Release"
   - Click "Run workflow"
   - Select patch/minor/major
   - Click "Run workflow"

2. **Automatic Chain**:
   ```
   cargo-release.yml → creates tag → release.yml → builds binaries
   ```

3. **Result**:
   - New version in `Cargo.toml`
   - Updated `CHANGELOG.md`
   - Git tag created
   - GitHub release with binaries
   - SHA256 checksums and manifest

### Manual Release (Fallback)

If workflows fail, manual release:
```bash
# Install cargo-release
cargo install cargo-release

# Create release
cargo release patch --execute  # or minor/major

# This creates the tag which triggers the build workflow
```

## Troubleshooting

### Permission Errors
- Verify repository has "Actions: Write" permission
- Check that workflows are enabled in repository settings

### Failed Cross-compilation
- Linux ARM64 builds require cross-compilation tools
- macOS builds require macOS runners (GitHub provides these)
- Windows builds use MinGW cross-compiler

### Missing Assets
- Release workflow may take 10-15 minutes to complete
- Check workflow logs for specific build failures
- Some platforms may fail independently

### Version Conflicts
- Ensure clean git state before releasing
- Check that version in `Cargo.toml` isn't already tagged
- Use `--dry-run` to preview changes

## Monitoring

Check workflow status:
1. **Repository Actions Tab** - View all workflow runs
2. **Release Page** - Verify releases have all expected assets
3. **CI Badges** - README shows current CI status

## Security

- Workflows run in isolated GitHub-hosted runners
- No credentials or secrets are exposed in logs
- Binary checksums allow users to verify downloads
- All code changes go through PR review before reaching main branch