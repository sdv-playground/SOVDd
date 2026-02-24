---
name: test-automation
description: Running and interpreting SOVD e2e tests. Use when running tests, debugging test failures, adding new tests, or validating SOVD server functionality.
metadata:
  author: sovd-team
  version: "2.0"
---

# Test Automation

This skill covers running, interpreting, and creating tests for the SOVD server.

## Test Architecture

```
crates/sovd-tests/
├── tests/
│   ├── e2e_test.rs           # Main e2e test suite
│   ├── gateway_e2e_test.rs   # Gateway-specific tests
│   └── api_integration_test.rs
└── src/
    └── lib.rs

crates/example-ecu/           # ECU simulator (runs on vcan0)
├── src/
│   ├── main.rs               # Entry point with CLI args
│   ├── config.rs             # Default ECU configuration (DIDs, DTCs, routines, outputs)
│   ├── parameters.rs         # UDS request handling
│   └── uds.rs                # UDS service dispatch
```

## Running Tests

### Unit Tests (no vcan required)
```bash
cargo test --lib
```

### All E2E Tests
```bash
# Tests must run serially (shared vcan0)
cargo test -p sovd-tests -- --test-threads=1
```

### Single Test
```bash
cargo test -p sovd-tests test_read_vin -- --test-threads=1
```

### With Debug Output
```bash
RUST_LOG=debug cargo test -p sovd-tests test_name -- --test-threads=1 --nocapture
```

### Using the E2E Script
```bash
# Runs all e2e tests (sets up vcan, builds, runs)
./run-e2e-tests.sh

# Single test
./run-e2e-tests.sh test_list_components
```

### Full CI Check
```bash
./build-and-test.sh --all
```

## Test Harness

The `TestHarness` in `e2e_test.rs` manages the full test lifecycle:

1. Sets up vcan0 interface
2. Starts `example-ecu` process on vcan0
3. Creates a test TOML config (inline in the test file)
4. Starts `sovdd` server on port 18080
5. Provides a `SovdClient` for making SOVD API calls

### TestHarness API

```rust
let harness = TestHarness::new().await?;

// Get a typed SOVD client
let client = harness.sovd_client();

// Direct HTTP access
let http = harness.http_client();
let base = harness.base_url();  // "http://localhost:18080"
```

### TestHarnessOptions

```rust
let opts = TestHarnessOptions {
    block_counter_start: 1,  // 0 or 1
    block_counter_wrap: 0,   // wrap-to value at 255
    supports_rollback: true, // enable commit/rollback
};
let harness = TestHarness::new_with_options(opts).await?;
```

### Helper Methods

```rust
// Set up programming session + security for flash tests
harness.setup_programming_and_security().await?;

// Set up extended session + security (for commit/rollback after ECU reset)
harness.setup_extended_and_security().await?;
```

Both helpers follow: `set_session()` → `security_access_request_seed()` → XOR with 0xFF → `security_access_send_key()`.

## Writing New Tests

### Basic Test Template
```rust
#[tokio::test]
#[serial_test::serial]
async fn test_my_feature() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let client = harness.sovd_client();

    let data = client.read_data("vtx_ecm", "vin").await.unwrap();
    assert!(data.value.is_string());
}
```

### Test with Session Setup
```rust
#[tokio::test]
#[serial_test::serial]
async fn test_extended_feature() {
    let harness = TestHarness::new().await.expect("Harness failed");
    let client = harness.sovd_client();

    // Switch to extended session
    client.set_session("vtx_ecm", SessionType::Extended).await.unwrap();

    // Now test extended-session feature
    let data = client.read_data("vtx_ecm", "engine_rpm").await.unwrap();
    assert!(data.value.is_number());
}
```

### Test with Security Setup
```rust
#[tokio::test]
#[serial_test::serial]
async fn test_protected_feature() {
    let harness = TestHarness::new().await.expect("Harness failed");
    let client = harness.sovd_client();

    // Extended session
    client.set_session("vtx_ecm", SessionType::Extended).await.unwrap();

    // Security access: request seed, XOR with 0xFF, send key
    let seed = client.security_access_request_seed("vtx_ecm", SecurityLevel::LEVEL_1).await.unwrap();
    let key: Vec<u8> = seed.iter().map(|b| b ^ 0xFF).collect();
    client.security_access_send_key("vtx_ecm", SecurityLevel::LEVEL_1, &key).await.unwrap();

    // Now test protected feature
}
```

### Flash Test
```rust
#[tokio::test]
#[serial_test::serial]
async fn test_flash_workflow() {
    let harness = TestHarness::new_with_options(TestHarnessOptions {
        supports_rollback: true,
        ..Default::default()
    }).await.expect("Harness failed");

    // Session + security setup required before start_flash
    harness.setup_programming_and_security().await.unwrap();

    let client = harness.sovd_client();
    // ... upload, verify, flash, poll, finalize, reset, commit
}
```

## Test ECU Configuration

The test harness uses `vtx_ecm` as the component ID. The ECU config is generated inline in the test file with the default example-ecu parameters.

### Security
- Algorithm: XOR with secret
- Default secret: `0xFF` (single byte)
- Key calculation: `key[i] = seed[i] ^ 0xFF`

### Key Parameters
| DID | ID | Access |
|-----|-----|--------|
| 0xF190 | vin | Default session |
| 0xF40C | engine_rpm | Extended session |
| 0xF42F | boost_pressure | Extended + Security L1 |
| 0xF199 | programming_date | Writable, Extended session |

### DTCs
| Code | Status | Description |
|------|--------|-------------|
| P0101 | 0x09 (active) | Mass Air Flow Circuit |
| P0300 | 0x24 (pending) | Random Cylinder Misfire |
| C0420 | 0x28 (historical) | Steering Angle Sensor |
| B1234 | 0x89 (active+MIL) | Airbag Warning Circuit |
| U0100 | 0x28 (historical) | Lost Communication |

### Routines
| RID | Name | Access |
|-----|------|--------|
| 0x0203 | Check Programming Preconditions | Extended session |
| 0xFF00 | Erase Memory | Programming + Security |
| 0xFF01 | Firmware Commit | Extended + Security |
| 0xFF02 | Firmware Rollback | Extended + Security |

## Debugging Test Failures

```bash
# Full debug output
RUST_LOG=debug cargo test -p sovd-tests test_name -- --test-threads=1 --nocapture 2>&1 | tee test.log

# Filter by module
grep -E "(sovdd|sovd_uds|sovd_api)" test.log    # Server logs
grep "example_ecu" test.log                       # ECU simulator logs
grep -E "(UDS request|UDS response)" test.log     # UDS traffic
```

Common failure causes:
- **vcan0 not set up**: Run `sudo modprobe vcan && sudo ip link add vcan0 type vcan && sudo ip link set vcan0 up`
- **Port conflict**: Another sovdd instance running on 18080
- **Session/security not set**: Flash tests need `setup_programming_and_security()` before `start_flash()`
- **Serial execution**: Tests share vcan0, always use `--test-threads=1`
