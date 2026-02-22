# sovd-conv

DID conversion library for automotive diagnostics.

Converts between raw UDS bytes and human-readable values with support for scalars, arrays, 2D maps, enums, bitfields, and histograms.

## Features

- **Type-safe DIDs** - DIDs are `u16`, not strings
- **Precision-aware** - No ugly `13.000000001` values
- **YAML definitions** - Human-readable, like DBC for CAN
- **Rich data types** - Scalars, arrays, maps, enums, bitfields, histograms
- **Axis metadata** - Breakpoints for map visualization

## Quick Start

```rust
use sovd_conv::{DidStore, DidDefinition, DataType};
use serde_json::json;

// Load from YAML file
let store = DidStore::from_file("engine_ecu.did.yaml")?;

// Or build programmatically
let store = DidStore::new();
store.register(0xF405, DidDefinition::scaled(DataType::Uint8, 1.0, -40.0)
    .with_name("Coolant Temperature")
    .with_unit("°C"));

// Decode raw bytes
let value = store.decode(0xF405, &[0x84])?;
assert_eq!(value, json!(92));

// Encode physical value
let bytes = store.encode(0xF405, &json!(92))?;
assert_eq!(bytes, vec![0x84]);
```

## YAML Definition Format

```yaml
meta:
  name: Engine ECU
  version: "1.0"

dids:
  # Scalar with scale/offset
  0xF405:
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
    unit: °C

  # Enum
  0xF404:
    name: Gear Position
    type: uint8
    enum:
      0: P
      1: R
      2: N
      3: D

  # Bitfield
  0xF410:
    name: Engine Status
    type: uint8
    bits:
      - name: engine_running
        bit: 0
      - name: check_engine
        bit: 7

  # Array with labels
  0xF421:
    name: Wheel Speeds
    type: uint16
    scale: 0.01
    unit: km/h
    array: 4
    labels: [FL, FR, RL, RR]

  # 2D Map with axes
  0xF500:
    name: Fuel Map
    type: uint16
    scale: 0.001
    unit: ms
    map:
      rows: 16
      cols: 16
      row_axis:
        name: RPM
        breakpoints: [800, 1200, ...]
      col_axis:
        name: Load
        breakpoints: [0, 10, ...]
```

## Supported Types

| Type | Description |
|------|-------------|
| `uint8/16/32` | Unsigned integers |
| `int8/16/32` | Signed integers |
| `float32/64` | IEEE 754 floats |
| `string` | ASCII/UTF-8 |
| `bytes` | Raw hex |

## Deployment Modes

| Mode | Server | Client | Use Case |
|------|--------|--------|----------|
| Server-decode | DidStore | - | Simple setup |
| Dynamic | DidStore (uploaded) | DidStore | Multiple ECU types |
| Encrypted | - | DidStore | E2E encryption |
| Hybrid | Optional | DidStore | Gradual rollout |

## Documentation

See [docs/did-conversion.md](../../docs/did-conversion.md) for full documentation.

## License

MIT
