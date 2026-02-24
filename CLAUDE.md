# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

SOVDd is a Rust implementation of the ASAM SOVD (Service-Oriented Vehicle Diagnostics) server. It translates SOVD REST API calls into UDS (Unified Diagnostic Services) commands for automotive ECU diagnostics over CAN/ISO-TP or DoIP transports. The server follows the ASAM SOVD standard API paths under `/vehicle/v1/`.

## Build & Test Commands

```bash
# Build
cargo build

# Full CI check: fmt + clippy + build + test + release build
./build-and-test.sh --all

# Clippy lints only (CI treats warnings as errors: -D warnings)
cargo clippy --all -- -D warnings

# Format check
cargo fmt --all -- --check

# Unit tests only (no vcan required)
cargo test --lib

# All tests (serial — tests share vcan0)
cargo test --workspace -- --test-threads=1

# Single test by name
cargo test test_name -- --test-threads=1

# E2E tests (sets up vcan0, requires built binaries)
./run-e2e-tests.sh
./run-e2e-tests.sh test_list_components    # single e2e test

# Quick compile check (fastest feedback)
./build-and-test.sh --check

# Multi-ECU simulation (3 ECUs + gateway on vcan1)
./simulations/basic_uds/start.sh
```

## Architecture

### Crate Dependency Graph

```
sovd-core (foundation — DiagnosticBackend trait, models, errors)
  ↑
  ├── sovd-conv (DID encoding/decoding from YAML definitions)
  ├── sovd-api (axum REST handlers, routing, AppState)
  ├── sovd-uds (UDS backend — SocketCAN, DoIP, Mock transports)
  ├── sovd-gateway (multi-ECU aggregation, federated routing)
  ├── sovd-client (typed HTTP client — SovdClient, FlashClient)
  └── sovd-proxy (HTTP proxy backend for supplier containers)

sovdd (server binary)
  ├── sovd-api + sovd-uds + sovd-gateway + sovd-conv + sovd-proxy

example-app (app entity binary)
  ├── sovd-api + sovd-proxy + sovd-core

sovd-cli → sovd-client
sovd-tests → sovd-api + sovd-uds + sovd-gateway + sovd-conv + sovd-client
```

### Central Abstraction: `DiagnosticBackend` Trait

Defined in `crates/sovd-core/src/backend.rs`. ~35 async methods grouped by domain, all with default `NotSupported` implementations so backends only implement what they support:

- **Data:** `list_parameters`, `read_data`, `write_data`, `read_raw_did`, `write_raw_did`, `subscribe_data`
- **Faults:** `list_faults`, `get_fault_detail`, `clear_faults`
- **Operations:** `list_operations`, `execute_operation`, `get_operation_status`, `stop_operation`
- **I/O Control:** `list_outputs`, `get_output`, `control_output`
- **Flash/Software:** `receive_package`, `start_flash`, `get_flash_status`, `abort_flash`, `finalize_flash`, `commit_flash`, `rollback_flash`, `get_activation_state`, `ecu_reset`, `get_software_info`
- **Session/Security:** `get_session_mode`, `set_session_mode`, `get_security_mode`, `set_security_mode`
- **Entities:** `list_sub_entities`, `get_sub_entity`

Three implementations: `UdsBackend` (sovd-uds), `GatewayBackend` (sovd-gateway), `SovdProxyBackend` (sovd-proxy). The API layer (`sovd-api`) dispatches to whichever backend is configured and never knows the concrete type.

### API Layer (sovd-api)

`crates/sovd-api/src/lib.rs` builds the axum router. `AppState` holds: backends map, `DidStore`, `SubscriptionManager`. Handlers are in `crates/sovd-api/src/handlers/` — one file per domain (data.rs, faults.rs, flash.rs, modes.rs, operations.rs, outputs.rs, streams.rs, subscriptions.rs, sub_entity.rs, etc.). The largest handlers are `sub_entity.rs` (~910 lines, nested resource routing) and `data.rs` (~709 lines, read/write with gateway child routing).

### UDS Backend (sovd-uds)

`crates/sovd-uds/src/backend.rs` (~1,900 lines) is the main implementation. Key internals:
- **Transport abstraction:** `TransportAdapter` trait in `transport/mod.rs` with three impls: `socketcan/` (ISO-TP framing), `doip/` (TCP/TLS, ISO 13400), `mock.rs`
- **Session management:** `session.rs` — auto-sends tester-present (0x3E) every 2s in non-default sessions
- **Subscriptions:** `subscription.rs` — `StreamManager` polls DIDs periodically to emulate UDS 0x2A
- **UDS services:** Each UDS SID (0x10, 0x11, 0x19, 0x22, 0x27, 0x2A, 0x2E, 0x2F, 0x31, 0x34, 0x36, 0x37, 0x3E, 0x87) is implemented in the backend

### Gateway Composition (sovd-gateway)

`GatewayBackend` wraps N child backends and itself implements `DiagnosticBackend`. Parameters are addressed as `child_id/param_id`. Capabilities are the OR of all children. Supports unlimited nesting for multi-tier architectures (tested up to 4-tier in `simulations/supplier_ota/`).

### Flash State Machine

Strict 10-state lifecycle enforced in `sovd-uds`:
```
Queued → Preparing → Transferring → AwaitingExit → AwaitingReset → Activated → Committed|RolledBack
```
- Abort only valid during Queued through AwaitingExit
- AwaitingReset enforces ECU reboot before commit/rollback
- State held in `parking_lot::RwLock`; lock ordering: `activation_state` before `flash_state` to prevent deadlocks

### DID Conversion Pipeline (sovd-conv)

Raw ECU bytes ↔ physical values via YAML definitions in `config/did-definitions/`. `DidStore` uses `DashMap` for lock-free concurrent lookups. Supported shapes: scalar, array, map, histogram, bitfield, enum. Inline TOML definitions and ISO 14229-1 standard DIDs are also supported.

### Configuration

Server config is TOML (`config/*.toml`). DID definitions are YAML (`config/did-definitions/`). Key configs:
- `config/sovd.toml` — Mock transport (no hardware needed, good for development)
- `config/sovd-socketcan.toml` — SocketCAN on vcan0
- `config/gateway-socketcan.toml` — Multi-ECU gateway

## Conventions

- Rust 2021 edition, workspace dependencies managed in root `Cargo.toml`
- API routes follow ASAM SOVD standard: `/vehicle/v1/components/:id/...`
- Error types: `BackendError` (sovd-core) wraps into `ApiError` (sovd-api) which produces HTTP responses
- Concurrency: `parking_lot::RwLock` for flash/activation state, `DashMap` for DID lookups, tokio broadcast channels for subscriptions/SSE
- E2E tests require Linux with `vcan` kernel module loaded; tests use `serial_test` crate on shared vcan0
- Server debug logging: `RUST_LOG=debug cargo run --bin sovdd -- config/sovd.toml`
