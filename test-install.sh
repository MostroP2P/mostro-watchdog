#!/bin/bash

# Test script for the installation script
# Tests various scenarios without actually installing

set -e

# Source the installation script functions for testing
# We'll create a test version that doesn't download/install

# Test colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

print_info() {
    echo -e "${BLUE}ℹ️  $1${NC}"
}

print_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

print_error() {
    echo -e "${RED}❌ $1${NC}"
}

# Mock uname for testing
mock_uname_s=""
mock_uname_m=""

uname() {
    case "$1" in
        -s) echo "$mock_uname_s" ;;
        -m) echo "$mock_uname_m" ;;
        *) echo "unknown" ;;
    esac
}

# Platform detection function (copied from install.sh)
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
            return 1
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
            return 1
            ;;
    esac
    
    if [ "$os" = "windows" ]; then
        BINARY_NAME="mostro-watchdog-${os}-${arch}.exe"
        EXECUTABLE_NAME="mostro-watchdog.exe"
    else
        BINARY_NAME="mostro-watchdog-${os}-${arch}"
        EXECUTABLE_NAME="mostro-watchdog"
    fi
    
    echo "Platform: ${os}-${arch}, Binary: ${BINARY_NAME}, Executable: ${EXECUTABLE_NAME}"
}

# Test cases
test_linux_x86_64() {
    print_info "Testing Linux x86_64 detection"
    mock_uname_s="Linux"
    mock_uname_m="x86_64"
    
    if detect_platform; then
        if [ "$BINARY_NAME" = "mostro-watchdog-linux-x86_64" ] && [ "$EXECUTABLE_NAME" = "mostro-watchdog" ]; then
            print_success "Linux x86_64 detection passed"
            return 0
        else
            print_error "Linux x86_64 detection failed - wrong binary name"
            return 1
        fi
    else
        print_error "Linux x86_64 detection failed"
        return 1
    fi
}

test_linux_aarch64() {
    print_info "Testing Linux ARM64 detection"
    mock_uname_s="Linux"
    mock_uname_m="aarch64"
    
    if detect_platform; then
        if [ "$BINARY_NAME" = "mostro-watchdog-linux-aarch64" ] && [ "$EXECUTABLE_NAME" = "mostro-watchdog" ]; then
            print_success "Linux ARM64 detection passed"
            return 0
        else
            print_error "Linux ARM64 detection failed - wrong binary name"
            return 1
        fi
    else
        print_error "Linux ARM64 detection failed"
        return 1
    fi
}

test_macos_x86_64() {
    print_info "Testing macOS Intel detection"
    mock_uname_s="Darwin"
    mock_uname_m="x86_64"
    
    if detect_platform; then
        if [ "$BINARY_NAME" = "mostro-watchdog-macos-x86_64" ] && [ "$EXECUTABLE_NAME" = "mostro-watchdog" ]; then
            print_success "macOS Intel detection passed"
            return 0
        else
            print_error "macOS Intel detection failed - wrong binary name"
            return 1
        fi
    else
        print_error "macOS Intel detection failed"
        return 1
    fi
}

test_macos_arm64() {
    print_info "Testing macOS Apple Silicon detection"
    mock_uname_s="Darwin"
    mock_uname_m="arm64"
    
    if detect_platform; then
        if [ "$BINARY_NAME" = "mostro-watchdog-macos-aarch64" ] && [ "$EXECUTABLE_NAME" = "mostro-watchdog" ]; then
            print_success "macOS Apple Silicon detection passed"
            return 0
        else
            print_error "macOS Apple Silicon detection failed - wrong binary name"
            return 1
        fi
    else
        print_error "macOS Apple Silicon detection failed"
        return 1
    fi
}

test_windows_x86_64() {
    print_info "Testing Windows detection"
    mock_uname_s="MINGW64_NT-10.0"
    mock_uname_m="x86_64"
    
    if detect_platform; then
        if [ "$BINARY_NAME" = "mostro-watchdog-windows-x86_64.exe" ] && [ "$EXECUTABLE_NAME" = "mostro-watchdog.exe" ]; then
            print_success "Windows detection passed"
            return 0
        else
            print_error "Windows detection failed - wrong binary name"
            return 1
        fi
    else
        print_error "Windows detection failed"
        return 1
    fi
}

test_unsupported_os() {
    print_info "Testing unsupported OS detection"
    mock_uname_s="FreeBSD"
    mock_uname_m="x86_64"
    
    if detect_platform 2>/dev/null; then
        print_error "Unsupported OS test failed - should have returned error"
        return 1
    else
        print_success "Unsupported OS correctly rejected"
        return 0
    fi
}

test_unsupported_arch() {
    print_info "Testing unsupported architecture detection"
    mock_uname_s="Linux"
    mock_uname_m="i386"
    
    if detect_platform 2>/dev/null; then
        print_error "Unsupported architecture test failed - should have returned error"
        return 1
    else
        print_success "Unsupported architecture correctly rejected"
        return 0
    fi
}

# Test GitHub API functionality (mock)
test_github_api() {
    print_info "Testing GitHub API functionality"
    
    # Test if we can reach GitHub API (without actually calling it)
    if command -v curl >/dev/null 2>&1; then
        print_success "curl is available for GitHub API calls"
    elif command -v wget >/dev/null 2>&1; then
        print_success "wget is available for GitHub API calls"
    else
        print_error "Neither curl nor wget available - installation would fail"
        return 1
    fi
    
    return 0
}

# Test checksum tools
test_checksum_tools() {
    print_info "Testing checksum verification tools"
    
    if command -v sha256sum >/dev/null 2>&1; then
        print_success "sha256sum is available for checksum verification"
        return 0
    elif command -v shasum >/dev/null 2>&1; then
        print_success "shasum is available for checksum verification"
        return 0
    else
        print_info "No checksum tools available - verification will be skipped"
        return 0
    fi
}

# Run all tests
run_tests() {
    local failed=0
    local total=0
    
    echo "🧪 mostro-watchdog Installation Script Tests"
    echo "============================================="
    echo
    
    # Platform detection tests
    total=$((total + 1))
    test_linux_x86_64 || failed=$((failed + 1))
    echo
    
    total=$((total + 1))
    test_linux_aarch64 || failed=$((failed + 1))
    echo
    
    total=$((total + 1))
    test_macos_x86_64 || failed=$((failed + 1))
    echo
    
    total=$((total + 1))
    test_macos_arm64 || failed=$((failed + 1))
    echo
    
    total=$((total + 1))
    test_windows_x86_64 || failed=$((failed + 1))
    echo
    
    # Error handling tests
    total=$((total + 1))
    test_unsupported_os || failed=$((failed + 1))
    echo
    
    total=$((total + 1))
    test_unsupported_arch || failed=$((failed + 1))
    echo
    
    # Dependency tests
    total=$((total + 1))
    test_github_api || failed=$((failed + 1))
    echo
    
    total=$((total + 1))
    test_checksum_tools || failed=$((failed + 1))
    echo
    
    # Summary
    echo "============================================="
    if [ $failed -eq 0 ]; then
        print_success "All $total tests passed! 🎉"
        return 0
    else
        print_error "$failed out of $total tests failed"
        return 1
    fi
}

# Main function
main() {
    run_tests
}

# Run tests
main "$@"