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
cargo test -p sovd-tests test_name -- --test-threads=1

# E2E tests (sets up vcan0, requires built binaries)
./run-e2e-tests.sh
./run-e2e-tests.sh test_list_components    # single e2e test

# Quick compile check (fastest feedback)
./build-and-test.sh --check

# Multi-ECU simulation (3 ECUs + gateway on vcan1)
./simulations/basic_uds/start.sh

# Server debug logging
RUST_LOG=debug cargo run --bin sovdd -- config/sovd.toml
```

## Architecture

> Full detail in `ARCHITECTURE.md` (verified 2026-06-03). This section is the quick contributor map.

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

example-app (app entity binary — proxies an upstream SOVD server, e.g. a sovdd fronting example-ecu)
  ├── sovd-api + sovd-proxy + sovd-core + sovd-client + sovd-uds

sovd-cli → sovd-client
sovd-tests → sovd-api + sovd-uds + sovd-gateway + sovd-conv + sovd-client

sovd-mdns (standalone — mDNS/DNS-SD advertiser, ISO 17978-3 §5.11; reads the SOVD-server
  instance id from the TLS leaf SAN, no sovd-core dep. Consumed by external SOVD servers as a
  git dep. Off-QNX backend: mdns-sd; QNX (nto): a hand-rolled simple-dns + socket2 responder.)
```

### Central Abstraction: `DiagnosticBackend` Trait

Defined in `crates/sovd-core/src/backend.rs`. ~45 async methods grouped by domain, almost all with default `NotSupported` implementations so backends only implement what they support:

- **Data:** `list_parameters`, `read_data`, `write_data`, `read_raw_did`, `write_raw_did`, `define_data_identifier`, `clear_data_identifier`, `subscribe_data`, `ecu_reset`
- **Faults:** `get_faults`, `get_fault_detail`, `clear_faults`
- **Logs:** `get_logs`, `get_log`, `get_log_content`, `delete_log`, `stream_logs`
- **Operations:** `list_operations`, `start_operation`, `get_operation_status`, `stop_operation`
- **I/O Control:** `list_outputs`, `get_output`, `control_output`
- **Software/packages:** `get_software_info`, `receive_package`, `receive_package_stream`, `list_packages`, `get_package`, `verify_package`, `verify_part`, `delete_package`
- **Async flash:** `start_flash`, `update_shape`, `get_flash_status`, `list_flash_transfers`, `abort_flash`, `finalize_flash`, `validate`, `invalidate`, `activate`, `commit_flash`, `rollback_flash`, `get_activation_state`
- **Modes:** `get/set_session_mode`, `get/set_security_mode`, `get/set_link_mode`
- **Entities:** `list_sub_entities`, `get_sub_entity`

Three library implementations — `UdsBackend` (sovd-uds), `GatewayBackend` (sovd-gateway), `SovdProxyBackend` (sovd-proxy) — plus the reference app-entity `ManagedEcuBackend`/`ExampleAppBackend` (example-app). The API layer (`sovd-api`) dispatches to whichever backend is configured and never knows the concrete type.

### API Layer (sovd-api)

`crates/sovd-api/src/lib.rs` builds the axum router (one flat `Router` with `GenericError` 404/405 fallbacks and CORS/trace/no-body-limit layers — no auth/TLS today). `AppState` holds: backends map, `DidStore`, `SubscriptionManager`, plus per-domain caches (operation executions, `/updates` tracking, log/clear-data config). Handlers are in `crates/sovd-api/src/handlers/` — one file per domain (data.rs, faults.rs, operations.rs, modes.rs, reset.rs, updates.rs, subscriptions.rs, sub_entity.rs, stubs.rs, …). The retired `flash.rs`/`files.rs`/`outputs.rs`/`streams.rs`/`discovery.rs` handlers are gone: flash/OTA is the `/updates` wire, I/O control (0x2F) lives under `/operations` (C-133), streaming is `cyclic-subscriptions` (the SSE is content-negotiated on the subscription resource), and bus-discovery (`POST /discovery`) was dropped (C-025).

### UDS Backend (sovd-uds)

`crates/sovd-uds/src/backend.rs` (~1,900 lines) is the main implementation. Key internals:
- **Transport abstraction:** `TransportAdapter` trait in `transport/mod.rs` with three impls, each feature-gated: `socketcan/` (ISO-TP framing; default feature), `doip/` (TCP/TLS, ISO 13400), `mock.rs` (`mock-transport`, opt-in — enabled by sovdd for the demo config and by sovd-uds's own tests via a self dev-dependency)
- **Session management:** `session.rs` — auto-sends tester-present (0x3E) every 2s in non-default sessions; `notify_ecu_reset()` tracks that ECU reverts to default session after reset (0x11)
- **Subscriptions:** `subscription.rs` — `StreamManager` polls DIDs periodically to emulate UDS 0x2A
- **UDS services:** Each UDS SID (0x10, 0x11, 0x19, 0x22, 0x27, 0x2A, 0x2E, 0x2F, 0x31, 0x34, 0x36, 0x37, 0x3E, 0x87) is implemented in the backend

### Gateway Composition (sovd-gateway)

`GatewayBackend` wraps N child backends and itself implements `DiagnosticBackend`. Children are exposed as sub-entities and addressed via `/apps/{child}/...` (the flat gateway data path was retired for C-021); internally, resources route by `child_id/param_id` prefix. The gateway advertises gateway-class capabilities (`sub_entities`); a client reads each child's real capabilities from that child's own detail — not a naive OR. Supports unlimited nesting for multi-tier architectures (tested up to 4-tier in `simulations/supplier_ota/`).

### App Entity Model (example-app)

`ManagedEcuBackend` in `crates/example-app/src/managed_ecu.rs` demonstrates the supplier app pattern. Key design:

- **Two-level session management:** outer app session (local `RwLock<String>`) gates flash operations; inner ECU session is managed via `SovdProxyBackend` calls to the upstream server
- **Internal security:** the app holds the supplier's security secret and performs seed-key authentication internally — external clients never see it (`set_security_mode` returns `NotSupported`)
- **Parameter whitelist:** when `parameter_definitions` are configured, only those are exposed via `list_parameters()`. Standard UDS DIDs are intentionally omitted unless the supplier adds them. This lets the tier-1 curate what the OEM sees.
- **Flash lifecycle:** `start_flash()` sets inner ECU to programming session + unlocks security. After ECU reset, `commit_flash()`/`rollback_flash()` must re-establish extended session + security because reset reverts both.

### Flash State Machine

13-state `FlashState` lifecycle (`crates/sovd-core/src/backend.rs`), branching on `supports_rollback`. See ARCHITECTURE.md §8 for the full dual-bank vs single-bank diagram. Dual-bank trial path:
```
Initial → Queued → Preparing → Transferring → AwaitingActivation [→ Validated] → AwaitingReboot → Verifying → Activated → Committed|RolledBack
```
Single-bank collapses to `… → Activated → Complete` (no reboot/trial). `Failed` is the terminal error/abort state.
- Abort only valid Queued..AwaitingActivation (+ Validated); after AwaitingReboot, revert via `rollback_flash()` once Activated
- `Verifying` = component-driven post-reset health check; `validate()`/`Validated` are opt-in
- State held in `parking_lot::RwLock`; lock ordering: `activation_state` before `flash_state` to prevent deadlocks

### Security Model

Authorization is a JWT bearer enforced at the `sovd-api` layer; UDS `SecurityAccess` (0x27) against a real ECU is the server's job. Session/security setup falls to one of:
- **Transparent server-side unlock (default path):** a per-ECU `UnlockProvider` (`sovd-uds/src/unlock.rs`) lets the backend perform the 0x27 seed/key dance itself, on demand. Configured via an optional `[ecu.*.unlock]` section (`algorithm` + `secret_hex`); the dev impl is `XorUnlock`. When a gated operation is rejected with NRC 0x33 (write `write_raw_did`, or the flash `RequestDownload` in `run_flash_transfer`), the backend unlocks and retries once — clients no longer need to drive `modes/security`. The tester-side pre-checked ops (`start_operation` 0x31, `control_output` 0x2F) never emit 0x33, so they unlock *proactively* at the pre-check instead (`ensure_unlocked_for`, at the op's own required level); session control stays a client/modes concern. Absent section ⇒ the op fails with the ECU's NRC / `SecurityRequired` (403). Auth already happened above; sovd-uds holds no auth logic.
- **Direct UDS access (classic):** the external tester still *may* set session and perform security access itself; the spec-mandated `modes/security` surface stays for conformance.
- **App entity access:** the supplier app (`ManagedEcuBackend`) holds its own secret internally and manages inner ECU session/security itself — transparent to OEM clients.

The retired offboard `SOVD-security-helper` (seed/key over a side channel) is superseded by the transparent server-side path above.

### DID Conversion Pipeline (sovd-conv)

Raw ECU bytes ↔ physical values via YAML definitions in `config/did-definitions/`. `DidStore` uses `DashMap` for lock-free concurrent lookups. Supported shapes: scalar, array, map, histogram, bitfield, enum. Inline TOML definitions and ISO 14229-1 standard DIDs are also supported.

### Configuration

Server config is TOML (`config/*.toml`). DID definitions are YAML (`config/did-definitions/`). Key configs:
- `config/sovd.toml` — Mock transport demo (no hardware needed, good for development). Also the no-config fallback: a bare `sovdd` run from the repo root serves this file (with a warning) and errors out if it is missing.
- `config/sovd-socketcan.toml` — SocketCAN on vcan0
- `config/gateway-socketcan.toml` — Multi-ECU gateway

## Conventions

- Rust 2021 edition, workspace dependencies managed in root `Cargo.toml`
- API routes follow ASAM SOVD standard: `/vehicle/v1/components/:id/...`
- Error types: `BackendError` (sovd-core) wraps into `ApiError` (sovd-api) which produces HTTP responses
- Concurrency: `parking_lot::RwLock` for flash/activation state, `DashMap` for DID lookups, tokio broadcast channels for subscriptions/SSE
- E2E tests require Linux with `vcan` kernel module loaded; tests use `serial_test` crate on shared vcan0
- E2E `TestHarness` spawns example-ecu + sovdd on port 18080; flash tests need `setup_programming_and_security()` before `start_flash()` and `setup_extended_and_security()` before `commit_flash()`/`rollback_flash()`
- After ECU reset (0x11), session reverts to default (0x01) and security re-locks — this is per ISO 14229 and tracked by `notify_ecu_reset()` in session manager
- Example-ecu security uses XOR algorithm with default secret `0xFF`
