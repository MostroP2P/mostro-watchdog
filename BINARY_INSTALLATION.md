# Binary Installation Guide

This document provides comprehensive instructions for installing pre-built mostro-watchdog binaries on all supported platforms.

## Quick Start

The fastest way to install mostro-watchdog is using our automatic installation script:

```bash
curl -fsSL https://raw.githubusercontent.com/MostroP2P/mostro-watchdog/main/install.sh | bash
```

## Installation Script Features

Our installation script provides a robust, cross-platform installation experience:

### ✅ **Automatic Platform Detection**
- Detects your operating system (Linux, macOS, Windows)
- Identifies your architecture (x86_64, ARM64)
- Downloads the correct binary automatically

### ✅ **Security & Verification**
- Downloads checksums from GitHub releases
- Verifies binary integrity using SHA256 checksums
- Uses secure HTTPS connections for all downloads

### ✅ **Intelligent Installation**
- Installs to `/usr/local/bin` by default (customizable)
- Handles sudo requirements automatically
- Sets correct executable permissions
- Verifies installation success

### ✅ **User-Friendly Experience**
- Colored output for easy reading
- Progress indicators and status messages
- Clear error messages and troubleshooting guidance
- Post-installation instructions and next steps

## Installation Script Usage

### Basic Installation

```bash
# Install to /usr/local/bin (requires sudo for most users)
curl -fsSL https://raw.githubusercontent.com/MostroP2P/mostro-watchdog/main/install.sh | bash
```

### Custom Installation Directory

```bash
# Install to ~/.local/bin (no sudo required)
curl -fsSL https://raw.githubusercontent.com/MostroP2P/mostro-watchdog/main/install.sh | bash -s -- --install-dir ~/.local/bin

# Install to custom directory
curl -fsSL https://raw.githubusercontent.com/MostroP2P/mostro-watchdog/main/install.sh | bash -s -- --install-dir /opt/mostro
```

### Environment Variables

```bash
# Set installation directory via environment variable
export INSTALL_DIR=~/.local/bin
curl -fsSL https://raw.githubusercontent.com/MostroP2P/mostro-watchdog/main/install.sh | bash
```

### Help and Options

```bash
# Download script first to see help
curl -fsSL https://raw.githubusercontent.com/MostroP2P/mostro-watchdog/main/install.sh > install.sh
chmod +x install.sh
./install.sh --help
```

## Supported Platforms

We provide pre-built binaries for all major platforms:

| Platform | Architecture | Binary Name | Notes |
|----------|-------------|-------------|--------|
| **Linux** | x86_64 | `mostro-watchdog-linux-x86_64` | Intel/AMD 64-bit |
| **Linux** | ARM64 | `mostro-watchdog-linux-aarch64` | Raspberry Pi, ARM servers |
| **macOS** | x86_64 | `mostro-watchdog-macos-x86_64` | Intel Macs |
| **macOS** | ARM64 | `mostro-watchdog-macos-aarch64` | Apple Silicon (M1/M2/M3) |
| **Windows** | x86_64 | `mostro-watchdog-windows-x86_64.exe` | 64-bit Windows |

## Manual Installation

If you prefer to install manually or the script doesn't work in your environment:

### 1. Download the Binary

Visit the [releases page](https://github.com/MostroP2P/mostro-watchdog/releases/latest) and download the appropriate binary for your platform.

### 2. Verify the Download (Recommended)

```bash
# Download checksums
curl -LO https://github.com/MostroP2P/mostro-watchdog/releases/latest/download/manifest.txt

# Linux/WSL
sha256sum -c manifest.txt --ignore-missing

# macOS
shasum -a 256 -c manifest.txt

# Windows (PowerShell)
Get-FileHash .\mostro-watchdog-windows-x86_64.exe -Algorithm SHA256
# Compare with hash in manifest.txt
```

### 3. Install the Binary

**Linux/macOS:**
```bash
# Make executable
chmod +x mostro-watchdog-*

# Move to system PATH
sudo mv mostro-watchdog-* /usr/local/bin/mostro-watchdog

# Or install to user directory (add ~/.local/bin to PATH if needed)
mkdir -p ~/.local/bin
mv mostro-watchdog-* ~/.local/bin/mostro-watchdog
```

**Windows:**
```powershell
# Option 1: System-wide installation (requires Administrator)
Move-Item .\mostro-watchdog-windows-x86_64.exe C:\Windows\System32\mostro-watchdog.exe

# Option 2: User installation
$UserBin = "$env:USERPROFILE\bin"
New-Item -ItemType Directory -Force -Path $UserBin
Move-Item .\mostro-watchdog-windows-x86_64.exe "$UserBin\mostro-watchdog.exe"
# Add $UserBin to your PATH environment variable
```

### 4. Verify Installation

```bash
# Check if binary is accessible
mostro-watchdog --version

# If not in PATH, try direct path
/usr/local/bin/mostro-watchdog --version
~/.local/bin/mostro-watchdog --version
```

## Troubleshooting

### Installation Script Issues

**Script fails to download:**
```bash
# Check internet connectivity
curl -I https://github.com

# Try alternative method
wget -qO- https://raw.githubusercontent.com/MostroP2P/mostro-watchdog/main/install.sh | bash
```

**Permission denied errors:**
```bash
# Install to user directory instead
curl -fsSL https://raw.githubusercontent.com/MostroP2P/mostro-watchdog/main/install.sh | bash -s -- --install-dir ~/.local/bin

# Make sure ~/.local/bin is in your PATH
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

**Unsupported platform:**
```bash
# Check your platform
uname -sm

# If your platform isn't supported, build from source:
git clone https://github.com/MostroP2P/mostro-watchdog.git
cd mostro-watchdog
cargo build --release
```

### Binary Issues

**Binary not found after installation:**
```bash
# Check if installed
ls -la /usr/local/bin/mostro-watchdog
ls -la ~/.local/bin/mostro-watchdog

# Check PATH
echo $PATH

# Run with full path
/usr/local/bin/mostro-watchdog --version
```

**Permission denied when running binary:**
```bash
# Make sure binary is executable
chmod +x /path/to/mostro-watchdog

# Check file permissions
ls -la /path/to/mostro-watchdog
```

### macOS Gatekeeper Issues

If macOS blocks the binary due to Gatekeeper:

```bash
# Remove quarantine attribute
xattr -d com.apple.quarantine /usr/local/bin/mostro-watchdog

# Or allow in System Preferences > Security & Privacy
```

### Windows Antivirus Issues

Some antivirus software may flag the binary:

1. Add an exception for the mostro-watchdog.exe file
2. Download directly from GitHub releases (trusted source)
3. Verify checksums to ensure file integrity

## Next Steps

After successful installation:

1. **Create configuration:**
   ```bash
   curl -LO https://raw.githubusercontent.com/MostroP2P/mostro-watchdog/main/config.example.toml
   cp config.example.toml config.toml
   # Edit config.toml with your settings
   ```

2. **Run mostro-watchdog:**
   ```bash
   mostro-watchdog
   ```

3. **Get help:**
   ```bash
   mostro-watchdog --help
   ```

## Updating

To update to a new version:

```bash
# Re-run the installation script
curl -fsSL https://raw.githubusercontent.com/MostroP2P/mostro-watchdog/main/install.sh | bash

# Or manually download the new binary and replace the old one
```

The installation script always downloads the latest release, so it's the easiest way to update.

## Uninstalling

To remove mostro-watchdog:

```bash
# Remove binary
sudo rm /usr/local/bin/mostro-watchdog
# Or: rm ~/.local/bin/mostro-watchdog

# Remove config (optional)
rm -rf ~/.config/mostro-watchdog/

# Remove current directory config (optional)
rm config.toml
```

## Building from Source

If pre-built binaries don't work for your platform or you prefer to build from source:

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build
git clone https://github.com/MostroP2P/mostro-watchdog.git
cd mostro-watchdog
cargo build --release

# Binary will be at ./target/release/mostro-watchdog
```

See the main README for detailed build dependencies and instructions.