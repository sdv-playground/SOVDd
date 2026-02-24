---
name: ecu-config
description: ECU configuration and parameter setup for SOVD server. Use when creating or modifying ECU configurations, setting up DID definitions, configuring transport layers, or defining parameter mappings.
metadata:
  author: sovd-team
  version: "2.0"
---

# ECU Configuration

This skill helps with configuring ECUs for the SOVD server, including transport setup, DID definitions, and parameter mappings.

## Server Configuration (TOML)

The server binary (`sovdd`) takes a TOML config file as its argument.

### Minimal Single-ECU Config

```toml
[server]
host = "0.0.0.0"
port = 9080
request_timeout_ms = 5000

[transport]
type = "socketcan"
interface = "vcan0"

[transport.isotp]
tx_id = "0x18DA00F1"    # Tester -> ECU
rx_id = "0x18DAF100"    # ECU -> Tester
tx_padding = 0xCC
block_size = 0
st_min_us = 0

[session]
default_session = 0x01
extended_session = 0x03

[session.security]
enabled = true
level = 0x01

[session.keepalive]
enabled = true
interval_ms = 2000
suppress_response = true

[ecu.my_ecu]
id = "my_ecu"
name = "Engine Control Module"
```

### Multi-ECU Gateway Config

```toml
[server]
host = "0.0.0.0"
port = 4000

[gateway]
enabled = true
id = "vehicle_gateway"
name = "Vehicle Gateway"

[ecu.engine_ecu]
id = "engine_ecu"
name = "Engine ECU"

[ecu.engine_ecu.transport.isotp]
interface = "vcan1"
tx_id = "0x18DA10F1"
rx_id = "0x18DAF110"

[ecu.trans_ecu]
id = "trans_ecu"
name = "Transmission ECU"

[ecu.trans_ecu.transport.isotp]
interface = "vcan1"
tx_id = "0x18DA20F1"
rx_id = "0x18DAF120"
```

### Proxy ECU Config (for supplier containers)

```toml
[proxy.supplier_ecu]
id = "supplier_ecu"
name = "Supplier ECU"
upstream_url = "http://localhost:4002"
upstream_component = "vtx_vx500"
```

## Transport Configuration

### ISO-TP (CAN)

```toml
[ecu.my_ecu.transport.isotp]
interface = "vcan0"           # CAN interface
tx_id = "0x18DA00F1"          # Tester -> ECU (29-bit extended)
rx_id = "0x18DAF100"          # ECU -> Tester
tx_padding = 0xCC             # Padding byte (default: 0x00)
rx_padding = 0xCC
block_size = 0                # Flow control BS
st_min_us = 0                 # Flow control STmin (microseconds)
tx_dl = 8                     # CAN frame data length
```

### DoIP (Ethernet)

```toml
[ecu.my_ecu.transport.doip]
host = "192.168.1.100"        # Gateway IP
port = 13400                  # DoIP port (default)
logical_address = 0x0010      # ECU logical address
source_address = 0x0E80       # Tester address
# Optional TLS:
# tls = true
# ca_cert = "certs/ca.pem"
```

### Mock (no hardware)

```toml
[transport]
type = "mock"
```

## Inline Parameter Definitions

Parameters can be defined directly in the TOML config:

```toml
[[ecu.my_ecu.params]]
id = "vin"
name = "Vehicle Identification Number"
did = "0xF190"
data_type = "string"
byte_length = 17

[[ecu.my_ecu.params]]
id = "engine_rpm"
name = "Engine Speed"
did = "0xF40C"
data_type = "uint16"
unit = "rpm"
scale = 0.25
offset = 0
byte_length = 2
```

## Operations and Outputs

```toml
[[ecu.my_ecu.operations]]
id = "check_preconditions"
name = "Check Programming Preconditions"
rid = "0x0203"
description = "Verify ECU is ready for programming"
security_level = 0

[[ecu.my_ecu.outputs]]
id = "led_status"
name = "LED Status"
ioid = "0xF000"
default_value = "00"
data_type = "uint8"
```

## DID Definitions (YAML)

Rich DID definitions use YAML files loaded with `--did-definitions` or the admin API. Format used by `sovd-conv`:

```yaml
meta:
  name: Engine ECU
  version: "1.0"
  description: Engine control module DIDs

dids:
  # Scalar with scale/offset
  0xF405:
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
    unit: "°C"

  # Enum
  0xF404:
    name: Engine State
    type: uint8
    enum:
      0: "Off"
      1: "Cranking"
      2: "Running"

  # Bitfield
  0xF410:
    name: Engine Status Flags
    type: uint8
    bits:
      - name: engine_running
        bit: 0
      - name: mil_on
        bit: 1
      - name: cruise_active
        bit: 4
        width: 2    # multi-bit field

  # Array with labels
  0xF420:
    name: Wheel Speeds
    type: uint16
    scale: 0.01
    unit: km/h
    array: 4
    labels: [FL, FR, RL, RR]

  # 2D Map with axes
  0xF500:
    name: Fuel Injection Map
    type: uint16
    scale: 0.001
    unit: ms
    map:
      rows: 16
      cols: 16
      row_axis:
        name: RPM
        breakpoints: [800, 1200, 1600, 2000, 2500, 3000, 3500, 4000, 4500, 5000, 5500, 6000, 6500, 7000, 7500, 8000]
      col_axis:
        name: Load
        breakpoints: [0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150]

  # Histogram
  0xF600:
    name: RPM Operating Histogram
    type: uint32
    histogram:
      bins: [0, 1000, 2000, 3000, 4000, 5000, 6000, 7000, 8000]
```

### Uploading Definitions at Runtime

```bash
# Upload via admin API
curl -X POST http://localhost:9080/admin/definitions \
  -H "Content-Type: application/yaml" \
  --data-binary @config/did-definitions/engine_ecu.did.yaml

# List loaded definitions
curl http://localhost:9080/admin/definitions

# Delete a specific DID definition
curl -X DELETE http://localhost:9080/admin/definitions/0xF405
```

## Data Types

| Type | Size | Description |
|------|------|-------------|
| `uint8` | 1 byte | Unsigned 8-bit integer |
| `uint16` | 2 bytes | Unsigned 16-bit integer |
| `uint32` | 4 bytes | Unsigned 32-bit integer |
| `int8` | 1 byte | Signed 8-bit integer |
| `int16` | 2 bytes | Signed 16-bit integer |
| `int32` | 4 bytes | Signed 32-bit integer |
| `float32` | 4 bytes | IEEE 754 float |
| `float64` | 8 bytes | IEEE 754 double |
| `string` | variable | ASCII/UTF-8 text |
| `bytes` | variable | Raw hex bytes |

Scaling formula: `physical_value = (raw_value * scale) + offset`

## Service ID Overrides

Some ECUs use non-standard UDS service IDs:

```toml
[service_overrides]
ddid = 0xBA          # Instead of standard 0x2C
```

## Session and Security

```toml
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
```

## Flash Commit/Rollback

```toml
[flash_commit]
supports_rollback = true
commit_routine = "0xFF01"
rollback_routine = "0xFF02"
```

## Testing Connectivity

```bash
# Start server
./target/release/sovdd config/sovd-socketcan.toml

# Verify connectivity
curl http://localhost:9080/vehicle/v1/components

# Read VIN
curl http://localhost:9080/vehicle/v1/components/my_ecu/data/vin

# Read raw DID
curl http://localhost:9080/vehicle/v1/components/my_ecu/did/0xF190
```

## Troubleshooting

| Issue | Cause | Solution |
|-------|-------|----------|
| No response | Wrong CAN IDs | Verify TX/RX addressing match the ECU |
| Timeout | ECU not running | Check CAN bus with `candump` |
| NRC 0x11 | Service not supported | Verify UDS service IDs, check overrides |
| NRC 0x22 | Wrong session | Switch to extended/programming session |
| NRC 0x33 | Security required | Perform security access first |
| NRC 0x72 | Programming failure | Check flash state, retry |

## Example Configs

| Config | Description |
|--------|-------------|
| `config/sovd.toml` | Mock transport (no hardware) |
| `config/sovd-socketcan.toml` | Single ECU on vcan0 |
| `config/gateway-socketcan.toml` | Multi-ECU gateway on vcan0 |
| `config/gateway-dual-ecu.toml` | Dual ECU gateway |
| `config/example-ecu-standard.toml` | Standard example ECU config |
| `config/example-ecu-vortex.toml` | Vortex Motors example ECU config |
