---
name: sovd-diagnostics
description: UDS diagnostic workflows for SOVD server. Use when working with diagnostic data, reading DIDs, managing DTCs, executing routines, or troubleshooting ECU communication issues.
metadata:
  author: sovd-team
  version: "1.0"
---

# SOVD Diagnostics

This skill helps with UDS (Unified Diagnostic Services) diagnostic operations through the SOVD REST API.

## SOVD API Endpoints

### Data (DIDs)
- `GET /vehicle/v1/components/{id}/data` - List available parameters
- `GET /vehicle/v1/components/{id}/data/{param}` - Read parameter value
- `PUT /vehicle/v1/components/{id}/data/{param}` - Write parameter value
- `GET /vehicle/v1/components/{id}/data/raw/{did}` - Read raw DID (hex)

### Faults (DTCs)
- `GET /vehicle/v1/components/{id}/faults` - List DTCs with status
- `GET /vehicle/v1/components/{id}/faults/{fault_id}` - Get DTC details
- `DELETE /vehicle/v1/components/{id}/faults` - Clear DTCs (requires extended session)

### Operations (Routines)
- `GET /vehicle/v1/components/{id}/operations` - List available routines
- `POST /vehicle/v1/components/{id}/operations/{op}/executions` - Start routine
- `GET /vehicle/v1/components/{id}/operations/{op}/executions/{exec_id}` - Get status
- `DELETE /vehicle/v1/components/{id}/operations/{op}/executions/{exec_id}` - Stop routine

### Session/Security Modes
- `GET /vehicle/v1/components/{id}/modes/session` - Get current session
- `PUT /vehicle/v1/components/{id}/modes/session` - Change session (default/extended/programming)
- `GET /vehicle/v1/components/{id}/modes/security` - Get security status
- `PUT /vehicle/v1/components/{id}/modes/security` - Request seed or send key

## Common Workflows

### Reading Diagnostic Data
```bash
# List all parameters
curl http://localhost:18080/vehicle/v1/components/vtx_ecm/data

# Read specific parameter
curl http://localhost:18080/vehicle/v1/components/vtx_ecm/data/engine_speed

# Read raw DID
curl http://localhost:18080/vehicle/v1/components/vtx_ecm/data/raw/0xF190
```

### Managing DTCs
```bash
# Get all DTCs
curl http://localhost:18080/vehicle/v1/components/vtx_ecm/faults

# Get active DTCs only
curl "http://localhost:18080/vehicle/v1/components/vtx_ecm/faults?status=active"

# Clear DTCs (requires extended session first)
curl -X PUT http://localhost:18080/vehicle/v1/components/vtx_ecm/modes/session \
  -H "Content-Type: application/json" -d '{"value": "extended"}'
curl -X DELETE http://localhost:18080/vehicle/v1/components/vtx_ecm/faults
```

### Executing Routines
```bash
# List available routines
curl http://localhost:18080/vehicle/v1/components/vtx_ecm/operations

# Start a routine
curl -X POST http://localhost:18080/vehicle/v1/components/vtx_ecm/operations/self_test/executions \
  -H "Content-Type: application/json" -d '{}'

# Check routine status
curl http://localhost:18080/vehicle/v1/components/vtx_ecm/operations/self_test/executions/{exec_id}
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
| Software transfer | Programming | Level 1+ |

## UDS Service IDs Reference

| Service | ID | Description |
|---------|-----|-------------|
| DiagnosticSessionControl | 0x10 | Change session |
| ECUReset | 0x11 | Reset ECU |
| SecurityAccess | 0x27 | Seed/key authentication |
| ReadDataByIdentifier | 0x22 | Read DIDs |
| WriteDataByIdentifier | 0x2E | Write DIDs |
| RoutineControl | 0x31 | Start/stop/get result |
| RequestDownload | 0x34 | Start download |
| RequestUpload | 0x35 | Start upload |
| TransferData | 0x36 | Transfer block |
| RequestTransferExit | 0x37 | End transfer |
| ReadDTCInformation | 0x19 | Read DTCs |
| ClearDiagnosticInformation | 0x14 | Clear DTCs |
| InputOutputControlByIdentifier | 0x2F | I/O control |

## Error Codes

| HTTP | Meaning |
|------|---------|
| 400 | Bad request (invalid parameters) |
| 403 | Security access required |
| 404 | Component/parameter not found |
| 412 | Session requirement not met |
| 502 | Protocol error from ECU |
| 503 | Transport unavailable |
| 504 | Timeout |

See [references/uds-services.md](references/uds-services.md) for detailed UDS service documentation.
