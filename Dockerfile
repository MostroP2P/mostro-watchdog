# ---- Build stage ----
FROM rust:1.83-slim-bookworm AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release && \
    strip target/release/mostro-watchdog

# ---- Runtime stage ----
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/* && \
    useradd -r -s /usr/sbin/nologin watchdog

COPY --from=builder /build/target/release/mostro-watchdog /usr/local/bin/mostro-watchdog

USER watchdog

ENTRYPOINT ["mostro-watchdog"]
CMD ["--config", "/config/config.toml"]
