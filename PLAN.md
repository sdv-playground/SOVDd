# Plan: Phase 2 - Write Operations

## Overview
Add write operations support using UDS services 0x2E (WriteDataByIdentifier), 0x31 (RoutineControl), and 0x2C (DynamicallyDefineDataIdentifier).

## Design Principles
- Server is agnostic - passes requests through to ECU
- Example-ECU defines what's writable/executable via config
- ECU enforces access control (session/security requirements)

---

## API Endpoints (SOVD Standard)

| Endpoint | Method | UDS Service | Description |
|----------|--------|-------------|-------------|
| `/components/{id}/data/{param}` | PUT | 0x2E | Write parameter value |
| `/components/{id}/operations` | GET | — | List available operations |
| `/components/{id}/operations/{op}` | POST | 0x31 | Execute routine (start/stop/result) |
| `/components/{id}/data-definitions` | POST | 0x2C | Define dynamic data identifier |
| `/components/{id}/data-definitions/{ddid}` | DELETE | 0x2C | Clear dynamic definition |

---

## Implementation Phases

### Phase 2.1: WriteDataByIdentifier (0x2E)

**Modify: `src/uds/services.rs`**
- `write_data_by_id(did, data)` - already exists, verify implementation

**New file: `src/api/handlers/data_write.rs`** (or extend `data.rs`)
- `write_parameter()` - PUT handler

**Modify: `src/api/router.rs`**
- Add PUT method to `/data/{param}` route

**Modify: `example-ecu/src/parameters.rs`**
- Add `writable: bool` flag to Parameter struct
- Add 0x2E handler that checks writable flag
- Requires extended session (0x03) for writes

**Writable test parameters:**
| DID | Name | Access |
|-----|------|--------|
| 0xF199 | Programming Date | Extended session |
| 0xF19D | ECU Install Date | Extended + Security |

**Tests:**
- `test_write_parameter` - Write to writable DID
- `test_write_parameter_readonly` - Reject write to read-only DID
- `test_write_parameter_security_required` - Reject without security

---

### Phase 2.2: RoutineControl (0x31)

**New file: `src/uds/routine.rs`**
- Sub-function constants (0x01 start, 0x02 stop, 0x03 result)
- Routine ID handling
- Response parsing

**Modify: `src/uds/services.rs`**
- `routine_control_start(routine_id, params)`
- `routine_control_stop(routine_id)`
- `routine_control_result(routine_id)`

**New file: `src/api/handlers/operations.rs`**
- `list_operations()` - GET available routines
- `execute_operation()` - POST to start/stop/get result

**New file: `src/api/models/operations.rs`**
- `OperationRequest { action: start|stop|result, params }`
- `OperationResponse { status, result_data }`

**Modify: `src/api/router.rs`**
- Add `/operations` and `/operations/{op}` routes

**Modify: `example-ecu/src/parameters.rs`**
- Add routine handlers for:

| Routine ID | Name | Description |
|------------|------|-------------|
| 0x0203 | Check Programming Preconditions | Returns success/failure |
| 0xFF00 | Erase Memory | Simulated erase (requires security) |

**Tests:**
- `test_routine_start_stop` - Basic routine execution
- `test_routine_with_result` - Get routine result data
- `test_routine_security_required` - Reject without security

---

### Phase 2.3: DynamicallyDefineDataIdentifier (0x2C)

**New file: `src/uds/ddid.rs`**
- Sub-function constants (0x01 defineByIdentifier, 0x02 defineByMemory, 0x03 clear)
- DDID range handling (0xF200-0xF3FF typically)

**Modify: `src/uds/services.rs`**
- `define_data_identifier(ddid, source_dids[])`
- `clear_data_identifier(ddid)`

**New file: `src/api/handlers/data_definitions.rs`**
- `create_definition()` - POST to define DDID
- `delete_definition()` - DELETE to clear

**Modify: `example-ecu/src/parameters.rs`**
- Add DDID storage (HashMap<u16, Vec<u16>>)
- Handle 0x2C defineByIdentifier
- Handle 0x22 reads of defined DDIDs

**Tests:**
- `test_define_ddid` - Create dynamic identifier
- `test_read_ddid` - Read defined DDID returns combined data
- `test_clear_ddid` - Clear definition

---

## Files Summary

| File | Action |
|------|--------|
| `src/uds/routine.rs` | Create |
| `src/uds/ddid.rs` | Create |
| `src/uds/mod.rs` | Modify - export new modules |
| `src/uds/services.rs` | Modify - add service methods |
| `src/api/handlers/data.rs` | Modify - add PUT handler |
| `src/api/handlers/operations.rs` | Create |
| `src/api/handlers/data_definitions.rs` | Create |
| `src/api/handlers/mod.rs` | Modify - export new modules |
| `src/api/models/request.rs` | Modify - add operation/ddid requests |
| `src/api/models/response.rs` | Modify - add operation/ddid responses |
| `src/api/router.rs` | Modify - add routes |
| `example-ecu/src/parameters.rs` | Modify - add writable params, routines, DDIDs |
| `example-ecu/src/uds.rs` | Modify - add service constants |
| `tests/e2e_test.rs` | Modify - add new tests |
| `docs/api-reference.md` | Modify - document new endpoints |

---

## Test Matrix

| Test | Service | Scenario |
|------|---------|----------|
| `test_write_parameter` | 0x2E | Write writable param in extended session |
| `test_write_parameter_readonly` | 0x2E | Reject write to read-only (NRC 0x72) |
| `test_write_parameter_wrong_session` | 0x2E | Reject in default session (NRC 0x22) |
| `test_write_parameter_security` | 0x2E | Reject without security (NRC 0x33) |
| `test_routine_start` | 0x31 | Start routine, get ack |
| `test_routine_result` | 0x31 | Get routine result data |
| `test_routine_not_found` | 0x31 | Unknown routine (NRC 0x31) |
| `test_define_ddid` | 0x2C | Define DDID from source DIDs |
| `test_read_ddid` | 0x22 | Read defined DDID |
| `test_clear_ddid` | 0x2C | Clear DDID definition |

---

## Verification

```bash
# Build
cargo build

# Run tests
cargo test

# Manual test
curl -X PUT http://localhost:9080/vehicle/v1/components/vtx_ecm/data/programming_date \
  -H "Content-Type: application/json" \
  -d '{"value": "2024-01-30"}'

curl -X POST http://localhost:9080/vehicle/v1/components/vtx_ecm/operations/check_preconditions \
  -H "Content-Type: application/json" \
  -d '{"action": "start"}'
```
