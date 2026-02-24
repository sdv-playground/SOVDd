# Example ECU Simulator

A config-driven UDS ECU simulator for developing and testing the SOVD server without real hardware. Runs on Linux virtual CAN (vcan).

## UDS Services

| Service | ID | Description |
|---------|-----|-------------|
| DiagnosticSessionControl | 0x10 | Default (0x01), Programming (0x02), Extended (0x03) |
| ECUReset | 0x11 | Hard/soft reset, reverts to default session |
| ClearDiagnosticInformation | 0x14 | Clear DTCs (requires extended session) |
| ReadDTCInformation | 0x19 | Sub-functions 0x01, 0x02, 0x04, 0x06 |
| ReadDataByIdentifier | 0x22 | Single and batch reads |
| SecurityAccess | 0x27 | XOR-based seed/key |
| ReadDataByPeriodicIdentifier | 0x2A | 1Hz, 5Hz, 10Hz rates |
| DynamicallyDefineDataIdentifier | 0x2C | Define (0x01) and clear (0x03) |
| WriteDataByIdentifier | 0x2E | Writable DIDs only |
| IOControlByIdentifier | 0x2F | Return/reset/freeze/adjust |
| RoutineControl | 0x31 | Start/stop/result |
| RequestDownload | 0x34 | Flash download initiation |
| RequestUpload | 0x35 | Memory upload initiation |
| TransferData | 0x36 | Block transfer with sequence counter |
| RequestTransferExit | 0x37 | Finalize transfer |
| TesterPresent | 0x3E | With suppress response |
| ResponseOnEvent | 0x86 | Event-triggered responses |
| LinkControl | 0x87 | Baud rate transitions |

## Usage

```bash
# Default (vcan0, standard CAN IDs)
./target/release/example-ecu

# With TOML config
./target/release/example-ecu --config config/example-ecu-standard.toml

# Custom interface and CAN IDs
./target/release/example-ecu --interface vcan1 --rx-id 0x18DA10F1 --tx-id 0x18DAF110

# Custom security secret (hex)
./target/release/example-ecu --security-secret aa

# Verbose mode
./target/release/example-ecu -v
```

### CLI Arguments

| Argument | Default | Description |
|----------|---------|-------------|
| `-c, --config` | (none) | TOML config file path |
| `-i, --interface` | `vcan0` | CAN interface name |
| `--rx-id` | `0x18DA00F1` | ECU receive CAN ID (29-bit extended) |
| `--tx-id` | `0x18DAF100` | ECU transmit CAN ID |
| `--security-secret` | `ff` | Security access shared secret (hex) |
| `-v, --verbose` | off | Enable verbose logging |

## Default Configuration

When run without a config file, the ECU starts with a standard set of parameters, DTCs, routines, and I/O outputs defined in `src/config.rs`.

### Parameters (DIDs)

Dynamic values update periodically with small random variations.

| DID | Name | Type | Session | Security |
|-----|------|------|---------|----------|
| 0xF190 | VIN | string | Default | None |
| 0xF187 | Part Number | string | Default | None |
| 0xF18C | Serial Number | string | Default | None |
| 0xF40E | Vehicle Speed | uint8 | Default | None |
| 0xF404 | Engine Load | uint8 | Default | None |
| 0xF405 | Coolant Temperature | uint8 | Default | None |
| 0xF40C | Engine RPM | uint16 | Extended | None |
| 0xF48A | Oil Pressure | uint16 | Extended | None |
| 0xF40D | Fuel Rate | uint16 | Extended | None |
| 0xF42F | Boost Pressure | uint16 | Extended | Level 1 |
| 0xF478 | Exhaust Temperature | uint16 | Extended | Level 1 |
| 0xF411 | Throttle Position | uint8 | Extended | Level 1 |

Standard ISO 14229-1 identification DIDs (0xF180-0xF19F) are also registered.

### DTCs

| DTC Code | Bytes | Status | Description |
|----------|-------|--------|-------------|
| P0101 | `01 01 00` | 0x09 (active) | Mass Air Flow Circuit |
| P0300 | `03 00 00` | 0x24 (pending) | Random Cylinder Misfire |
| C0420 | `44 20 00` | 0x28 (historical) | Steering Angle Sensor |
| B1234 | `92 34 00` | 0x89 (active+MIL) | Airbag Warning Circuit |
| U0100 | `C1 00 00` | 0x28 (historical) | Lost Communication with ECM |

### Routines

| RID | Name | Session | Security | Description |
|-----|------|---------|----------|-------------|
| 0x0203 | Check Programming Preconditions | Extended | None | Returns 0x00 = success |
| 0xFF00 | Erase Memory | Programming | Level 1 | Simulated erase |
| 0xFF01 | Firmware Commit | Extended | Level 1 | Commit activated firmware (A/B bank) |
| 0xFF02 | Firmware Rollback | Extended | Level 1 | Rollback to previous firmware |

### I/O Outputs

| IOID | Name | Size | Security | Default |
|------|------|------|----------|---------|
| 0xF000 | LED Status | 1 byte | None | 0x00 |
| 0xF001 | Fan Speed | 2 bytes | None | 0x0000 |
| 0xF002 | Relay 1 | 1 byte | None | 0x00 |
| 0xF003 | Relay 2 | 1 byte | Level 1 | 0x00 |
| 0xF004 | PWM Output | 1 byte | None | 0x80 |

### Flash Simulation

The ECU simulates A/B bank firmware with configurable block counter behavior. Flash transfers use UDS 0x34/0x36/0x37 with support for commit (0xFF01) and rollback (0xFF02) routines. After ECU reset, session reverts to default and security re-locks per ISO 14229.

### Security

Default XOR algorithm: `key[i] = seed[i] ^ secret[i % secret.len()]`

Default secret: `0xFF` (single byte). Override with `--security-secret`.

## TOML Configuration

The ECU can be configured via TOML for custom transport settings and flash behavior. See `config/example-ecu-standard.toml` and `config/example-ecu-vortex.toml` for examples.

## License

Apache-2.0
