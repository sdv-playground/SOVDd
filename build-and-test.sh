#!/bin/bash
# Development convenience script - build and test after each edit
# Usage: ./build-and-test.sh [options]
#
# Options:
#   --release    Build in release mode
#   --quick      Skip tests, only build
#   --watch      Watch for changes and rebuild (requires cargo-watch)
#   --check      Only check compilation, don't build binaries
#   --clippy     Run clippy lints
#   --all        Run all checks: clippy, build, test

set -e

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$PROJECT_DIR"

# Source cargo environment if available
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

log_step() { echo -e "\n${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"; echo -e "${CYAN}▶ $1${NC}"; }
log_ok() { echo -e "${GREEN}✓ $1${NC}"; }
log_err() { echo -e "${RED}✗ $1${NC}"; }
log_warn() { echo -e "${YELLOW}! $1${NC}"; }

# Track timing
START_TIME=$(date +%s)

# Parse arguments
MODE="default"
BUILD_TYPE="debug"

for arg in "$@"; do
    case $arg in
        --release)
            BUILD_TYPE="release"
            ;;
        --quick)
            MODE="quick"
            ;;
        --watch)
            MODE="watch"
            ;;
        --check)
            MODE="check"
            ;;
        --clippy)
            MODE="clippy"
            ;;
        --all)
            MODE="all"
            ;;
        --help|-h)
            echo "Usage: $0 [options]"
            echo ""
            echo "Options:"
            echo "  --release    Build in release mode"
            echo "  --quick      Skip tests, only build"
            echo "  --watch      Watch for changes and rebuild (requires cargo-watch)"
            echo "  --check      Only check compilation, don't build binaries"
            echo "  --clippy     Run clippy lints"
            echo "  --all        Run all checks: fmt, clippy, build, test"
            echo ""
            echo "Examples:"
            echo "  $0                  # Default: build debug + run tests"
            echo "  $0 --quick          # Fast compile check"
            echo "  $0 --release        # Build release + run tests"
            echo "  $0 --all            # Full CI-style check"
            exit 0
            ;;
    esac
done

# Watch mode - requires cargo-watch
if [[ "$MODE" == "watch" ]]; then
    if ! command -v cargo-watch &>/dev/null; then
        log_warn "cargo-watch not installed. Install with: cargo install cargo-watch"
        exit 1
    fi
    log_step "Watching for changes..."
    cargo watch -x "check --all" -x "test --all -- --nocapture --test-threads=1"
    exit 0
fi

# Check mode - fastest feedback
if [[ "$MODE" == "check" ]]; then
    log_step "Checking compilation..."
    cargo check --all
    log_ok "Compilation OK"
    exit 0
fi

# Clippy mode
if [[ "$MODE" == "clippy" ]]; then
    log_step "Running Clippy..."
    cargo clippy --all -- -D warnings
    log_ok "Clippy OK"
    exit 0
fi

# All mode - full CI check
if [[ "$MODE" == "all" ]]; then
    log_step "Checking formatting..."
    if cargo fmt --all -- --check 2>/dev/null; then
        log_ok "Format OK"
    else
        log_warn "Format issues found. Run: cargo fmt --all"
    fi

    log_step "Running Clippy..."
    cargo clippy --all -- -D warnings
    log_ok "Clippy OK"

    log_step "Building (debug)..."
    cargo build --all
    log_ok "Build OK"

    log_step "Running tests..."
    cargo test --all -- --nocapture --test-threads=1
    log_ok "Tests OK"

    log_step "Building (release)..."
    cargo build --all --release
    log_ok "Release build OK"

    END_TIME=$(date +%s)
    DURATION=$((END_TIME - START_TIME))
    echo ""
    log_ok "All checks passed in ${DURATION}s"
    exit 0
fi

# Quick mode - build only
if [[ "$MODE" == "quick" ]]; then
    log_step "Building ($BUILD_TYPE)..."
    if [[ "$BUILD_TYPE" == "release" ]]; then
        cargo build --all --release
    else
        cargo build --all
    fi
    log_ok "Build OK"

    END_TIME=$(date +%s)
    DURATION=$((END_TIME - START_TIME))
    echo ""
    log_ok "Build completed in ${DURATION}s"
    exit 0
fi

# Default mode - build + test
log_step "Building ($BUILD_TYPE)..."
if [[ "$BUILD_TYPE" == "release" ]]; then
    cargo build --all --release
else
    cargo build --all
fi
log_ok "Build OK"

log_step "Running tests..."
cargo test --all -- --nocapture --test-threads=1
log_ok "Tests OK"

END_TIME=$(date +%s)
DURATION=$((END_TIME - START_TIME))

echo ""
echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
log_ok "Build and test completed in ${DURATION}s"

# Show binary locations
if [[ "$BUILD_TYPE" == "release" ]]; then
    echo ""
    echo "Binaries:"
    echo "  ./target/release/sovd-server"
    echo "  ./target/release/example-ecu"
fi
