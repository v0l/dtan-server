# DTAN Server - Agent Instructions

## Quick Start

**Build:** `cargo build --release`

**Run:** `cargo run -- --config /path/to/config.yml` (uses `./config.yml` by default)

**Docker:** `docker run --rm -it -p 8000:8000 voidic/dtan-server`

## Architecture

- **Single binary**: `src/main.rs` is the entrypoint
- **3 modules**: `http.rs` (web/relay), `peers.rs` (DHT peer discovery), `main.rs` (orchestration)
- **Database**: LMDB via `nostr-lmdb` (data dir from config)
- **Ports**: Default 7777 for relay, configurable via `listen` in config.yml

## Key Conventions

- **Config file**: YAML format (`config.yml`), required at startup
- **Custom nostr-sdk**: Pinned to `v0l/nostr-sdk` git repo (not crates.io)
- **Custom portmapper**: Uses `n0-computer/net-tools` for NAT-PMP/uPnP
- **Web UI**: Served from `web_dir` config (defaults to `./www`), built separately via `dtan` frontend repo

## Development

**Hot reload**: Not supported - rebuild required for code changes

**UI development**: Frontend is in separate `dtan` repo, built into `www/` dir

**Data persistence**: Database stored in `database_dir` from config (defaults to `./data`)

## Testing

No test suite exists. Manual verification via:
1. `cargo build` - compile check
2. Run with test config pointing to bootstrap relays
3. Verify web UI at configured port

## Deployment

**Docker build:**
```bash
docker buildx build --push -t voidic/dtan-server:latest .
```

**Drone CI**: `.drone.yml` handles automated builds to Docker Hub

## Config Keys

- `database_dir`: LMDB storage path
- `listen`: Bind address (default `0.0.0.0:7777`)
- `relays`: Bootstrap Nostr relays for peer discovery
- `dht_public_ip`: Explicit public IP for DHT (NAT traversal)
- `web_dir`: Static file serving path
- `use_dht`: Enable DHT peer discovery (default true)
- `distinct_info_hash`: Reject duplicate torrents (default false)
- `relay_document`: NIP-11 relay info document
