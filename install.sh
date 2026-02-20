#!/bin/bash

# mostro-watchdog installation script
# Downloads and installs the latest pre-built binary for your platform

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Default installation directory
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

# GitHub repository
REPO="MostroP2P/mostro-watchdog"

# Print colored output
print_info() {
    echo -e "${BLUE}ℹ️  $1${NC}"
}

print_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

print_warning() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

print_error() {
    echo -e "${RED}❌ $1${NC}"
}

# Detect OS and architecture
detect_platform() {
    local os=$(uname -s | tr '[:upper:]' '[:lower:]')
    local arch=$(uname -m)
    
    case "$os" in
        linux*)
            os="linux"
            ;;
        darwin*)
            os="macos"
            ;;
        mingw*|msys*|cygwin*)
            os="windows"
            ;;
        *)
            print_error "Unsupported operating system: $os"
            exit 1
            ;;
    esac
    
    case "$arch" in
        x86_64|amd64)
            arch="x86_64"
            ;;
        aarch64|arm64)
            arch="aarch64"
            ;;
        *)
            print_error "Unsupported architecture: $arch"
            exit 1
            ;;
    esac
    
    if [ "$os" = "windows" ]; then
        BINARY_NAME="mostro-watchdog-${os}-${arch}.exe"
        EXECUTABLE_NAME="mostro-watchdog.exe"
    else
        BINARY_NAME="mostro-watchdog-${os}-${arch}"
        EXECUTABLE_NAME="mostro-watchdog"
    fi
    
    print_info "Detected platform: ${os}-${arch}"
}

# Get latest release version
get_latest_version() {
    print_info "Fetching latest release information..."
    
    if command -v curl >/dev/null 2>&1; then
        LATEST_VERSION=$(curl -s "https://api.github.com/repos/${REPO}/releases/latest" | grep -o '"tag_name": *"[^"]*"' | grep -o '"[^"]*"$' | tr -d '"')
    elif command -v wget >/dev/null 2>&1; then
        LATEST_VERSION=$(wget -qO- "https://api.github.com/repos/${REPO}/releases/latest" | grep -o '"tag_name": *"[^"]*"' | grep -o '"[^"]*"$' | tr -d '"')
    else
        print_error "Neither curl nor wget is available. Please install one of them."
        exit 1
    fi
    
    if [ -z "$LATEST_VERSION" ]; then
        print_error "Failed to get latest version information"
        exit 1
    fi
    
    print_info "Latest version: $LATEST_VERSION"
}

# Download binary
download_binary() {
    local download_url="https://github.com/${REPO}/releases/download/${LATEST_VERSION}/${BINARY_NAME}"
    local checksum_url="https://github.com/${REPO}/releases/download/${LATEST_VERSION}/manifest.txt"
    local temp_dir=$(mktemp -d)
    local binary_path="${temp_dir}/${BINARY_NAME}"
    local checksum_path="${temp_dir}/manifest.txt"
    
    print_info "Downloading ${BINARY_NAME}..."
    
    if command -v curl >/dev/null 2>&1; then
        curl -L -o "$binary_path" "$download_url"
        curl -L -o "$checksum_path" "$checksum_url"
    elif command -v wget >/dev/null 2>&1; then
        wget -O "$binary_path" "$download_url"
        wget -O "$checksum_path" "$checksum_url"
    fi
    
    if [ ! -f "$binary_path" ]; then
        print_error "Failed to download binary"
        exit 1
    fi
    
    print_success "Binary downloaded to $binary_path"
    
    # Verify checksum
    print_info "Verifying checksum..."
    cd "$temp_dir"
    
    if command -v sha256sum >/dev/null 2>&1; then
        if sha256sum -c manifest.txt --ignore-missing --status 2>/dev/null; then
            print_success "Checksum verification passed"
        else
            print_warning "Checksum verification failed, but continuing installation"
        fi
    elif command -v shasum >/dev/null 2>&1; then
        # macOS
        local expected_checksum=$(grep "$BINARY_NAME" manifest.txt | cut -d' ' -f1)
        local actual_checksum=$(shasum -a 256 "$BINARY_NAME" | cut -d' ' -f1)
        if [ "$expected_checksum" = "$actual_checksum" ]; then
            print_success "Checksum verification passed"
        else
            print_warning "Checksum verification failed, but continuing installation"
        fi
    else
        print_warning "No checksum tool available, skipping verification"
    fi
    
    TEMP_BINARY_PATH="$binary_path"
}

# Install binary
install_binary() {
    local install_path="${INSTALL_DIR}/${EXECUTABLE_NAME}"
    
    print_info "Installing to $install_path..."
    
    # Check if install directory is writable
    if [ ! -w "$INSTALL_DIR" ]; then
        print_info "Installation directory requires sudo access"
        if command -v sudo >/dev/null 2>&1; then
            sudo cp "$TEMP_BINARY_PATH" "$install_path"
            sudo chmod +x "$install_path"
        else
            print_error "Cannot write to $INSTALL_DIR and sudo is not available"
            print_info "Please run: cp '$TEMP_BINARY_PATH' '$install_path' && chmod +x '$install_path'"
            exit 1
        fi
    else
        cp "$TEMP_BINARY_PATH" "$install_path"
        chmod +x "$install_path"
    fi
    
    print_success "mostro-watchdog installed to $install_path"
}

# Verify installation
verify_installation() {
    if command -v "$EXECUTABLE_NAME" >/dev/null 2>&1; then
        local version=$("$EXECUTABLE_NAME" --version 2>/dev/null || echo "unknown")
        print_success "Installation verified: $version"
        return 0
    elif [ -x "${INSTALL_DIR}/${EXECUTABLE_NAME}" ]; then
        local version=$("${INSTALL_DIR}/${EXECUTABLE_NAME}" --version 2>/dev/null || echo "unknown")
        print_success "Installation verified: $version"
        print_warning "Note: ${INSTALL_DIR} may not be in your PATH"
        print_info "You can run: ${INSTALL_DIR}/${EXECUTABLE_NAME}"
        return 0
    else
        print_error "Installation verification failed"
        return 1
    fi
}

# Show post-installation instructions
show_next_steps() {
    echo
    print_info "🚀 Installation complete!"
    echo
    print_info "Next steps:"
    echo "  1. Create a config file:"
    echo "     curl -LO https://raw.githubusercontent.com/${REPO}/main/config.example.toml"
    echo "     cp config.example.toml config.toml"
    echo "     # Edit config.toml with your settings"
    echo
    echo "  2. Run mostro-watchdog:"
    if command -v "$EXECUTABLE_NAME" >/dev/null 2>&1; then
        echo "     $EXECUTABLE_NAME"
    else
        echo "     ${INSTALL_DIR}/${EXECUTABLE_NAME}"
    fi
    echo
    echo "  3. For help:"
    if command -v "$EXECUTABLE_NAME" >/dev/null 2>&1; then
        echo "     $EXECUTABLE_NAME --help"
    else
        echo "     ${INSTALL_DIR}/${EXECUTABLE_NAME} --help"
    fi
    echo
    print_info "📚 Documentation: https://github.com/${REPO}#readme"
}

# Cleanup
cleanup() {
    if [ -n "$TEMP_BINARY_PATH" ] && [ -f "$TEMP_BINARY_PATH" ]; then
        rm -f "$TEMP_BINARY_PATH"
    fi
}
trap cleanup EXIT

# Main installation flow
main() {
    echo "🐕 mostro-watchdog Installation Script"
    echo "======================================"
    echo
    
    # Parse command line arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            --install-dir)
                INSTALL_DIR="$2"
                shift 2
                ;;
            --help|-h)
                echo "Usage: $0 [options]"
                echo
                echo "Options:"
                echo "  --install-dir DIR    Installation directory (default: /usr/local/bin)"
                echo "  --help, -h           Show this help message"
                echo
                echo "Environment variables:"
                echo "  INSTALL_DIR          Installation directory"
                echo
                exit 0
                ;;
            *)
                print_error "Unknown option: $1"
                echo "Use --help for usage information"
                exit 1
                ;;
        esac
    done
    
    print_info "Installing to: $INSTALL_DIR"
    echo
    
    detect_platform
    get_latest_version
    download_binary
    install_binary
    
    if verify_installation; then
        show_next_steps
    else
        exit 1
    fi
}

# Run main function
main "$@"