---
name: sovd-diagnostics
description: UDS diagnostic workflows for SOVD server. Use when working with diagnostic data, reading DIDs, managing DTCs, executing routines, controlling outputs, or troubleshooting ECU communication issues.
metadata:
  author: sovd-team
  version: "2.0"
---

# SOVD Diagnostics

This skill helps with UDS (Unified Diagnostic Services) diagnostic operations through the SOVD REST API.

## SOVD API Endpoints

All paths are under `/vehicle/v1/components/{id}/`.

### Data (DIDs)

| Method | Path | UDS | Description |
|--------|------|-----|-------------|
| GET | `/data` | — | List available parameters |
| GET | `/data/{param}` | 0x22 | Read parameter value |
| PUT | `/data/{param}` | 0x2E | Write parameter value |
| GET | `/did/{did}` | 0x22 | Read raw DID (hex) |
| PUT | `/did/{did}` | 0x2E | Write raw DID |

### Faults (DTCs)

| Method | Path | UDS | Description |
|--------|------|-----|-------------|
| GET | `/faults` | 0x19 | List DTCs with status |
| GET | `/faults/{fault_id}` | 0x19 | Get DTC details (snapshot, extended data) |
| DELETE | `/faults` | 0x14 | Clear DTCs (requires extended session) |
| GET | `/dtcs` | 0x19 | Active DTCs only |

### Operations (Routines)

| Method | Path | UDS | Description |
|--------|------|-----|-------------|
| GET | `/operations` | — | List available routines |
| POST | `/operations/{op}` | 0x31 | Execute routine |
| GET | `/operations/{op}/status/{exec_id}` | 0x31 | Get execution status |

### I/O Control (Outputs)

| Method | Path | UDS | Description |
|--------|------|-----|-------------|
| GET | `/outputs` | — | List I/O controls |
| GET | `/outputs/{out}` | — | Get output detail |
| POST | `/outputs/{out}` | 0x2F | Control output |

### Session/Security Modes

| Method | Path | UDS | Description |
|--------|------|-----|-------------|
| GET | `/modes/session` | 0x10 | Get current session |
| PUT | `/modes/session` | 0x10 | Change session |
| GET | `/modes/security` | 0x27 | Get security status |
| PUT | `/modes/security` | 0x27 | Request seed or send key |
| GET | `/modes/link` | 0x87 | Get link control state |
| PUT | `/modes/link` | 0x87 | Set link control |

### Other

| Method | Path | UDS | Description |
|--------|------|-----|-------------|
| POST | `/reset` | 0x11 | ECU reset |
| GET | `/apps` | — | List app sub-entities |
| POST | `/subscriptions` | 0x2A | Subscribe to data |
| GET | `/streams` | — | SSE data stream |

## Common Workflows

### Reading Diagnostic Data
```bash
BASE=http://localhost:9080/vehicle/v1/components/vtx_ecm

# List all parameters
curl "$BASE/data"

# Read specific parameter
curl "$BASE/data/engine_rpm"

# Read raw DID (hex bytes)
curl "$BASE/did/0xF190"

# Batch read (via query param)
curl "$BASE/data?ids=engine_rpm,coolant_temp"
```

### Managing DTCs
```bash
# Get all DTCs
curl "$BASE/faults"

# Get active DTCs
curl "$BASE/dtcs"

# Filter by category
curl "$BASE/faults?category=powertrain"

# Get DTC detail with snapshot data
curl "$BASE/faults/010100"

# Clear DTCs (requires extended session)
curl -X PUT "$BASE/modes/session" \
  -H "Content-Type: application/json" -d '{"value": "extended"}'
curl -X DELETE "$BASE/faults"
```

### I/O Control
```bash
# List outputs
curl "$BASE/outputs"

# Short-term adjustment
curl -X POST "$BASE/outputs/fan_speed" \
  -H "Content-Type: application/json" \
  -d '{"action": "adjust", "value": 50}'

# Return control to ECU
curl -X POST "$BASE/outputs/fan_speed" \
  -H "Content-Type: application/json" \
  -d '{"action": "return_to_ecu"}'
```

### Executing Routines
```bash
# List available routines
curl "$BASE/operations"

# Start a routine
curl -X POST "$BASE/operations/check_preconditions" \
  -H "Content-Type: application/json" -d '{}'
```

### Session and Security Access
```bash
# Change session
curl -X PUT "$BASE/modes/session" \
  -H "Content-Type: application/json" -d '{"value": "extended"}'

# Security access: request seed
curl -X PUT "$BASE/modes/security" \
  -H "Content-Type: application/json" \
  -d '{"value": "level1_requestseed"}'
# Response: { "state": "seed_sent", "seed": "a1b2c3..." }

# Security access: send key
curl -X PUT "$BASE/modes/security" \
  -H "Content-Type: application/json" \
  -d '{"value": "level1", "key": "hex_key_here"}'
```

## Session Requirements

| Operation | Required Session | Required Security |
|-----------|-----------------|-------------------|
| Read public DIDs | Default | None |
| Read extended DIDs | Extended | None |
| Read protected DIDs | Extended | Level 1+ |
| Write parameters | Extended | Varies |
| Clear DTCs | Extended | None |
| Execute routines | Extended | Varies |
| I/O control | Extended | Varies |
| Software transfer | Programming | Level 1+ |
| Commit/rollback | Extended | Level 1+ |

## Error Codes

| HTTP | UDS NRC | Meaning |
|------|---------|---------|
| 400 | 0x31 | Invalid parameters / request out of range |
| 403 | 0x33 | Security access required |
| 404 | — | Component/parameter/fault not found |
| 412 | 0x22 | Session requirement not met |
| 502 | various | Protocol error from ECU |
| 503 | — | Transport unavailable |
| 504 | — | Timeout |

See [references/uds-services.md](references/uds-services.md) for detailed UDS service documentation.
