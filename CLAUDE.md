# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

SOVDd is a Rust implementation of the ASAM SOVD (Service-Oriented Vehicle Diagnostics) server. It translates SOVD REST API calls into UDS (Unified Diagnostic Services) commands for automotive ECU diagnostics over CAN/ISO-TP or DoIP transports. The server follows the ASAM SOVD standard API paths under `/vehicle/v1/`.

## Build & Test Commands

```bash
# Build
cargo build

# Full CI check: fmt + clippy + build + test
./build-and-test.sh --all

# Clippy lints only
cargo clippy --workspace --all-targets

# Format check
cargo fmt --all -- --check

# Unit tests only (no vcan required)
cargo test --lib

# All tests (serial - tests share vcan0)
cargo test --workspace -- --test-threads=1

# Single test by name
cargo test test_name -- --test-threads=1

# E2E tests (requires vcan0 and running example-ecu + sovd-server)
./run-e2e-tests.sh
./run-e2e-tests.sh test_list_components    # single e2e test
```

## Architecture

### Workspace Structure (12 crates)

**Libraries:**
- `sovd-core` — Central `DiagnosticBackend` trait (~35 async methods, all with default `NotSupported` impls), shared model types, and error types. Every backend implements this trait.
- `sovd-api` — Backend-agnostic HTTP REST layer built on axum. Creates the router, holds `AppState` (backends map, DidStore, SubscriptionManager). Handlers dispatch to whichever `DiagnosticBackend` is configured.
- `sovd-uds` — UDS protocol backend (`UdsBackend`). Implements `DiagnosticBackend` by encoding SOVD operations as UDS service requests. Supports three transports: SocketCAN (ISO-TP), DoIP (ISO 13400), and Mock.
- `sovd-gateway` — Multi-ECU aggregation (`GatewayBackend`). Wraps N child backends, routes requests by `backend_id/param_id` prefix. Itself implements `DiagnosticBackend`, enabling nested gateway architectures.
- `sovd-conv` — DID value encoding/decoding from YAML definitions (`DidStore`, `DidDefinition`). Supports scalar, array, map, histogram, bitfield, and enum types.
- `sovd-client` — Typed HTTP client (`SovdClient`) for calling SOVD servers. Used by proxy backend and tests.
- `sovd-proxy` — HTTP proxy backend (`SovdProxyBackend`). Enables supplier containers to serve SOVD without direct CAN bus access by proxying to an upstream SOVD server.

**Binaries:**
- `sovdd` — Main server binary. Loads TOML config, bootstraps backends, starts axum server.
- `example-ecu` — Example ECU simulator running on vcan. Simulates UDS responses including flash A/B banks.
- `example-app` — Example app entity binary demonstrating the managed ECU sub-entity pattern. `ExampleAppBackend` (type "app") owns a `ManagedEcuBackend` (type "ecu") sub-entity that proxies diagnostics and manages flash transfers.
- `sovd-cli` — CLI tool (clap-based) with table/JSON/CSV output.

**Tests:**
- `sovd-tests` — E2E integration tests. Requires vcan0 + example-ecu + sovd-server running. Tests use `serial_test` crate for serial execution.

### Key Design Patterns

**Trait-based backend abstraction:** `DiagnosticBackend` in `sovd-core` is the central interface. Three implementations exist: `UdsBackend`, `GatewayBackend`, `SovdProxyBackend`. The API layer never knows which backend is active.

**Gateway composition:** `GatewayBackend` itself implements `DiagnosticBackend` and wraps child backends. Parameters are addressed as `child_id/param_id`. This enables multi-tier architectures (e.g., 4-tier supplier OTA).

**Flash state machine:** `Queued → Preparing → Transferring → AwaitingExit → AwaitingReset → Activated → Committed/RolledBack`. State is held in `parking_lot::RwLock` for sync-friendly access.

**Concurrency:** `parking_lot::RwLock` for flash/activation state, `DashMap` for lock-free DID lookups, tokio channels for subscriptions/SSE streaming.

### Configuration

Server config is TOML (`config/*.toml`). DID definitions are YAML (`config/did-definitions/`). Key configs:
- `config/sovd.toml` — Mock transport (no hardware needed)
- `config/sovd-socketcan.toml` — SocketCAN on vcan0
- `config/gateway-socketcan.toml` — Multi-ECU gateway

Multi-ECU simulation scripts are in `simulations/` (`basic_uds/`, `supplier_ota/`).

## Conventions

- Rust 2021 edition, workspace dependencies managed in root `Cargo.toml`
- API routes follow ASAM SOVD standard: `/vehicle/v1/components/:id/...`
- UDS service implementations are in `sovd-uds/src/services/` (one file per UDS SID)
- Error types: `BackendError` (core) wraps into `ApiError` (api) which produces HTTP responses
- E2E tests require Linux with `vcan` kernel module loaded
