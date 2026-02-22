#!/bin/bash
# View simulation logs
# Usage: ./view-logs.sh [server|engine|transmission|body|all]

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOG_DIR="$SCRIPT_DIR/logs"

if [[ ! -d "$LOG_DIR" ]]; then
    echo "No logs directory. Start the simulation first."
    exit 1
fi

case "${1:-all}" in
    server)       tail -f "$LOG_DIR/sovd-server.log" ;;
    engine)       tail -f "$LOG_DIR/engine_ecu.log" ;;
    transmission) tail -f "$LOG_DIR/transmission_ecu.log" ;;
    body)         tail -f "$LOG_DIR/body_ecu.log" ;;
    all)          tail -f "$LOG_DIR"/*.log ;;
    *)
        echo "Usage: $0 [server|engine|transmission|body|all]"
        exit 1
        ;;
esac
