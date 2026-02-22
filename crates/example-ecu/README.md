# Example ECU Simulator

A simulated VTX ECM for testing the SOVD server without real hardware.

## Features

- **UDS Protocol Support**:
  - 0x10 Diagnostic Session Control (Default, Extended, Engineering)
  - 0x14 Clear Diagnostic Information (requires extended session)
  - 0x19 Read DTC Information (sub-functions 0x01, 0x02, 0x04, 0x06)
  - 0x22 Read Data By Identifier
  - 0x27 Security Access (with simple XOR key)
  - 0x2A Read Data By Periodic Identifier
  - 0x2C Dynamically Define Data Identifier (sub-functions 0x01, 0x03)
  - 0x2E Write Data By Identifier (writable DIDs only)
  - 0x31 Routine Control (sub-functions 0x01, 0x02, 0x03)
  - 0x3E Tester Present

- **Simulated Parameters** (matching `vtx_ecm.toml`):
  - Engine RPM (0xF40C)
  - Coolant Temperature (0xF405)
  - Oil Pressure (0xF48A)
  - Fuel Rate (0xF40D)
  - Vehicle Speed (0xF40E)
  - Boost Pressure (0xF42F)
  - Intake Temperature (0xF406)
  - Exhaust Temperature (0xF478)
  - Throttle Position (0xF411)
  - Engine Load (0xF404)

- **Realistic Simulation**:
  - Values change randomly within bounds
  - Periodic transmission at configured rates (1Hz, 5Hz, 10Hz)

## Building

```bash
cd example-ecu
cargo build --release
```

## Usage

### 1. Set up Virtual CAN (vcan0)

```bash
# Load the vcan kernel module
sudo modprobe vcan

# Create vcan0 interface
sudo ip link add dev vcan0 type vcan
sudo ip link set up vcan0

# Verify
ip link show vcan0
```

### 2. Run the Example ECU

```bash
# Default settings (vcan0)
./target/release/example-ecu

# Custom interface and IDs
./target/release/example-ecu --interface vcan0 \
    --rx-id 0x18DA00F1 \
    --tx-id 0x18DAF100

# Verbose mode
./target/release/example-ecu -v
```

### 3. Run the SOVD Server (in another terminal)

Update `config/sovd.toml` to use SocketCAN:

```toml
[transport]
type = "socketcan"

[transport.socketcan]
interface = "vcan0"

[transport.socketcan.isotp]
tx_id = "0x18DA00F1"  # Server sends to ECU
rx_id = "0x18DAF100"  # Server receives from ECU
```

Then run:
```bash
./target/release/sovd-server config/sovd.toml
```

### 4. Test with curl

```bash
# List components
curl http://localhost:9080/vehicle/v1/components

# List parameters
curl http://localhost:9080/vehicle/v1/components/vtx_ecm/data

# Read a single parameter
curl http://localhost:9080/vehicle/v1/components/vtx_ecm/data/engine_rpm

# Create a streaming subscription
curl -X POST http://localhost:9080/vehicle/v1/subscriptions \
  -H "Content-Type: application/json" \
  -d '{
    "component_id": "vtx_ecm",
    "parameters": ["engine_rpm", "coolant_temp"],
    "rate_hz": 10,
    "mode": "periodic"
  }'

# Connect to the stream
curl -N http://localhost:9080/vehicle/v1/streams/{subscription_id}
```

### 5. Test DTC/Fault Endpoints

```bash
# List all stored faults
curl http://localhost:9080/vehicle/v1/components/vtx_ecm/faults

# List active faults only (currently failing)
curl http://localhost:9080/vehicle/v1/components/vtx_ecm/dtcs

# Get detail for a specific DTC
curl http://localhost:9080/vehicle/v1/components/vtx_ecm/faults/010100

# Filter faults by category
curl "http://localhost:9080/vehicle/v1/components/vtx_ecm/faults?category=powertrain"

# Clear faults (requires extended session first)
curl -X PUT http://localhost:9080/vehicle/v1/components/vtx_ecm/modes/session \
  -H "Content-Type: application/json" \
  -d '{"value": "extended"}'

curl -X DELETE http://localhost:9080/vehicle/v1/components/vtx_ecm/faults
```

## CAN ID Configuration

The default configuration uses ISO-TP addressing:

| Direction | CAN ID | Description |
|-----------|--------|-------------|
| Tester → ECU | 0x18DA00F1 | Server sends requests |
| ECU → Tester | 0x18DAF100 | ECU sends responses |

These are 29-bit extended CAN IDs following J1939 conventions.

## Monitoring CAN Traffic

```bash
# Install can-utils if needed
sudo apt install can-utils

# Monitor all CAN traffic
candump vcan0

# Monitor with decode
candump -t A vcan0

# Send a test message (read RPM)
cansend vcan0 18DA00F1#03220CF4
```

## Architecture

```
┌─────────────────┐      vcan0        ┌─────────────────┐
│   SOVD Server   │ ←───────────────→ │   Example ECU   │
│                 │   ISO-TP/CAN      │                 │
│ TX: 0x18DA00F1  │                   │ RX: 0x18DA00F1  │
│ RX: 0x18DAF100  │                   │ TX: 0x18DAF100  │
└─────────────────┘                   └─────────────────┘
```

## Simulated Behavior

### Parameter Values

| Parameter | Range | Initial | Update Rate |
|-----------|-------|---------|-------------|
| Engine RPM | 0-8000 | 1850 | Varies ±2% |
| Coolant Temp | -40-215°C | 92°C | Varies ±2% |
| Oil Pressure | 0-1000 kPa | 450 kPa | Varies ±2% |
| Vehicle Speed | 0-255 km/h | 65 km/h | Varies ±2% |

Values update every 500ms with small random variations to simulate realistic sensor behavior.

### Simulated DTCs (Fault Codes)

The example ECU includes 5 pre-configured DTCs with different status conditions:

| DTC Code | Bytes | Status | Description |
|----------|-------|--------|-------------|
| **P0101** | `01 01 00` | `0x09` (active) | Mass Air Flow Circuit - test_failed + confirmed |
| **P0300** | `03 00 00` | `0x24` (pending) | Random Misfire - pending + test_failed_since_clear |
| **C0420** | `44 20 00` | `0x28` (historical) | Steering Sensor - confirmed + test_failed_since_clear |
| **B1234** | `92 34 00` | `0x89` (active+MIL) | Airbag Circuit - test_failed + confirmed + warning_indicator |
| **U0100** | `C1 00 00` | `0x28` (historical) | Lost Communication - confirmed + test_failed_since_clear |

**DTC Status Bits (ISO 14229-1):**
- Bit 0 (0x01): testFailed - Currently failing
- Bit 2 (0x04): pendingDTC - Failed but not yet confirmed
- Bit 3 (0x08): confirmedDTC - Malfunction confirmed and stored
- Bit 5 (0x20): testFailedSinceLastClear - Failed since last clear
- Bit 7 (0x80): warningIndicatorRequested - MIL lamp requested

**Snapshot Data:** P0101 includes freeze frame data (coolant temp, vehicle speed)
**Extended Data:** P0101 and C0420 include occurrence counters and aging data

DTCs are static and do not change during runtime. They can be cleared via UDS service 0x14 (requires extended session 0x03), but will not reappear until the ECU is restarted.

### Access Levels

Parameters have different access requirements:

| Level | Session | Security | Parameters |
|-------|---------|----------|------------|
| Public | Default (0x01) | None | VIN, part numbers, vehicle speed, coolant temp, engine load |
| Extended | Extended (0x03) | None | Engine RPM, oil pressure, fuel rate, intake temp |
| Protected | Extended (0x03) | Level 1 | Boost pressure, exhaust temp, throttle position |

DTC clearing requires Extended session (0x03).

### Writable Parameters

| DID | Name | Access Required |
|-----|------|-----------------|
| 0xF199 | Programming Date | Extended session |
| 0xF19D | Installation Date | Extended + Security |

Writable parameters can be updated via UDS 0x2E WriteDataByIdentifier. The ECU enforces access control - read-only parameters return NRC 0x72 (GeneralProgrammingFailure).

### Supported Routines (0x31)

| Routine ID | Name | Access Required | Description |
|------------|------|-----------------|-------------|
| 0x0203 | Check Programming Preconditions | None | Returns 0x00 = success |
| 0xFF00 | Erase Memory | Security Level 1 | Simulated erase operation |

### Dynamic Data Identifiers (0x2C)

DDIDs in the range 0xF200-0xF3FF can be defined dynamically. Use:
- Sub-function 0x01: Define by identifier (compose from source DIDs)
- Sub-function 0x03: Clear DDID definition

Defined DDIDs can be read via normal 0x22 ReadDataByIdentifier requests.
