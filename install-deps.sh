#!/bin/bash
# Install all dependencies for SOVD Server development
# Usage: ./install-deps.sh [options]
#
# Options:
#   --all        Install everything (default)
#   --system     Install system packages only
#   --rust       Install Rust toolchain only
#   --cargo      Install cargo tools only
#   --check      Check what's installed without installing

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

log_step() { echo -e "\n${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"; echo -e "${CYAN}▶ $1${NC}"; }
log_ok() { echo -e "${GREEN}✓${NC} $1"; }
log_missing() { echo -e "${RED}✗${NC} $1"; }
log_warn() { echo -e "${YELLOW}!${NC} $1"; }
log_info() { echo -e "${BLUE}i${NC} $1"; }

# Detect package manager
detect_package_manager() {
    if command -v apt-get &>/dev/null; then
        PKG_MGR="apt"
        PKG_INSTALL="sudo apt-get install -y"
        PKG_UPDATE="sudo apt-get update"
    elif command -v dnf &>/dev/null; then
        PKG_MGR="dnf"
        PKG_INSTALL="sudo dnf install -y"
        PKG_UPDATE="sudo dnf check-update || true"
    elif command -v pacman &>/dev/null; then
        PKG_MGR="pacman"
        PKG_INSTALL="sudo pacman -S --noconfirm"
        PKG_UPDATE="sudo pacman -Sy"
    elif command -v zypper &>/dev/null; then
        PKG_MGR="zypper"
        PKG_INSTALL="sudo zypper install -y"
        PKG_UPDATE="sudo zypper refresh"
    else
        PKG_MGR="unknown"
    fi
}

# Check if command exists
check_cmd() {
    command -v "$1" &>/dev/null
}

# Check status of all dependencies
check_status() {
    echo ""
    echo "Dependency Status"
    echo "================="
    echo ""

    echo "System packages:"
    check_cmd gcc && log_ok "gcc (C compiler)" || log_missing "gcc (C compiler)"
    check_cmd make && log_ok "make" || log_missing "make"
    check_cmd pkg-config && log_ok "pkg-config" || log_missing "pkg-config"
    check_cmd curl && log_ok "curl" || log_missing "curl"
    check_cmd git && log_ok "git" || log_missing "git"
    check_cmd jq && log_ok "jq (JSON processor)" || log_missing "jq (optional)"

    echo ""
    echo "CAN utilities:"
    check_cmd candump && log_ok "can-utils (candump, cansend)" || log_missing "can-utils"
    [[ -f /lib/modules/$(uname -r)/kernel/net/can/can.ko* ]] && log_ok "CAN kernel modules" || log_warn "CAN kernel modules (may need kernel headers)"

    echo ""
    echo "Rust toolchain:"
    if check_cmd rustc; then
        log_ok "rustc $(rustc --version | cut -d' ' -f2)"
    else
        log_missing "rustc"
    fi
    if check_cmd cargo; then
        log_ok "cargo $(cargo --version | cut -d' ' -f2)"
    else
        log_missing "cargo"
    fi
    check_cmd rustfmt && log_ok "rustfmt" || log_missing "rustfmt"
    check_cmd clippy-driver && log_ok "clippy" || log_missing "clippy"

    echo ""
    echo "Cargo tools (optional):"
    check_cmd cargo-watch && log_ok "cargo-watch" || log_missing "cargo-watch (optional)"
    check_cmd cargo-tarpaulin && log_ok "cargo-tarpaulin (coverage)" || log_missing "cargo-tarpaulin (optional)"

    echo ""
}

# Install system packages
install_system() {
    log_step "Installing system packages..."

    detect_package_manager

    if [[ "$PKG_MGR" == "unknown" ]]; then
        log_warn "Unknown package manager. Please install manually:"
        echo "  - build-essential (gcc, make)"
        echo "  - pkg-config"
        echo "  - curl"
        echo "  - git"
        echo "  - can-utils"
        echo "  - linux-headers (for CAN modules)"
        echo "  - jq (optional)"
        return 1
    fi

    log_info "Detected package manager: $PKG_MGR"

    # Update package lists
    log_info "Updating package lists..."
    $PKG_UPDATE

    case $PKG_MGR in
        apt)
            $PKG_INSTALL \
                build-essential \
                pkg-config \
                curl \
                git \
                can-utils \
                linux-headers-$(uname -r) \
                jq
            ;;
        dnf)
            $PKG_INSTALL \
                gcc \
                make \
                pkgconfig \
                curl \
                git \
                can-utils \
                kernel-devel \
                jq
            ;;
        pacman)
            $PKG_INSTALL \
                base-devel \
                pkgconf \
                curl \
                git \
                can-utils \
                linux-headers \
                jq
            ;;
        zypper)
            $PKG_INSTALL \
                gcc \
                make \
                pkg-config \
                curl \
                git \
                can-utils \
                kernel-devel \
                jq
            ;;
    esac

    log_ok "System packages installed"
}

# Install Rust toolchain
install_rust() {
    log_step "Installing Rust toolchain..."

    if check_cmd rustc; then
        local version=$(rustc --version | cut -d' ' -f2)
        log_info "Rust already installed: $version"

        # Check if version is recent enough (1.70+)
        local major=$(echo "$version" | cut -d'.' -f1)
        local minor=$(echo "$version" | cut -d'.' -f2)

        if [[ "$major" -eq 1 && "$minor" -lt 70 ]]; then
            log_warn "Rust version is old. Updating..."
            rustup update stable
        else
            log_ok "Rust version is sufficient"
        fi
    else
        log_info "Installing Rust via rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

        # Source cargo env for current session
        source "$HOME/.cargo/env"
    fi

    # Ensure components are installed
    log_info "Installing Rust components..."
    rustup component add rustfmt clippy

    log_ok "Rust toolchain ready"
    log_info "Rust: $(rustc --version)"
    log_info "Cargo: $(cargo --version)"
}

# Install cargo tools
install_cargo_tools() {
    log_step "Installing cargo tools..."

    # cargo-watch for auto-rebuild
    if check_cmd cargo-watch; then
        log_ok "cargo-watch already installed"
    else
        log_info "Installing cargo-watch..."
        cargo install cargo-watch
    fi

    # cargo-tarpaulin for coverage (optional, can be slow to compile)
    echo ""
    read -p "Install cargo-tarpaulin for code coverage? (takes a while to compile) [y/N] " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        if check_cmd cargo-tarpaulin; then
            log_ok "cargo-tarpaulin already installed"
        else
            log_info "Installing cargo-tarpaulin..."
            cargo install cargo-tarpaulin
        fi
    else
        log_info "Skipping cargo-tarpaulin"
    fi

    log_ok "Cargo tools installed"
}

# Load CAN kernel modules
setup_can_modules() {
    log_step "Setting up CAN kernel modules..."

    # Check if modules exist
    if ! modinfo can &>/dev/null; then
        log_warn "CAN kernel modules not found. You may need to install kernel headers."
        return 1
    fi

    # Load modules
    log_info "Loading CAN modules..."
    sudo modprobe can
    sudo modprobe can_raw
    sudo modprobe vcan

    log_ok "CAN modules loaded"

    # Show loaded modules
    lsmod | grep can
}

# Print post-install instructions
print_instructions() {
    echo ""
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${GREEN}Installation complete!${NC}"
    echo ""
    echo "Next steps:"
    echo ""
    echo "  1. Source cargo environment (if just installed):"
    echo "     source ~/.cargo/env"
    echo ""
    echo "  2. Build the project:"
    echo "     ./build-and-test.sh"
    echo ""
    echo "  3. Run the simulation:"
    echo "     ./example-deployment/run-simulation.sh"
    echo ""
    echo "  4. Or test manually:"
    echo "     ./scripts/setup-vcan.sh"
    echo "     ./target/release/example-ecu -v &"
    echo "     ./target/release/sovd-server config/sovd-socketcan.toml"
    echo ""
}

# Main
main() {
    echo ""
    echo "SOVD Server - Dependency Installer"
    echo "==================================="

    # Parse arguments
    MODE="${1:---all}"

    case $MODE in
        --check)
            check_status
            exit 0
            ;;
        --system)
            install_system
            ;;
        --rust)
            install_rust
            ;;
        --cargo)
            install_cargo_tools
            ;;
        --all|*)
            check_status
            echo ""
            read -p "Install missing dependencies? [Y/n] " -n 1 -r
            echo ""
            if [[ ! $REPLY =~ ^[Nn]$ ]]; then
                install_system
                install_rust
                install_cargo_tools
                setup_can_modules
                print_instructions
            fi
            ;;
        --help|-h)
            echo "Usage: $0 [options]"
            echo ""
            echo "Options:"
            echo "  --all        Install everything (default)"
            echo "  --system     Install system packages only"
            echo "  --rust       Install Rust toolchain only"
            echo "  --cargo      Install cargo tools only"
            echo "  --check      Check what's installed without installing"
            exit 0
            ;;
    esac
}

main "$@"
