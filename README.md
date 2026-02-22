# SOVD Server for VTX ECM Data Collection

A Service-Oriented Vehicle Diagnostics (SOVD) server for reading and streaming data from a Vortex Motors VTX ECM over CAN bus.

## Overview

```
┌─────────────────┐      HTTP/SSE      ┌─────────────────┐      CAN/UDS      ┌─────────────────┐
│  Data Collection│ ←───────────────── │   SOVD Server   │ ←───────────────→ │    VTX ECM      │
│      App        │                    │                 │    (SocketCAN)    │  (or example-ecu)│
└─────────────────┘                    └─────────────────┘                   └─────────────────┘
```

## Features

- **SOVD REST API** - Standard endpoints for vehicle diagnostics
- **Real-time Streaming** - Server-Sent Events (SSE) for live data
- **Multi-client Support** - Multiple simultaneous subscriptions with smart multiplexing
- **Transport Adapters** - SocketCAN (direct) or SOME/IP (via gateway)
- **UDS Protocol** - Full implementation (0x10, 0x22, 0x27, 0x2A, 0x3E, 0x86)
- **Session Management** - Engineering session with tester present keepalive
- **Configuration-Driven** - All parameters loaded from TOML files
- **Example ECU Included** - Simulator for development without hardware

## Project Structure

```
sovd-server/
├── src/                         # SOVD Server source
│   ├── main.rs                  # Entry point
│   ├── api/                     # REST API (Axum)
│   │   ├── handlers/            # Request handlers
│   │   │   ├── components.rs    # ECU listing
│   │   │   ├── data.rs          # Parameter reads
│   │   │   ├── subscriptions.rs # Subscription CRUD
│   │   │   └── streams.rs       # SSE streaming
│   │   ├── models/              # Request/Response DTOs
│   │   └── router.rs            # Route definitions
│   ├── config/                  # Configuration loading
│   ├── ecu/                     # ECU registry & parameters
│   ├── session/                 # Session management
│   ├── subscription/            # Multi-client streaming
│   ├── transport/               # CAN/SOME-IP adapters
│   │   ├── socketcan/           # SocketCAN + ISO-TP
│   │   ├── someip/              # SOME/IP gateway
│   │   └── mock.rs              # For testing
│   └── uds/                     # UDS protocol
├── example-ecu/                 # ECU Simulator
│   └── src/
│       ├── main.rs              # Simulator entry point
│       ├── parameters.rs        # Simulated ECU state
│       └── uds.rs               # UDS handling
├── config/
│   ├── sovd.toml                # Default config (mock)
│   ├── sovd-socketcan.toml      # SocketCAN config
│   └── parameters/
│       └── vtx_ecm.toml         # VTX ECM parameters
├── scripts/
│   └── setup-vcan.sh            # Virtual CAN setup
└── docs/
    └── architecture.md          # Detailed architecture
```

## Quick Start

### Prerequisites

- Linux (for SocketCAN support)
- Rust 1.70+ (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- can-utils (`sudo apt install can-utils`)

### Build

```bash
cd /home/saka/BALI/SOVDd
cargo build --release
```

### Option A: Test with Simulator (No Hardware)

```bash
# Terminal 1: Set up virtual CAN
./scripts/setup-vcan.sh

# Terminal 2: Start example ECU
./target/release/example-ecu -v

# Terminal 3: Start SOVD server
./target/release/sovd-server config/sovd-socketcan.toml

# Terminal 4: Test the API
curl http://localhost:9080/vehicle/v1/components/vtx_ecm/data/engine_rpm
```

### Option B: Connect to Real ECU

1. Edit `config/sovd-socketcan.toml`:
   ```toml
   [transport.socketcan]
   interface = "can0"  # Your CAN interface

   [transport.socketcan.isotp]
   tx_id = "0x18DA00F1"  # Adjust for your ECU
   rx_id = "0x18DAF100"
   ```

2. Run the server:
   ```bash
   ./target/release/sovd-server config/sovd-socketcan.toml
   ```

---

## API Reference

### Components

#### List Components
```http
GET /vehicle/v1/components
```

Response:
```json
{
  "items": [
    {
      "id": "vtx_ecm",
      "name": "VTX ECM",
      "description": "Vortex Motors Engine Control Module",
      "href": "/vehicle/v1/components/vtx_ecm"
    }
  ]
}
```

#### Get Component Details
```http
GET /vehicle/v1/components/{component_id}
```

Response:
```json
{
  "id": "vtx_ecm",
  "name": "VTX ECM",
  "data": "/vehicle/v1/components/vtx_ecm/data",
  "subscriptions": "/vehicle/v1/components/vtx_ecm/subscriptions",
  "capabilities": {
    "read_data": true,
    "write_data": false,
    "periodic_identifier": true,
    "response_on_event": true
  }
}
```

### Parameters

#### List Parameters
```http
GET /vehicle/v1/components/{component_id}/data
```

Response:
```json
{
  "items": [
    {
      "id": "engine_rpm",
      "name": "Engine Speed",
      "did": "0xF40C",
      "unit": "rpm"
    },
    {
      "id": "coolant_temp",
      "name": "Engine Coolant Temperature",
      "did": "0xF405",
      "unit": "°C"
    }
  ]
}
```

#### Read Single Parameter
```http
GET /vehicle/v1/components/{component_id}/data/{param_id}
```

Response:
```json
{
  "id": "engine_rpm",
  "name": "Engine Speed",
  "value": 1850,
  "unit": "rpm",
  "timestamp": 1706550000000
}
```

### Subscriptions (Streaming)

#### Create Subscription
```http
POST /vehicle/v1/subscriptions
Content-Type: application/json

{
  "component_id": "vtx_ecm",
  "parameters": ["engine_rpm", "coolant_temp", "oil_pressure"],
  "rate_hz": 10,
  "mode": "periodic",
  "duration_secs": 3600
}
```

**Parameters:**
| Field | Type | Description |
|-------|------|-------------|
| `component_id` | string | ECU identifier |
| `parameters` | array | List of parameter IDs to stream |
| `rate_hz` | integer | Update rate (1, 5, or 10 Hz) |
| `mode` | string | `"periodic"` (0x2A), `"on_change"` (0x86), or `"polled"` |
| `duration_secs` | integer | Optional auto-expiry |

Response:
```json
{
  "subscription_id": "550e8400-e29b-41d4-a716-446655440000",
  "stream_url": "/vehicle/v1/streams/550e8400-e29b-41d4-a716-446655440000",
  "status": "active",
  "parameters": ["engine_rpm", "coolant_temp", "oil_pressure"],
  "rate_hz": 10,
  "mode": "periodic",
  "created_at": "2024-01-30T12:00:00Z"
}
```

#### List Subscriptions
```http
GET /vehicle/v1/subscriptions
```

#### Get Subscription
```http
GET /vehicle/v1/subscriptions/{subscription_id}
```

#### Delete Subscription
```http
DELETE /vehicle/v1/subscriptions/{subscription_id}
```

### Streams (SSE)

#### Connect to Stream
```http
GET /vehicle/v1/streams/{subscription_id}
Accept: text/event-stream
```

Response (Server-Sent Events):
```
data: {"ts":1706550000000,"seq":1,"engine_rpm":1850,"coolant_temp":92}

data: {"ts":1706550100000,"seq":2,"engine_rpm":1855,"coolant_temp":92}

data: {"ts":1706550200000,"seq":3,"engine_rpm":1848,"coolant_temp":93}
```

---

## Configuration

### Server Configuration (`config/sovd.toml`)

```toml
[server]
host = "0.0.0.0"
port = 8080
request_timeout_ms = 5000

# Transport: "socketcan", "someip", or "mock"
[transport]
type = "socketcan"

[transport.socketcan]
interface = "can0"
bitrate = 500000

[transport.socketcan.isotp]
tx_id = "0x18DA00F1"    # Tester -> ECU
rx_id = "0x18DAF100"    # ECU -> Tester
tx_padding = 0xCC
block_size = 0
st_min_us = 0

[session]
default_session = 0x01
extended_session = 0x03
engineering_session = 0x60

[session.security]
enabled = true
level = 0x01

[session.keepalive]
enabled = true
interval_ms = 2000
suppress_response = true

[subscriptions]
max_concurrent = 10
max_params_per_subscription = 50

[ecu.vtx_ecm]
id = "vtx_ecm"
name = "VTX ECM"

[ecu.vtx_ecm.capabilities]
read_data = true
periodic_identifier = true
max_periodic_dids = 16

[ecu.vtx_ecm.periodic_rates]
slow_hz = 1
medium_hz = 5
fast_hz = 10

[ecu.vtx_ecm.parameters]
config_file = "parameters/vtx_ecm.toml"
```

### Parameter Configuration (`config/parameters/vtx_ecm.toml`)

```toml
[[parameters]]
id = "engine_rpm"
name = "Engine Speed"
did = "0xF40C"
byte_length = 2
data_type = "uint16"
unit = "rpm"
scale = 0.25
offset = 0
min = 0
max = 8000
description = "Engine rotational speed"

[[parameters]]
id = "coolant_temp"
name = "Engine Coolant Temperature"
did = "0xF405"
byte_length = 1
data_type = "uint8"
unit = "°C"
scale = 1.0
offset = -40
min = -40
max = 215

# Parameter groups for convenience
[[parameter_groups]]
id = "engine_basics"
name = "Basic Engine Parameters"
parameters = ["engine_rpm", "coolant_temp", "oil_pressure"]
```

---

## Example ECU Simulator

The included example ECU simulates a VTX ECM for development without hardware.

### Features

| UDS Service | Code | Description |
|-------------|------|-------------|
| Diagnostic Session Control | 0x10 | Sessions: 0x01, 0x03, 0x60 |
| Read Data By ID | 0x22 | Read any configured parameter |
| Security Access | 0x27 | XOR-based seed/key |
| Periodic Identifier | 0x2A | 1Hz, 5Hz, 10Hz rates |
| Tester Present | 0x3E | With suppress response |
| ECU Reset | 0x11 | Resets session state |

### Simulated Parameters

| Parameter | DID | Range | Initial |
|-----------|-----|-------|---------|
| Engine RPM | 0xF40C | 0-8000 rpm | 1850 |
| Coolant Temp | 0xF405 | -40-215°C | 92 |
| Oil Pressure | 0xF48A | 0-1000 kPa | 450 |
| Fuel Rate | 0xF40D | 0-500 L/h | 30 |
| Vehicle Speed | 0xF40E | 0-255 km/h | 65 |
| Boost Pressure | 0xF42F | 0-500 kPa | 150 |
| Intake Temp | 0xF406 | -40-215°C | 35 |
| Exhaust Temp | 0xF478 | 0-1000°C | 450 |
| Throttle Position | 0xF411 | 0-100% | 30 |
| Engine Load | 0xF404 | 0-100% | 50 |

### Running the Simulator

```bash
# Basic usage
./target/release/example-ecu

# Custom interface
./target/release/example-ecu --interface vcan0

# Custom CAN IDs
./target/release/example-ecu --rx-id 0x18DA00F1 --tx-id 0x18DAF100

# Verbose mode
./target/release/example-ecu -v
```

---

## Multi-Client Streaming

The server supports multiple simultaneous clients with intelligent multiplexing:

```
Client A: [rpm, temp] @ 10Hz  ─┐
Client B: [rpm, oil]  @ 10Hz  ─┼─→ ECU: [rpm, temp, oil] @ 10Hz
Client C: [speed]     @ 1Hz   ─┘   + [speed] @ 1Hz
```

- Subscriptions are merged into optimal ECU configuration
- ECU sends data once per parameter
- Server fans out to each client based on their subscription
- Clients only receive their requested parameters

---

## UDS Protocol Support

| Service | ID | Description | Status |
|---------|-----|-------------|--------|
| DiagnosticSessionControl | 0x10 | Session management | ✅ |
| ECUReset | 0x11 | Reset ECU | ✅ |
| SecurityAccess | 0x27 | Unlock security | ✅ |
| TesterPresent | 0x3E | Keep session alive | ✅ |
| ReadDataByIdentifier | 0x22 | Single/batch reads | ✅ |
| WriteDataByIdentifier | 0x2E | Write parameters | ✅ |
| ReadDataByPeriodicIdentifier | 0x2A | Streaming data | ✅ |
| ResponseOnEvent | 0x86 | Event-triggered | ✅ |

---

## Monitoring & Debugging

### CAN Traffic

```bash
# Monitor all traffic
candump vcan0

# Monitor with timestamps
candump -t A vcan0

# Send manual UDS request (read RPM)
# Format: [CAN_ID]#[ISO-TP_header][UDS_data]
cansend vcan0 18DA00F1#03220CF4
```

### Server Logs

```bash
# Debug logging
RUST_LOG=debug ./target/release/sovd-server config/sovd.toml

# Specific module logging
RUST_LOG=sovd_server::subscription=debug ./target/release/sovd-server config/sovd.toml
```

---

## Extending

### Adding New Parameters

1. Add to `config/parameters/vtx_ecm.toml`:
```toml
[[parameters]]
id = "new_param"
name = "New Parameter"
did = "0xF4XX"
byte_length = 2
data_type = "uint16"
unit = "units"
scale = 1.0
offset = 0
```

2. If using example-ecu, add to `example-ecu/src/parameters.rs`:
```rust
parameters.insert(0xF4XX, Parameter::new("new_param", 0xF4XX, 2, 0, 65535, 1000));
```

### Adding New Transport

1. Create `src/transport/newtransport/adapter.rs`
2. Implement `TransportAdapter` trait
3. Add to `src/transport/mod.rs`
4. Add config type to `src/config/types.rs`

---

## License

Apache-2.0
