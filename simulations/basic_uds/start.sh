#!/bin/bash
# =============================================================================
# basic_uds - 3-ECU vehicle network (Engine, Transmission, Body)
#
# Standard UDS over vcan1 with SOVD gateway on port 4000.
# Security helper on port 9100 for SOVD Explorer key derivation.
# Builds sovdd + example-ecu + security-helper automatically before starting.
# Usage: ./start.sh
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$(dirname "$SCRIPT_DIR")/lib/common.sh"

CONFIG="$SCRIPT_DIR/config"
PORT=4000
HELPER_PORT_NUM=9100
SIM_LOG_DIR="$SCRIPT_DIR/logs"

sim_register_cleanup
sim_stop              # clean up stale processes from a previous unclean exit
sim_find_binaries --build
sim_setup_vcan vcan1

sim_start_ecu engine_ecu       "$CONFIG/ecu-engine.toml"
sim_start_ecu transmission_ecu "$CONFIG/ecu-transmission.toml"
sim_start_ecu body_ecu         "$CONFIG/ecu-body.toml"

sim_start_helper "$HELPER_PORT_NUM" "$CONFIG/secrets.toml"

sim_start_server "$PORT" "$CONFIG/gateway.toml" \
    "$CONFIG/dids-engine.yaml" \
    "$CONFIG/dids-transmission.yaml" \
    "$CONFIG/dids-body.yaml"

sim_print_status "$PORT"
sim_wait
