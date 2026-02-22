#!/bin/bash
# =============================================================================
# supplier_ota - Supplier container reference architecture
#
# Architecture:
#   vehicle-gw (port 4000)  -- OEM central compute gateway (no CAN access)
#     ├── uds_gw (proxy) -> sovdd (port 4002) [UDS gateway, all ECUs]
#     │                        ├── engine_ecu      (CAN on vcan1)
#     │                        ├── transmission_ecu (CAN on vcan1)
#     │                        ├── body_ecu         (CAN on vcan1)
#     │                        └── vtx_vx500        (CAN on vcan1)
#     └── vortex_engine (proxy) -> example-app (port 4001)
#                                  └── proxy -> sovdd (port 4002) / vtx_vx500
#
# One UDS gateway owns the CAN bus and manages all ECUs.
# The supplier app entity (example-app) does NOT get direct CAN access —
# it reaches its ECU (vtx_vx500) through the shared UDS gateway via SOVD HTTP.
#
# Usage: ./start.sh
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$(dirname "$SCRIPT_DIR")/lib/common.sh"

CONFIG="$SCRIPT_DIR/config"
VEHICLE_GW_PORT=4000
SUPPLIER_GW_PORT=4001
UDS_GW_PORT=4002
HELPER_PORT_NUM=9100
SIM_LOG_DIR="$SCRIPT_DIR/logs"

sim_register_cleanup
sim_stop              # clean up stale processes from a previous unclean exit
sim_find_binaries --build
sim_setup_vcan vcan1

# -- Layer 1: Test ECUs on vCAN -----------------------------------------------
sim_start_ecu engine_ecu           "$CONFIG/ecu-engine.toml"
sim_start_ecu transmission_ecu     "$CONFIG/ecu-transmission.toml"
sim_start_ecu body_ecu             "$CONFIG/ecu-body.toml"
sim_start_ecu vtx_vx500  "$CONFIG/ecu-supplier-engine.toml"

# -- Security helper -----------------------------------------------------------
sim_start_helper "$HELPER_PORT_NUM" "$CONFIG/secrets.toml"

# -- Layer 2: UDS gateway (port 4002) - CAN access to ALL ECUs ----------------
sim_start_server "$UDS_GW_PORT" "$CONFIG/uds-gateway.toml" \
    "$CONFIG/dids-engine.yaml" \
    "$CONFIG/dids-transmission.yaml" \
    "$CONFIG/dids-body.yaml" \
    "$CONFIG/dids-vtx-vx500.yaml"

# -- Layer 3: Example app (port 4001) - supplier container --------------------
# Proxies to the UDS gateway for vtx_vx500. No direct CAN access.
sim_start_example_app "$SUPPLIER_GW_PORT" \
    "http://localhost:$UDS_GW_PORT" \
    "vtx_vx500" \
    --upstream-gateway "uds_gw" \
    --auth-token "supplier-secret-123" \
    --config "$CONFIG/example-app.toml"

# -- Layer 4: Vehicle gateway (port 4000) - pure SOVD HTTP aggregator ---------
# No DID files needed -- it's purely proxies, no direct CAN access.
sim_start_server "$VEHICLE_GW_PORT" "$CONFIG/vehicle-gateway.toml"

sim_print_status "$VEHICLE_GW_PORT" \
    "" \
    "Architecture:" \
    "  Vehicle GW:        http://localhost:$VEHICLE_GW_PORT  (SOVD HTTP aggregator)" \
    "  Example App:       http://localhost:$SUPPLIER_GW_PORT  (Vortex Motors app, auth: supplier-secret-123)" \
    "  UDS GW:            http://localhost:$UDS_GW_PORT  (engine, transmission, body, vtx_vx500)" \
    "" \
    "Try:" \
    "  # List vehicle gateway sub-components:" \
    "  curl http://localhost:$VEHICLE_GW_PORT/vehicle/v1/components" \
    "" \
    "  # List all ECUs directly:" \
    "  curl http://localhost:$UDS_GW_PORT/vehicle/v1/components" \
    "" \
    "  # List supplier app components (requires auth):" \
    "  curl -H 'Authorization: Bearer supplier-secret-123' http://localhost:$SUPPLIER_GW_PORT/vehicle/v1/components"

sim_wait
