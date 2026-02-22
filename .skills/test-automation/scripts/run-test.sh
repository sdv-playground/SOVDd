#!/bin/bash
# Helper script for running SOVD e2e tests
#
# Usage:
#   ./run-test.sh                    # Run all tests
#   ./run-test.sh test_read_vin      # Run specific test
#   ./run-test.sh dtc                # Run tests matching pattern
#   ./run-test.sh --debug test_name  # Run with debug logging

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Parse arguments
DEBUG=""
PATTERN=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --debug|-d)
            DEBUG="RUST_LOG=debug"
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [--debug] [test_pattern]"
            echo ""
            echo "Options:"
            echo "  --debug, -d    Enable debug logging"
            echo "  --help, -h     Show this help"
            echo ""
            echo "Examples:"
            echo "  $0                     Run all e2e tests"
            echo "  $0 test_read_vin       Run specific test"
            echo "  $0 dtc                 Run tests matching 'dtc'"
            echo "  $0 --debug session     Debug session tests"
            exit 0
            ;;
        *)
            PATTERN="$1"
            shift
            ;;
    esac
done

# Change to project root
cd "$(dirname "$0")/../../.."

# Check vcan0 interface
if ! ip link show vcan0 &>/dev/null; then
    echo -e "${YELLOW}Setting up vcan0 interface...${NC}"
    sudo modprobe vcan
    sudo ip link add dev vcan0 type vcan
    sudo ip link set up vcan0
fi

# Build
echo -e "${YELLOW}Building workspace...${NC}"
cargo build --workspace

# Run tests
echo -e "${YELLOW}Running tests...${NC}"
if [ -n "$PATTERN" ]; then
    echo -e "Pattern: ${GREEN}$PATTERN${NC}"
    if [ -n "$DEBUG" ]; then
        $DEBUG cargo test --test e2e_test "$PATTERN" -- --nocapture 2>&1 | tee /tmp/sovd-test.log
    else
        cargo test --test e2e_test "$PATTERN" -- --nocapture 2>&1 | tee /tmp/sovd-test.log
    fi
else
    if [ -n "$DEBUG" ]; then
        $DEBUG cargo test --test e2e_test -- --nocapture 2>&1 | tee /tmp/sovd-test.log
    else
        cargo test --test e2e_test 2>&1 | tee /tmp/sovd-test.log
    fi
fi

# Parse results
PASSED=$(grep -oP '\d+(?= passed)' /tmp/sovd-test.log | tail -1 || echo "0")
FAILED=$(grep -oP '\d+(?= failed)' /tmp/sovd-test.log | tail -1 || echo "0")

echo ""
echo "================================"
if [ "$FAILED" = "0" ]; then
    echo -e "${GREEN}All tests passed!${NC} ($PASSED tests)"
else
    echo -e "${RED}Tests failed!${NC}"
    echo -e "Passed: ${GREEN}$PASSED${NC}"
    echo -e "Failed: ${RED}$FAILED${NC}"
    echo ""
    echo "Failed tests:"
    grep "^    test_" /tmp/sovd-test.log || true
    exit 1
fi
