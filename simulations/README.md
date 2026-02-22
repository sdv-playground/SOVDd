# SOVDd Simulations

Reusable test networks for SOVDd development. Each subdirectory is a
self-contained simulation variant with its own configs and a `start.sh` that
sources the shared library.

Binaries are built automatically on start (debug builds, incremental).

## Quick start

```bash
# Start the 3-ECU network (builds sovdd + example-ecu first)
./simulations/basic_uds/start.sh

# Stop it (Ctrl+C in the terminal, or from another terminal)
./simulations/basic_uds/stop.sh
```

## Structure

```
simulations/
├── lib/
│   └── common.sh           # Shared functions (source from start scripts)
├── basic_uds/              # 3-ECU vehicle network (engine, transmission, body)
│   ├── config/             # ECU configs, gateway config, DID definitions
│   ├── start.sh            # Build + launch the simulation
│   ├── stop.sh             # Kill processes from a previous run
│   └── view-logs.sh        # Tail log files
└── README.md
```

## Available simulations

| Name | ECUs | Port | Interface | Description |
|------|------|------|-----------|-------------|
| `basic_uds` | 3 (engine, transmission, body) | 4000 | vcan1 | Standard UDS vehicle network |

## Creating a new variant

1. Copy `basic_uds/` to a new directory (e.g. `vortex_single/`)
2. Edit the configs in `config/` for your scenario
3. Edit `start.sh` — the shared lib handles the heavy lifting, you just
   call `sim_start_ecu` / `sim_start_server` with your configs

Minimal `start.sh` example:

```bash
#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$(dirname "$SCRIPT_DIR")/lib/common.sh"

CONFIG="$SCRIPT_DIR/config"
PORT=4001
SIM_LOG_DIR="$SCRIPT_DIR/logs"

sim_register_cleanup
sim_stop
sim_find_binaries --build
sim_setup_vcan vcan2

sim_start_ecu my_ecu "$CONFIG/ecu.toml"
sim_start_server "$PORT" "$CONFIG/gateway.toml" "$CONFIG/dids.yaml"

sim_print_status "$PORT"
sim_wait
```

## Shared library (`lib/common.sh`)

Source it from any start script. Functions available:

| Function | Purpose |
|----------|---------|
| `sim_find_binaries --build` | Build and locate debug binaries |
| `sim_setup_vcan <iface>` | Create and bring up a vcan interface |
| `sim_start_ecu <name> <config>` | Start an example-ecu process |
| `sim_start_server <port> <gw_config> [dids...]` | Start sovdd with health check |
| `sim_stop` | Kill processes from a previous run (via PID file) |
| `sim_register_cleanup` | Install Ctrl+C / exit trap |
| `sim_print_status <port>` | Print running process summary |
| `sim_wait` | Block until server dies or Ctrl+C |
| `sim_info/ok/warn/error` | Colored log output |

All ECU PIDs are tracked in the `SIM_PIDS` associative array and written to
a PID file (`logs/.pids`). Cleanup kills exactly those PIDs — safe to run
alongside unit tests on other vcan interfaces. Set `SIM_LOG_DIR` before
starting ECUs/server to capture logs to files.
