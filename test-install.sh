#!/bin/bash

# Test script for the installation script
# Sources install.sh directly to test real functions

set -e

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

# Export mock function so it's available in subshells
export -f uname
export mock_uname_s mock_uname_m

# Source install.sh to get the real detect_platform function
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/install.sh"

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

    # Run in subshell to capture exit (real install.sh uses exit 1)
    if (detect_platform 2>/dev/null); then
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

    # Run in subshell to capture exit (real install.sh uses exit 1)
    if (detect_platform 2>/dev/null); then
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

# Run tests
run_tests
