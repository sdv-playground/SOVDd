# syntax=docker/dockerfile:1
#
# Dockerfile for example-app (SOVD supplier app entity)
#
# Build from the workspace root:
#   docker build -t sovd-example-app -f crates/example-app/Dockerfile .
#
# Run standalone:
#   docker run -p 4001:4001 sovd-example-app \
#     --upstream-url http://host.docker.internal:4002 \
#     --upstream-component vtx_vx500
#
# Run with config file:
#   docker run -p 4001:4001 \
#     -v ./config/example-app.toml:/config/example-app.toml:ro \
#     sovd-example-app \
#     --upstream-url http://host.docker.internal:4002 \
#     --upstream-component vtx_vx500 \
#     --config /config/example-app.toml

# ---------------------------------------------------------------------------
# Stage 1: Build (Alpine = musl toolchain, static linking)
# ---------------------------------------------------------------------------
FROM rust:1.87-alpine AS builder

RUN apk add --no-cache musl-dev pkgconf openssl-dev openssl-libs-static

ENV OPENSSL_STATIC=1

WORKDIR /build

COPY . .

RUN --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --bin example-app && \
    cp target/release/example-app /example-app

# ---------------------------------------------------------------------------
# Stage 2: Runtime (scratch — fully static binary, nothing else needed)
# ---------------------------------------------------------------------------
FROM scratch

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /example-app /example-app

EXPOSE 4001

ENTRYPOINT ["/example-app"]
