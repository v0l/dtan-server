# DTAN Server

**Distributed Torrent Archive on Nostr** - A self-replicating torrent index built on Nostr protocol.

DTAN server acts as both a Nostr relay for torrent metadata and a DHT node for peer discovery,
providing a censorship-resistant way to index and discover torrents.

## Features

- 🌐 **Decentralized torrent indexing** via Nostr protocol
- 🔄 **Self-replicating** across multiple relay nodes
- 🌍 **DHT integration** for peer discovery
- 📱 **Web interface** for browsing torrents
- ⚡ **High performance** Rust implementation
- 🔒 **Censorship resistant** distributed architecture

## Installation

### Using Cargo

```bash
cargo install dtan-server --git https://github.com/v0l/dtan-server
```

### Using Docker

```bash
docker run --rm -it -p 8000:8000 voidic/dtan-server
```

### Build from Source

```bash
git clone https://github.com/v0l/dtan-server
cd dtan-server
cargo build --release
```

## Configuration

Create a `config.yml` file to customize your DTAN server:

```yaml
# Location where the relay will store its data
database_dir: "./data"

# Listen address of dtan relay
listen: "0.0.0.0:8000"

# Bootstrap relays to discover content
relays:
  - "wss://relay.damus.io"
  - "wss://nos.lol"
  - "wss://relay.nostr.band"
  - "wss://relay.snort.social"

# Explicitly set your public IP for DHT (default NAT-PMP / Main public IP)
dht_public_ip: "140.140.140.140"

# Path to DTAN web site build (default www dir)
web_dir: "./www"
```

## Usage

### Start the server

```bash
# Using default config
dtan-server

# With custom config
dtan-server --config /path/to/config.yml
```

### Access the web interface

Open your browser to `http://localhost:8000` to browse and search torrents.

### API Endpoints

- `GET /` - Web interface
- WebSocket connection for Nostr relay functionality
- DHT integration runs automatically

## How it Works

1. **Torrent Discovery**: Connects to bootstrap Nostr relays to discover torrent metadata
2. **Local Storage**: Stores discovered torrents in a local database
3. **Relay Service**: Acts as a Nostr relay, allowing other nodes to discover your torrents
4. **DHT Integration**: Participates in the BitTorrent DHT for peer discovery
5. **Web Interface**: Provides a user-friendly interface for browsing torrents

## Network Ports

- **8000** (default): HTTP/WebSocket server for web interface and Nostr relay
- **Various DHT ports**: Automatically configured for DHT participation

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests if applicable
5. Submit a pull request

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.