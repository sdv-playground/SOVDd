#!/bin/bash
# Run end-to-end tests against example-ecu
#
# These tests:
# 1. Set up vcan0 interface
# 2. Start example-ecu simulator
# 3. Start sovd-server
# 4. Exercise the REST API
# 5. Verify responses contain real ECU data
#
# Prerequisites:
# - Binaries built (run ./build-and-test.sh --release first)
# - vcan kernel module available (sudo modprobe vcan)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Source cargo environment
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_step() { echo -e "\n${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"; echo -e "${BLUE}▶ $1${NC}"; }
log_ok() { echo -e "${GREEN}✓ $1${NC}"; }
log_warn() { echo -e "${YELLOW}! $1${NC}"; }
log_err() { echo -e "${RED}✗ $1${NC}"; }

# Check prerequisites
check_prerequisites() {
    log_step "Checking prerequisites..."

    # Check binaries exist
    if [[ ! -f "target/release/sovd-server" ]] && [[ ! -f "target/debug/sovd-server" ]]; then
        log_err "sovd-server not built. Run: ./build-and-test.sh --release"
        exit 1
    fi

    if [[ ! -f "target/release/example-ecu" ]] && [[ ! -f "target/debug/example-ecu" ]]; then
        log_err "example-ecu not built. Run: ./build-and-test.sh --release"
        exit 1
    fi

    # Check vcan module
    if ! modinfo vcan &>/dev/null; then
        log_warn "vcan kernel module not found. Tests may fail."
    fi

    log_ok "Prerequisites OK"
}

# Setup vcan interface
setup_vcan() {
    log_step "Setting up vcan0..."

    # Load vcan module
    if ! lsmod | grep -q vcan; then
        sudo modprobe vcan || true
    fi

    # Create interface if needed
    if ! ip link show vcan0 &>/dev/null; then
        sudo ip link add dev vcan0 type vcan
    fi

    # Bring up interface
    sudo ip link set up vcan0

    log_ok "vcan0 is ready"
}

# Run tests
run_tests() {
    log_step "Running e2e tests..."

    # Run tests with single thread to avoid port conflicts
    # Use --nocapture to see test output
    cargo test --test e2e_test -- --test-threads=1 --nocapture "$@"
}

# Parse arguments
FILTER=""
VERBOSE=""

while [[ $# -gt 0 ]]; do
    case $1 in
        -v|--verbose)
            VERBOSE="--nocapture"
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [options] [test_filter]"
            echo ""
            echo "Options:"
            echo "  -v, --verbose    Show test output"
            echo "  -h, --help       Show this help"
            echo ""
            echo "Examples:"
            echo "  $0                        # Run all e2e tests"
            echo "  $0 test_read_engine_rpm   # Run specific test"
            echo "  $0 -v test_list           # Run matching tests with output"
            exit 0
            ;;
        *)
            FILTER="$1"
            shift
            ;;
    esac
done

# Main
echo ""
echo "=========================================="
echo "  SOVD E2E Tests"
echo "=========================================="
echo ""

check_prerequisites
setup_vcan

if [[ -n "$FILTER" ]]; then
    run_tests "$FILTER" $VERBOSE
else
    run_tests $VERBOSE
fi

echo ""
log_ok "E2E tests completed"
