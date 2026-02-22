#!/bin/bash
# =============================================================================
# Shared simulation library for SOVDd test networks
#
# Source this file from simulation start scripts:
#   SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   source "$(dirname "$SCRIPT_DIR")/lib/common.sh"
#
# Provides:
#   Logging        - sim_info, sim_ok, sim_warn, sim_error
#   Binaries       - sim_find_binaries [--build]  (sets SERVER_BIN, ECU_BIN, HELPER_BIN, EXAMPLE_APP_BIN)
#   vCAN           - sim_setup_vcan <interface>
#   ECU mgmt       - sim_start_ecu <name> <config> [log_file]
#   Server mgmt    - sim_start_server <port> <gateway_config> [did_files...]
#   Helper mgmt    - sim_start_helper <port> <secrets_config> [token]
#   Health check   - sim_wait_ready <port> [max_attempts]
#   Process mgmt   - sim_register_cleanup, sim_cleanup, sim_wait
#   Status         - sim_print_status <port> [extra_lines...]
# =============================================================================

set -e

# -- Resolve project root (SOVDd) relative to this lib file ------------------
_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SIM_ROOT="$(dirname "$_LIB_DIR")"
PROJECT_DIR="$(dirname "$SIM_ROOT")"
# Security helper is a sibling project at the same level as SOVDd
HELPER_DIR="$(dirname "$PROJECT_DIR")/SOVD-security-helper"

# -- State --------------------------------------------------------------------
declare -A SIM_PIDS
SERVER_PID=""
HELPER_PID=""
HELPER_PORT=""
SIM_LOG_DIR=""
SIM_PID_FILE=""   # Written by start, read by stop. Defaults to $SIM_LOG_DIR/.pids
SERVER_BIN=""
ECU_BIN=""
HELPER_BIN=""
EXAMPLE_APP_BIN=""

# -- Colors -------------------------------------------------------------------
_RED='\033[0;31m'
_GREEN='\033[0;32m'
_YELLOW='\033[1;33m'
_BLUE='\033[0;34m'
_CYAN='\033[0;36m'
_NC='\033[0m'

# =============================================================================
# Logging
# =============================================================================

sim_info()  { echo -e "${_BLUE}[INFO]${_NC} $1"; }
sim_ok()    { echo -e "${_GREEN}[ OK ]${_NC} $1"; }
sim_warn()  { echo -e "${_YELLOW}[WARN]${_NC} $1"; }
sim_error() { echo -e "${_RED}[ERR ]${_NC} $1"; }

# =============================================================================
# Binary discovery
#
# Always uses debug builds from target/debug/.
# Pass --build to cargo build if binaries are missing.
# Sets: SERVER_BIN, ECU_BIN, HELPER_BIN, EXAMPLE_APP_BIN
#
# Security helper resolution order:
#   1. Local sibling checkout (../SOVD-security-helper) — for active development
#   2. cargo install from GitHub into target/tools/     — for normal use
# =============================================================================

HELPER_REPO="https://github.com/skarlsson/SOVD-security-helper"
HELPER_TOOLS_DIR="$PROJECT_DIR/target/tools"

sim_find_binaries() {
    local build_if_missing=false
    [[ "${1:-}" == "--build" ]] && build_if_missing=true

    SERVER_BIN="$PROJECT_DIR/target/debug/sovdd"
    ECU_BIN="$PROJECT_DIR/target/debug/example-ecu"
    EXAMPLE_APP_BIN="$PROJECT_DIR/target/debug/example-app"

    sim_info "Building sovdd + example-ecu + example-app (debug)..."
    (cd "$PROJECT_DIR" && cargo build -p sovdd -p example-ecu -p example-app)

    # Security helper: prefer local sibling checkout for development
    if [[ -d "$HELPER_DIR" ]]; then
        HELPER_BIN="$HELPER_DIR/target/debug/sovd-security-helper"
        sim_info "Building security helper (local)..."
        (cd "$HELPER_DIR" && cargo build)
    else
        # Install from GitHub into project-local tools directory
        HELPER_BIN="$HELPER_TOOLS_DIR/bin/sovd-security-helper"
        if [[ ! -x "$HELPER_BIN" ]]; then
            sim_info "Installing security helper from $HELPER_REPO..."
            cargo install --git "$HELPER_REPO" --root "$HELPER_TOOLS_DIR" 2>&1 \
                | tail -1 || {
                sim_warn "Failed to install security helper — security unlock won't work"
                HELPER_BIN=""
                sim_ok "Binaries: $SERVER_BIN"
                return 0
            }
        else
            sim_info "Security helper already installed"
        fi
    fi

    if [[ -n "$HELPER_BIN" ]]; then
        sim_ok "Binaries: $SERVER_BIN, $HELPER_BIN"
    fi
}

# =============================================================================
# PID file management
#
# _sim_pid_file     - resolves the PID file path
# _sim_write_pid    - appends a PID to the file
# _sim_remove_pids  - deletes the PID file
# sim_stop          - reads PID file, kills all listed processes
# =============================================================================

_sim_pid_file() {
    if [[ -n "$SIM_PID_FILE" ]]; then
        echo "$SIM_PID_FILE"
    elif [[ -n "$SIM_LOG_DIR" ]]; then
        echo "$SIM_LOG_DIR/.pids"
    else
        echo "/tmp/sovd-sim-$$.pids"
    fi
}

_sim_write_pid() {
    local pid="$1"
    local pidfile
    pidfile="$(_sim_pid_file)"
    mkdir -p "$(dirname "$pidfile")"
    echo "$pid" >> "$pidfile"
}

_sim_remove_pids() {
    local pidfile
    pidfile="$(_sim_pid_file)"
    rm -f "$pidfile"
}

sim_stop() {
    local pidfile
    pidfile="$(_sim_pid_file)"

    if [[ ! -f "$pidfile" ]]; then
        sim_warn "No PID file found at $pidfile — nothing to stop."
        return 0
    fi

    sim_info "Reading PIDs from $pidfile"

    # SIGTERM first
    while IFS= read -r pid; do
        [[ -z "$pid" ]] && continue
        if kill -0 "$pid" 2>/dev/null; then
            sim_info "Stopping PID $pid"
            kill "$pid" 2>/dev/null || true
        fi
    done < "$pidfile"

    sleep 1

    # SIGKILL stragglers
    while IFS= read -r pid; do
        [[ -z "$pid" ]] && continue
        if kill -0 "$pid" 2>/dev/null; then
            sim_warn "Force killing PID $pid"
            kill -9 "$pid" 2>/dev/null || true
        fi
    done < "$pidfile"

    rm -f "$pidfile"
    sim_ok "All processes stopped"
}

# =============================================================================
# Virtual CAN setup
#
# Usage: sim_setup_vcan <interface>
# =============================================================================

sim_setup_vcan() {
    local iface="${1:?Usage: sim_setup_vcan <interface>}"

    sim_info "Setting up virtual CAN: $iface"

    if ! lsmod | grep -q "^vcan"; then
        sudo modprobe vcan
    fi

    if ! ip link show "$iface" &>/dev/null; then
        sudo ip link add dev "$iface" type vcan
    fi

    if ! ip link show "$iface" | grep -q "UP"; then
        sudo ip link set up "$iface"
    fi

    sim_ok "$iface ready"
}

# =============================================================================
# ECU management
#
# Usage: sim_start_ecu <name> <config_path> [log_file]
#
# Starts an example-ecu process, tracks its PID in SIM_PIDS[<name>].
# Log defaults to $SIM_LOG_DIR/<name>.log if SIM_LOG_DIR is set.
# =============================================================================

sim_start_ecu() {
    local name="${1:?Usage: sim_start_ecu <name> <config>}"
    local config="${2:?Usage: sim_start_ecu <name> <config>}"
    local log_file="${3:-}"

    [[ -z "$ECU_BIN" ]] && { sim_error "Call sim_find_binaries first."; exit 1; }
    [[ ! -f "$config" ]] && { sim_error "ECU config not found: $config"; exit 1; }

    if [[ -z "$log_file" ]]; then
        if [[ -n "$SIM_LOG_DIR" ]]; then
            mkdir -p "$SIM_LOG_DIR"
            log_file="$SIM_LOG_DIR/$name.log"
        else
            log_file="/dev/null"
        fi
    fi

    sim_info "Starting ECU: $name"
    "$ECU_BIN" --config "$config" -v > "$log_file" 2>&1 &
    local pid=$!
    sleep 0.5

    if kill -0 "$pid" 2>/dev/null; then
        SIM_PIDS["$name"]=$pid
        _sim_write_pid "$pid"
        sim_ok "$name started (PID: $pid)"
    else
        sim_error "$name failed to start. Check: $log_file"
        exit 1
    fi
}

# =============================================================================
# Server management
#
# Usage: sim_start_server <port> <gateway_config> [did_file ...]
#
# Starts sovdd, waits for /health, sets SERVER_PID.
# =============================================================================

sim_start_server() {
    local port="${1:?Usage: sim_start_server <port> <gateway_config> [did_files...]}"
    local gateway_config="${2:?Usage: sim_start_server <port> <gateway_config> [did_files...]}"
    shift 2
    local did_files=("$@")

    [[ -z "$SERVER_BIN" ]] && { sim_error "Call sim_find_binaries first."; exit 1; }
    [[ ! -f "$gateway_config" ]] && { sim_error "Gateway config not found: $gateway_config"; exit 1; }

    local did_args=()
    for f in "${did_files[@]}"; do
        [[ ! -f "$f" ]] && { sim_warn "DID file not found, skipping: $f"; continue; }
        did_args+=("-d" "$f")
    done

    local log_file="/dev/null"
    if [[ -n "$SIM_LOG_DIR" ]]; then
        mkdir -p "$SIM_LOG_DIR"
        log_file="$SIM_LOG_DIR/sovd-server-${port}.log"
    fi

    sim_info "Starting SOVD server on port $port..."
    RUST_LOG=info "$SERVER_BIN" "${did_args[@]}" "$gateway_config" \
        > "$log_file" 2>&1 &
    SERVER_PID=$!

    _sim_write_pid "$SERVER_PID"
    SIM_PIDS["server_${port}"]=$SERVER_PID
    sim_wait_ready "$port" 30
    sim_ok "SOVD server started (PID: $SERVER_PID)"
}

# =============================================================================
# Example app management
#
# Usage: sim_start_example_app <port> <upstream_url> <upstream_component> [--upstream-gateway GW] [--auth-token TOKEN] [--config PATH] [log_file]
#
# Starts an example-app process that proxies SOVD requests to an upstream
# SOVD server with optional bearer token authentication and config file.
# When --upstream-gateway is specified, the component is accessed as a
# sub-entity of that gateway on the upstream server.
# Waits for /health before returning.
# =============================================================================

sim_start_example_app() {
    local port="${1:?Usage: sim_start_example_app <port> <upstream_url> <upstream_component> [--upstream-gateway GW] [--auth-token TOKEN] [--config PATH]}"
    local upstream_url="${2:?Usage: sim_start_example_app <port> <upstream_url> <upstream_component>}"
    local upstream_component="${3:?Usage: sim_start_example_app <port> <upstream_url> <upstream_component>}"
    shift 3

    local auth_token=""
    local config_path=""
    local upstream_gateway=""
    local log_file=""

    # Parse optional args
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --auth-token)
                auth_token="$2"
                shift 2
                ;;
            --config)
                config_path="$2"
                shift 2
                ;;
            --upstream-gateway|-g)
                upstream_gateway="$2"
                shift 2
                ;;
            *)
                log_file="$1"
                shift
                ;;
        esac
    done

    [[ -z "$EXAMPLE_APP_BIN" ]] && { sim_error "Call sim_find_binaries first."; exit 1; }

    if [[ -z "$log_file" ]]; then
        if [[ -n "$SIM_LOG_DIR" ]]; then
            mkdir -p "$SIM_LOG_DIR"
            log_file="$SIM_LOG_DIR/example-app.log"
        else
            log_file="/dev/null"
        fi
    fi

    local extra_args=()
    if [[ -n "$auth_token" ]]; then
        extra_args+=(--auth-token "$auth_token")
    fi
    if [[ -n "$config_path" ]]; then
        extra_args+=(--config "$config_path")
    fi
    if [[ -n "$upstream_gateway" ]]; then
        extra_args+=(--upstream-gateway "$upstream_gateway")
    fi

    if [[ -n "$auth_token" ]]; then
        sim_info "Starting example app on port $port -> $upstream_url (auth enabled)..."
    else
        sim_info "Starting example app on port $port -> $upstream_url..."
    fi

    RUST_LOG=info "$EXAMPLE_APP_BIN" \
        --port "$port" \
        --upstream-url "$upstream_url" \
        --upstream-component "$upstream_component" \
        "${extra_args[@]}" \
        > "$log_file" 2>&1 &
    local pid=$!

    _sim_write_pid "$pid"
    SIM_PIDS["example_app"]=$pid
    sim_wait_ready "$port" 30
    sim_ok "Example app started (PID: $pid, port: $port)"
}

# =============================================================================
# Security helper management
#
# Usage: sim_start_helper <port> <secrets_config> [token]
#
# Starts the SOVD security helper service for key derivation.
# Requires sim_find_binaries to have been called first.
# When token is omitted, auth mode is determined by the secrets config file.
# Pass a token to force static auth mode (e.g. for scripted tests).
# =============================================================================

sim_start_helper() {
    local port="${1:?Usage: sim_start_helper <port> <secrets_config> [token]}"
    local secrets_config="${2:?Usage: sim_start_helper <port> <secrets_config> [token]}"
    # local token="${3:-}"              # uncomment for OIDC (config-driven auth)
    local token="${3:-dev-secret-123}"

    if [[ -z "$HELPER_BIN" ]]; then
        sim_warn "Security helper binary not available — skipping."
        return 0
    fi

    [[ ! -f "$secrets_config" ]] && { sim_error "Secrets config not found: $secrets_config"; exit 1; }

    local log_file="/dev/null"
    if [[ -n "$SIM_LOG_DIR" ]]; then
        mkdir -p "$SIM_LOG_DIR"
        log_file="$SIM_LOG_DIR/security-helper.log"
    fi

    local helper_args=(--port "$port" --config "$secrets_config")
    if [[ -n "$token" ]]; then
        helper_args+=(--token "$token")
        sim_info "Starting security helper on port $port (static token)..."
    else
        sim_info "Starting security helper on port $port (config auth mode)..."
    fi
    "$HELPER_BIN" "${helper_args[@]}" \
        > "$log_file" 2>&1 &
    HELPER_PID=$!
    HELPER_PORT="$port"
    sleep 0.5

    if kill -0 "$HELPER_PID" 2>/dev/null; then
        SIM_PIDS["security_helper"]=$HELPER_PID
        _sim_write_pid "$HELPER_PID"
        sim_ok "Security helper started (PID: $HELPER_PID, port: $port)"
    else
        sim_error "Security helper failed to start. Check: $log_file"
        exit 1
    fi
}

# =============================================================================
# Health check
#
# Usage: sim_wait_ready <port> [max_attempts]
# Polls /health every 0.5s.
# =============================================================================

sim_wait_ready() {
    local port="${1:?}"
    local max="${2:-30}"
    local attempt=0

    sim_info "Waiting for server on port $port..."

    while [[ $attempt -lt $max ]]; do
        if curl -sf "http://localhost:$port/health" > /dev/null 2>&1; then
            return 0
        fi

        if [[ -n "$SERVER_PID" ]] && ! kill -0 "$SERVER_PID" 2>/dev/null; then
            sim_error "Server process died."
            [[ -n "$SIM_LOG_DIR" && -f "$SIM_LOG_DIR/sovd-server.log" ]] && \
                tail -20 "$SIM_LOG_DIR/sovd-server.log"
            exit 1
        fi

        sleep 0.5
        attempt=$((attempt + 1))
    done

    sim_error "Server not ready after $max attempts"
    exit 1
}

# =============================================================================
# Process cleanup
# =============================================================================

sim_register_cleanup() {
    trap sim_cleanup SIGINT SIGTERM EXIT
}

sim_cleanup() {
    echo ""
    sim_info "Shutting down simulation..."

    for name in "${!SIM_PIDS[@]}"; do
        local pid="${SIM_PIDS[$name]}"
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            sim_info "Stopping $name (PID: $pid)"
            kill "$pid" 2>/dev/null || true
        fi
    done

    if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
        sim_info "Stopping SOVD server (PID: $SERVER_PID)"
        kill "$SERVER_PID" 2>/dev/null || true
    fi

    if [[ -n "$HELPER_PID" ]] && kill -0 "$HELPER_PID" 2>/dev/null; then
        sim_info "Stopping security helper (PID: $HELPER_PID)"
        kill "$HELPER_PID" 2>/dev/null || true
    fi

    sleep 1

    # Force-kill stragglers
    for name in "${!SIM_PIDS[@]}"; do
        local pid="${SIM_PIDS[$name]}"
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            sim_warn "Force killing $name (PID: $pid)"
            kill -9 "$pid" 2>/dev/null || true
        fi
    done
    if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
        sim_warn "Force killing server (PID: $SERVER_PID)"
        kill -9 "$SERVER_PID" 2>/dev/null || true
    fi
    if [[ -n "$HELPER_PID" ]] && kill -0 "$HELPER_PID" 2>/dev/null; then
        sim_warn "Force killing helper (PID: $HELPER_PID)"
        kill -9 "$HELPER_PID" 2>/dev/null || true
    fi

    _sim_remove_pids
    sim_ok "Simulation stopped"
}

# =============================================================================
# Main loop - blocks, exits if server dies.
# =============================================================================

sim_wait() {
    while true; do
        sleep 2
        for name in "${!SIM_PIDS[@]}"; do
            local pid="${SIM_PIDS[$name]}"
            if [[ -n "$pid" ]] && ! kill -0 "$pid" 2>/dev/null; then
                sim_error "$name (PID: $pid) died unexpectedly"
                exit 1
            fi
        done
    done
}

# =============================================================================
# Status banner
#
# Usage: sim_print_status <port> [extra_lines...]
# =============================================================================

sim_print_status() {
    local port="${1:?}"
    shift

    echo ""
    echo "=============================================="
    echo -e "${_GREEN}Simulation Running${_NC}"
    echo "=============================================="
    echo ""
    echo "  SOVD Server:     http://localhost:$port"
    if [[ -n "$HELPER_PORT" ]]; then
        echo "  Security Helper: http://localhost:$HELPER_PORT"
    fi
    echo ""
    echo "  ECUs:"
    for name in "${!SIM_PIDS[@]}"; do
        [[ "$name" == "security_helper" ]] && continue
        printf "    %-24s PID %s\n" "$name" "${SIM_PIDS[$name]}"
    done
    echo ""

    for line in "$@"; do
        echo "  $line"
    done

    echo "  Test:"
    echo "    curl http://localhost:$port/health"
    echo "    curl http://localhost:$port/vehicle/v1/components"
    if [[ -n "$HELPER_PORT" ]]; then
        echo "    curl http://localhost:$HELPER_PORT/info"
    fi
    echo ""
    [[ -n "$SIM_LOG_DIR" ]] && echo "  Logs: $SIM_LOG_DIR/"
    echo ""
    echo "  Press Ctrl+C to stop"
    echo "=============================================="
}
