# ---- Build stage ----
FROM rust:1.94-slim-bookworm AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock build.rs ./
COPY src/ src/

# The build context has no .git directory, so pass the commit hash explicitly to
# have `/version` report it: docker build --build-arg GIT_COMMIT=$(git rev-parse --short HEAD)
ARG GIT_COMMIT
ENV MOSTRO_WATCHDOG_GIT_COMMIT=$GIT_COMMIT

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
