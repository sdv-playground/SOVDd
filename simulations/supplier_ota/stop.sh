#!/bin/bash
# =============================================================================
# Stop the supplier_ota simulation
#
# Reads the PID file written by start.sh and kills exactly those processes.
# Safe to run alongside unit tests — no process name sweeps.
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$(dirname "$SCRIPT_DIR")/lib/common.sh"

SIM_LOG_DIR="$SCRIPT_DIR/logs"

sim_stop
