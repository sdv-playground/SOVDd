---
name: ecu-config
description: ECU configuration and parameter setup for SOVD server. Use when creating or modifying ECU configurations, setting up DID definitions, configuring transport layers, or defining parameter mappings.
metadata:
  author: sovd-team
  version: "1.0"
---

# ECU Configuration

This skill helps with configuring ECUs for the SOVD server, including transport setup, DID definitions, and parameter mappings.

## Configuration File Structure

The SOVD server uses TOML configuration files:

```toml
# Server configuration
[server]
host = "0.0.0.0"
port = 18080

# ECU definitions
[ecu.engine_ecu]
name = "Engine Control Module"
entity_type = "ecu"

[ecu.engine_ecu.transport.isotp]
interface = "vcan0"
tx_id = "0x18DA00F1"
rx_id = "0x18DAF100"

# DID definitions
[dids]
store_path = "config/dids.yaml"
```

## Transport Configuration

### ISO-TP (CAN)

```toml
[ecu.my_ecu.transport.isotp]
interface = "vcan0"           # CAN interface
tx_id = "0x18DA00F1"          # Tester -> ECU (29-bit extended)
rx_id = "0x18DAF100"          # ECU -> Tester
# Optional:
padding = 0xCC                # Padding byte (default: 0x00)
block_size = 0                # Flow control BS
st_min = 0                    # Flow control STmin (ms)
```

### DoIP (Ethernet)

```toml
[ecu.my_ecu.transport.doip]
host = "192.168.1.100"        # Gateway IP
port = 13400                  # DoIP port (default)
logical_address = 0x0010      # ECU logical address
source_address = 0x0E80       # Tester address
```

## DID Definitions (YAML)

DIDs are defined in a YAML file for parameter mapping:

```yaml
# config/dids.yaml
dids:
  # Standard identification DIDs
  - did: 0xF190
    name: vin
    description: Vehicle Identification Number
    access: public
    data_type: ascii
    length: 17

  - did: 0xF187
    name: part_number
    description: ECU Part Number
    access: public
    data_type: ascii
    length: 16

  # Operational parameters
  - did: 0x1000
    name: engine_speed
    description: Engine RPM
    access: extended
    data_type: uint16
    scaling:
      factor: 0.25
      offset: 0
      unit: rpm

  - did: 0x1001
    name: coolant_temp
    description: Coolant Temperature
    access: extended
    data_type: int8
    scaling:
      factor: 1
      offset: -40
      unit: "°C"

  # Protected parameters (require security)
  - did: 0x2000
    name: programming_date
    description: ECU Programming Date
    access: protected
    security_level: 1
    data_type: bcd
    length: 4
```

## Data Types

| Type | Description | Example |
|------|-------------|---------|
| `uint8` | Unsigned 8-bit | 0-255 |
| `uint16` | Unsigned 16-bit | 0-65535 |
| `uint32` | Unsigned 32-bit | 0-4294967295 |
| `int8` | Signed 8-bit | -128 to 127 |
| `int16` | Signed 16-bit | -32768 to 32767 |
| `int32` | Signed 32-bit | Full range |
| `float32` | IEEE 754 float | Decimal values |
| `ascii` | ASCII string | Text |
| `utf8` | UTF-8 string | Unicode text |
| `hex` | Raw hex bytes | Binary data |
| `bcd` | Binary-coded decimal | Dates, IDs |
| `bitmap` | Bit flags | Status bits |

## Access Levels

| Level | Session | Security | Description |
|-------|---------|----------|-------------|
| `public` | Default | None | Always readable |
| `extended` | Extended | None | Requires session change |
| `protected` | Extended | Level 1+ | Requires authentication |

## Scaling Configuration

For numeric parameters:
```yaml
scaling:
  factor: 0.25      # Multiply raw value
  offset: -40       # Add after factor
  unit: "rpm"       # Display unit
  min: 0            # Minimum valid value
  max: 8000         # Maximum valid value
```

Formula: `physical_value = (raw_value * factor) + offset`

## Creating a New ECU Configuration

1. **Identify transport parameters:**
   - For CAN: interface, TX/RX CAN IDs
   - For DoIP: gateway IP, logical address

2. **Create base config:**
```toml
[ecu.new_ecu]
name = "New ECU"
entity_type = "ecu"

[ecu.new_ecu.transport.isotp]
interface = "can0"
tx_id = "0x7E0"
rx_id = "0x7E8"
```

3. **Define DIDs:**
   - Start with standard IDs (0xF1xx)
   - Add operational parameters
   - Document access requirements

4. **Test communication:**
```bash
# Verify connectivity
curl http://localhost:18080/vehicle/v1/components/new_ecu

# Read VIN
curl http://localhost:18080/vehicle/v1/components/new_ecu/data/raw/0xF190
```

## Example: Complete ECU Config

See `config/test-config.toml` for a complete example with:
- ISO-TP transport setup
- Session timing parameters
- Service ID overrides
- Security configuration

## Dynamic DID Upload

DIDs can be uploaded at runtime:
```bash
curl -X POST http://localhost:18080/vehicle/v1/dids \
  -H "Content-Type: application/yaml" \
  --data-binary @config/dids.yaml
```

## Troubleshooting

| Issue | Cause | Solution |
|-------|-------|----------|
| No response | Wrong CAN IDs | Verify TX/RX addressing |
| Timeout | ECU not responding | Check CAN bus, interface |
| NRC 0x12 | Service not supported | Verify service IDs |
| NRC 0x22 | Wrong session | Switch to extended session |
| NRC 0x33 | Security required | Perform security access |
