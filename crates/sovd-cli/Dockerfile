# syntax=docker/dockerfile:1
#
# Dockerfile for sovd-cli — the SOVD diagnostics CLI, packaged so the autoloader
# (examples/autoloader/sovd-get-logs.sh) can pull logs from a device with no
# local Rust toolchain.
#
# Built + published from supernova-towers (which submodules SOVDd) as
#   <registry>/tools/supernova-towers/sovd-cli
# alongside sumo-towers + sumo-minter. Build context = the SOVDd workspace root.
#
# Build from the SOVDd repo root:
#   docker build -t sovd-cli -f crates/sovd-cli/Dockerfile .
#
# Run (mint a token elsewhere; see sovd-get-logs.sh for the dance):
#   docker run --rm sovd-cli --server https://cvc.local:4000 --token "$JWT" \
#     --insecure logs list <ecu>
#
# TLS: reqwest uses rustls (no OpenSSL). The default path verifies against the
# bundled ca-certificates.crt; --ca-cert pins a root; --insecure skips verify.

# ---------------------------------------------------------------------------
# Stage 1: Build (Alpine = musl toolchain, static linking, no OpenSSL — rustls)
# ---------------------------------------------------------------------------
# Match the repo's pinned toolchain (rust-toolchain.toml → 1.96.0) so rustup
# doesn't re-download a channel at build time.
FROM rust:1.96-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build

COPY . .

RUN --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --bin sovd-cli && \
    cp target/release/sovd-cli /sovd-cli

# ---------------------------------------------------------------------------
# Stage 2: Runtime (scratch — fully static binary + a CA bundle for rustls)
# ---------------------------------------------------------------------------
FROM scratch

# rustls reads system roots for the default (non-pinned) verification path.
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /sovd-cli /sovd-cli

ENTRYPOINT ["/sovd-cli"]
