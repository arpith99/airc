# AIRC - Async IRC Client

A specialized IRC client for downloading books via DCC file transfers. Built with Rust and tokio for concurrent, non-blocking operations.

## Features

- **IRC Protocol**: Connect, authenticate, join channels, PING/PONG handling
- **DCC Transfers**: Concurrent file downloads with streaming and ACK protocol
- **Search**: IRC-based book search with local filtering
- **Configuration**: TOML config file with CLI overrides
- **Async**: Non-blocking I/O, concurrent downloads, graceful shutdown
- **Error Handling**: Custom error types with descriptive messages
- **Logging**: Structured logging with colored terminal output

## Installation

```bash
cargo build --release
```

Binary will be at `target/release/airc`

## Configuration

Config file location: `~/.config/airc/config.toml`

```toml
server = "irc.undernet.org"
channel = "#bookz"
username = "myuser"  # Optional, generates random if not set
download_path = "./downloads/"
connection_timeout_secs = 30
dcc_timeout_secs = 300
```

Override with CLI arguments:
```bash
airc --server irc.example.com --channel "#mychannel" --username myuser
```

## Commands

| Command | Description |
|---------|-------------|
| `/join` or `/j` | Join the configured channel |
| `/search <term>` or `/s <term>` | Search for books on IRC |
| `/ss <term>` | Local search within downloaded results |
| `/<number>` | Request book by entry number |
| `/quit [msg]` or `/q [msg]` | Disconnect with optional message |
| `/<command>` | Send raw IRC command |

## Usage Example

```bash
# Start the client
./airc

# Search for a book
/search rust programming

# Filter local results
/ss async

# Request book #5
/5

# Quit
/q
```

## Architecture

- **src/main.rs** - CLI parsing, config wiring, task orchestration
- **src/client.rs** - `IrcClient`, connection/registration, task loops, IRC receive & DCC dispatch
- **src/dcc.rs** - DCC file transfer, filename sanitization, unzip
- **src/commands.rs** - User command parsing and local search
- **src/net.rs** - Network retry with exponential backoff
- **src/ui.rs** - Colored terminal output
- **src/error.rs** - Custom error types
- **src/config.rs** - Configuration management

## Testing

```bash
cargo test
```

20 tests covering error handling, config merging, IP conversion, regex patterns, command processing, and DCC task draining.

## Dependencies

- tokio (async runtime)
- thiserror (error handling)
- tracing (logging)
- clap (CLI parsing)
- serde + toml (configuration)
- regex + once_cell (pattern matching)
- colored, chrono (terminal output)

## License

See LICENSE file.
