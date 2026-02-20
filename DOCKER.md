# Docker Deployment

This document describes how to run mostro-watchdog using Docker.

## Quick Start

### 1. Create your configuration

```bash
cp config.example.toml config.toml
# Edit config.toml with your Mostro pubkey, Telegram bot token, and chat ID
```

### 2. Run with Docker Compose (recommended)

```bash
docker compose up -d
```

### 3. Check logs

```bash
docker compose logs -f
```

## Alternative: Run with Docker directly

```bash
# Build the image
docker build -t mostro-watchdog .

# Run the container
docker run -d \
  --name mostro-watchdog \
  --restart unless-stopped \
  -v $(pwd)/config.toml:/config/config.toml:ro \
  mostro-watchdog
```

## Pre-built Images

Pre-built images are available from GitHub Container Registry on each release:

```bash
docker pull ghcr.io/mostrop2p/mostro-watchdog:latest

# Or a specific version
docker pull ghcr.io/mostrop2p/mostro-watchdog:0.1.2
```

Run with a pre-built image:

```bash
docker run -d \
  --name mostro-watchdog \
  --restart unless-stopped \
  -v $(pwd)/config.toml:/config/config.toml:ro \
  ghcr.io/mostrop2p/mostro-watchdog:latest
```

## Health Endpoint

To use the HTTP health endpoint in Docker, you must configure **both** `enable_http_endpoint` and `http_bind` in your `config.toml`:

```toml
[health]
enable_http_endpoint = true
http_bind = "0.0.0.0"   # Required for Docker — default 127.0.0.1 is not reachable from outside the container
http_port = 8080
```

Then expose the port in `docker-compose.yml`:

```yaml
services:
  mostro-watchdog:
    # ...
    ports:
      - "8080:8080"
```

Or with `docker run`:

```bash
docker run -d \
  --name mostro-watchdog \
  --restart unless-stopped \
  -v $(pwd)/config.toml:/config/config.toml:ro \
  -p 8080:8080 \
  ghcr.io/mostrop2p/mostro-watchdog:latest
```

Check it:

```bash
curl http://localhost:8080/health
```

## Configuration

The container expects a config file mounted at `/config/config.toml`. See `config.example.toml` for all available options.

### Environment

The container runs as a non-root user (`watchdog`) for security. No environment variables are required — all configuration is done via the config file.

### Logging

Set the log level via the `RUST_LOG` environment variable:

```bash
docker run -d \
  --name mostro-watchdog \
  -e RUST_LOG=debug \
  -v $(pwd)/config.toml:/config/config.toml:ro \
  ghcr.io/mostrop2p/mostro-watchdog:latest
```

Or in docker-compose.yml:

```yaml
services:
  mostro-watchdog:
    # ...
    environment:
      - RUST_LOG=debug
```

## Image Details

- **Base image:** `debian:bookworm-slim`
- **Architecture:** `linux/amd64` (x86_64)
- **User:** `watchdog` (non-root)
- **Size:** ~30MB compressed
- **Build:** Multi-stage (Rust builder → slim runtime)
