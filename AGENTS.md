# SOVDd Index

Rust ASAM SOVD server workspace translating `/vehicle/v1` REST/SSE calls to UDS, DoIP, mock, gateway, and proxy backends.

## Where to look

- `README.md` — user-facing build/run/test and server examples.
- `ARCHITECTURE.md` — detailed crate map, backend trait, HTTP API, security and flash model.
- `CLAUDE.md` — concise contributor conventions and current commands.
- `Cargo.toml` — workspace crates and features.
- `crates/sovd-core/` — `DiagnosticBackend`, models, errors.
- `crates/sovd-api/` — axum REST handlers and app state.
- `crates/sovd-uds/` — UDS transports and services.
- `crates/sovd-client/`, `crates/sovd-cli/`, `crates/sovd-tests/` — client, CLI, integration tests.
- `config/`, `simulations/` — TOML/YAML configs and runnable ECU simulations.

## Essential commands

No component-local `mise` file is present; use Cargo and repo scripts from this submodule root.

```bash
cargo build
./build-and-test.sh --check
./build-and-test.sh --all
cargo fmt --all -- --check
cargo clippy --all -- -D warnings
cargo test --lib
cargo test --workspace -- --test-threads=1
./run-e2e-tests.sh
RUST_LOG=debug cargo run --bin sovdd -- config/sovd.toml
```

Finding commands:

```bash
rg --files -g 'Cargo.toml' -g 'README*' -g 'ARCHITECTURE.md' -g 'CLAUDE.md'
rg -n "DiagnosticBackend|FlashState|GatewayBackend|SovdProxyBackend|/updates|security|vcan" crates config simulations README.md ARCHITECTURE.md CLAUDE.md
```

## Stack

- Rust 2021 workspace, axum HTTP API, tokio async runtime, SocketCAN/DoIP/mock transports.
- TOML server configs and YAML DID conversion definitions.

## Guardrails

- Keep SOVDd spec-pure; vendor-specific SUMO behavior belongs in machine-manager layers unless documented otherwise.
- E2E tests share vcan interfaces; run serially.
- Do not put security secrets in SOVDd; simulations use the security helper.

## Gotchas

- E2E paths require Linux/vcan setup and built binaries.
- ECU reset returns session/security to defaults; tests must re-establish them before commit/rollback.

## Missing docs/specs to watch

- Auth and security design is split across architecture docs and workspace design drafts.
- Some SOVD behavior is tracked against draft/condensed ISO/SOVD specs in workspace docs, not only here.
