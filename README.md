# SOVDd — Service-Oriented Vehicle Diagnostics Server

A Rust implementation of the [ASAM SOVD](https://www.asam.net/standards/detail/sovd/) standard. Translates SOVD REST API calls into UDS (Unified Diagnostic Services) commands for automotive ECU diagnostics over CAN/ISO-TP or DoIP transports.

```
┌───────────┐    HTTP/SSE    ┌───────────┐    CAN/UDS     ┌───────────┐
│  Client   │ ◄────────────► │   sovdd   │ ◄────────────► │    ECU    │
│ (CLI/GUI) │                │  (server) │   (SocketCAN   │ (physical │
└───────────┘                └───────────┘    or DoIP)     │  or sim)  │
                                  │                       └───────────┘
                                  │
                             ┌────┴────┐
                             │ Gateway │──► ECU 2, ECU 3, ...
                             └─────────┘
```

## Features

- **ASAM SOVD REST API** — standard `/vehicle/v1/` endpoints for data, faults, operations, I/O control, flash/OTA, and streaming
- **UDS Protocol** — services 0x10, 0x11, 0x19, 0x22, 0x27, 0x2A, 0x2E, 0x2F, 0x31, 0x34, 0x36, 0x37, 0x3E, 0x87
- **Transport Adapters** — SocketCAN (ISO-TP), DoIP (ISO 13400 over TCP/TLS), Mock (no hardware)
- **Multi-ECU Gateway** — federated routing across multiple ECUs with nested gateway support
- **App Entity Model** — supplier containers that proxy diagnostics and manage OTA for their ECUs
- **Real-time Streaming** — Server-Sent Events (SSE) with smart subscription multiplexing
- **Flash/OTA** — full 10-state lifecycle: upload, verify, transfer, activate, commit/rollback
- **DID Conversion** — YAML-driven encoding/decoding (scalar, array, map, histogram, bitfield, enum)
- **CLI Tool** — `sovd-cli` with table/JSON/CSV output for interactive diagnostics
- **Configuration-Driven** — TOML server config, YAML DID definitions

## Workspace

```
crates/
├── sovd-core       # DiagnosticBackend trait, models, errors
├── sovd-api        # Axum REST handlers, routing
├── sovd-uds        # UDS backend (SocketCAN, DoIP, Mock)
├── sovd-gateway    # Multi-ECU aggregation
├── sovd-conv       # DID encoding/decoding from YAML
├── sovd-client     # Typed HTTP client (SovdClient, FlashClient)
├── sovd-proxy      # HTTP proxy backend for supplier containers
├── sovdd           # Server binary
├── sovd-cli        # CLI tool
├── example-ecu     # ECU simulator (runs on vcan)
├── example-app     # Example supplier app entity
└── sovd-tests      # E2E integration tests
```

## Quick Start

### Build

```bash
cargo build --release
```

### Run with Mock Transport (no hardware)

```bash
./target/release/sovdd config/sovd.toml
```

```bash
# List ECUs
curl http://localhost:9080/vehicle/v1/components

# Read a parameter
curl http://localhost:9080/vehicle/v1/components/vtx_ecm/data/engine_rpm
```

### Run with SocketCAN

```bash
# Set up virtual CAN
sudo modprobe vcan
sudo ip link add dev vcan0 type vcan
sudo ip link set up vcan0

# Terminal 1: Start ECU simulator
./target/release/example-ecu

# Terminal 2: Start SOVD server
./target/release/sovdd config/sovd-socketcan.toml
```

### Multi-ECU Gateway

```bash
# Start 3 ECU simulators + gateway server
./simulations/basic_uds/start.sh
```

### CLI

```bash
# List components
sovd-cli --url http://localhost:9080 components

# Read data
sovd-cli --url http://localhost:9080 read vtx_ecm engine_rpm

# Monitor in real-time
sovd-cli --url http://localhost:9080 monitor vtx_ecm engine_rpm coolant_temp --rate 10
```

## Configuration

Server config is TOML, DID definitions are YAML.

| Config | Description |
|--------|-------------|
| `config/sovd.toml` | Mock transport (no hardware) |
| `config/sovd-socketcan.toml` | Single ECU on vcan0 |
| `config/gateway-socketcan.toml` | Multi-ECU gateway |
| `config/did-definitions/*.yaml` | DID encoding/decoding rules |

## Simulations

| Simulation | Description |
|------------|-------------|
| `simulations/basic_uds/` | 3 ECUs + gateway on vcan1 |
| `simulations/supplier_ota/` | 4-tier architecture: ECU + supplier app + OEM gateway + OTA |

## Testing

```bash
# Unit tests
cargo test --lib

# E2E tests (requires vcan0)
cargo test -p sovd-tests -- --test-threads=1

# Or use the script (sets up vcan + starts processes)
./run-e2e-tests.sh

# Full CI check
./build-and-test.sh --all
```

## License

Apache-2.0
