---
name: test-automation
description: Running and interpreting SOVD e2e tests. Use when running tests, debugging test failures, adding new tests, or validating SOVD server functionality.
metadata:
  author: sovd-team
  version: "1.0"
---

# Test Automation

This skill covers running, interpreting, and creating tests for the SOVD server.

## Test Architecture

```
crates/sovd-tests/
├── tests/
│   └── e2e_test.rs      # Main e2e test file
└── src/
    └── lib.rs           # Test utilities

crates/example-ecu/          # ECU simulator for testing
├── src/
│   ├── main.rs          # ECU simulator entry point
│   ├── config.rs        # ECU configuration
│   └── parameters.rs    # Simulated DIDs, DTCs, routines
```

## Running Tests

### All E2E Tests
```bash
cargo test --test e2e_test
```

### Specific Test
```bash
cargo test --test e2e_test test_read_vin -- --nocapture
```

### With Debug Output
```bash
RUST_LOG=debug cargo test --test e2e_test test_name -- --nocapture
```

### Filtered Tests
```bash
# Run all DTC tests
cargo test --test e2e_test dtc

# Run all session tests
cargo test --test e2e_test session

# Run all security tests
cargo test --test e2e_test security
```

## Test Categories

| Category | Pattern | Description |
|----------|---------|-------------|
| Data | `test_read_*`, `test_write_*` | DID read/write |
| Faults | `test_*fault*`, `test_*dtc*` | DTC operations |
| Operations | `test_*routine*`, `test_*operation*` | Routine control |
| Session | `test_*session*` | Session management |
| Security | `test_*security*` | Authentication |
| Software | `test_*download*`, `test_*upload*` | Flash operations |
| Discovery | `test_*discovery*` | ECU discovery |
| Streaming | `test_*stream*`, `test_*sse*` | Real-time data |

## Test Harness

The `TestHarness` manages test infrastructure:

```rust
let harness = TestHarness::new().await?;

// Make requests
let (status, json) = harness.get("/vehicle/v1/components").await?;
let (status, json) = harness.post("/path", json_body).await?;
let (status, json) = harness.put("/path", json_body).await?;
let status = harness.delete("/path").await?;
```

### Harness Setup
- Starts `example-ecu` simulator on vcan0
- Starts `sovdd` server on port 18080
- Uploads test DID definitions
- Provides HTTP client methods

## Writing New Tests

### Basic Test Template
```rust
#[tokio::test]
#[serial_test::serial]
async fn test_my_feature() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // Test implementation
    let (status, json) = harness.get("/vehicle/v1/components/vtx_ecm/data/vin")
        .await
        .expect("Request failed");

    assert_eq!(status, 200);
    assert!(json["value"].is_string());
}
```

### Test with Session Setup
```rust
#[tokio::test]
#[serial_test::serial]
async fn test_extended_session_feature() {
    let harness = TestHarness::new().await.expect("Harness failed");

    // Switch to extended session
    let body = serde_json::json!({"value": "extended"});
    let (status, _) = harness.put("/vehicle/v1/components/vtx_ecm/modes/session", body)
        .await
        .expect("Session change failed");
    assert_eq!(status, 200);

    // Now test extended-session feature
    // ...
}
```

### Test with Security Setup
```rust
#[tokio::test]
#[serial_test::serial]
async fn test_protected_feature() {
    let harness = TestHarness::new().await.expect("Harness failed");

    // Extended session first
    let body = serde_json::json!({"value": "extended"});
    harness.put("/vehicle/v1/components/vtx_ecm/modes/session", body)
        .await.expect("Session failed");

    // Request seed
    let body = serde_json::json!({"value": "level1_requestseed"});
    let (_, json) = harness.put("/vehicle/v1/components/vtx_ecm/modes/security", body)
        .await.expect("Seed request failed");

    let seed = json["seed"].as_str().unwrap();
    let key = calculate_key(seed);  // XOR with secret

    // Send key
    let body = serde_json::json!({"value": "level1", "key": key});
    let (status, _) = harness.put("/vehicle/v1/components/vtx_ecm/modes/security", body)
        .await.expect("Key send failed");
    assert_eq!(status, 200);

    // Now test protected feature
    // ...
}
```

## Common Assertions

```rust
// Status code
assert_eq!(status, 200);
assert_eq!(status, 201);  // Created
assert_eq!(status, 204);  // No Content
assert_eq!(status, 400);  // Bad Request
assert_eq!(status, 403);  // Forbidden (security required)
assert_eq!(status, 404);  // Not Found
assert_eq!(status, 412);  // Precondition Failed (session required)

// JSON fields
assert!(json["value"].is_string());
assert_eq!(json["count"].as_u64(), Some(5));
assert!(json["items"].is_array());
assert!(json["error"].is_null());

// String content
let value = json["value"].as_str().unwrap();
assert!(value.contains("expected"));
assert_eq!(value.len(), 17);  // VIN length
```

## Debugging Test Failures

### 1. Check Test Output
```bash
cargo test --test e2e_test test_name -- --nocapture 2>&1 | tee test.log
```

### 2. Check Server Logs
```bash
RUST_LOG=debug cargo test --test e2e_test test_name -- --nocapture 2>&1 | \
  grep -E "(sovdd|sovd_uds|sovd_api)"
```

### 3. Check ECU Simulator
```bash
RUST_LOG=debug cargo test --test e2e_test test_name -- --nocapture 2>&1 | \
  grep "example_ecu"
```

### 4. Check UDS Messages
```bash
RUST_LOG=debug cargo test --test e2e_test test_name -- --nocapture 2>&1 | \
  grep -E "(Incoming message|UDS request|UDS response)"
```

## Test ECU Configuration

The example-ecu simulator is configured with:

### Parameters (DIDs)
| DID | Name | Access | Description |
|-----|------|--------|-------------|
| 0xF190 | vin | public | VIN (WF0XXXGCDX1234567) |
| 0xF187 | part_number | public | Part number |
| 0x1000 | engine_speed | extended | Engine RPM |
| 0x2000 | programming_date | protected | Requires security |

### DTCs
| DTC | Status | Description |
|-----|--------|-------------|
| P0100 | Active | MAF sensor |
| P0300 | Confirmed | Misfire |
| C0035 | Pending | ABS sensor |

### Routines
| RID | Name | Description |
|-----|------|-------------|
| 0xFF00 | self_test | Run self-test |
| 0xFF01 | clear_adaptation | Clear learned values |

### Security
- Level 1: XOR algorithm with secret `[0x12, 0x34, 0x56, 0x78]`

## Continuous Integration

Tests run automatically on:
- Pull requests
- Main branch pushes

### CI Script
```bash
./run-e2e-tests.sh
```

This script:
1. Sets up vcan0 interface
2. Builds all crates
3. Runs e2e tests
4. Reports results

## Test Coverage

Current test coverage by feature:

| Feature | Tests | Status |
|---------|-------|--------|
| Component listing | 2 | ✓ |
| DID read (public) | 5 | ✓ |
| DID read (extended) | 3 | ✓ |
| DID read (protected) | 2 | ✓ |
| DID write | 4 | ✓ |
| DTC read | 4 | ✓ |
| DTC clear | 2 | ✓ |
| Routines | 3 | ✓ |
| Session control | 4 | ✓ |
| Security access | 3 | ✓ |
| Software download | 4 | ✓ |
| Software upload | 2 | ✓ |
| ECU discovery | 2 | ✓ |
| SSE streaming | 2 | ✓ |
| I/O control | 3 | ✓ |

See [scripts/run-test.sh](scripts/run-test.sh) for a helper script.
