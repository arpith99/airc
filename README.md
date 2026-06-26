# AIRC - Async IRC Client

A specialized IRC client for downloading books via DCC file transfers. Built with Rust and tokio for concurrent, non-blocking operations.

<img width="1920" height="1157" alt="airc" src="https://github.com/user-attachments/assets/a6833314-fa29-4da9-86dd-73eac9082b73" />


## Features

- **IRC Protocol**: Connect, authenticate, join channels, PING/PONG handling
- **TLS**: Optional encrypted control connection (rustls) with certificate verification
- **DCC Transfers**: Concurrent file downloads with streaming and ACK protocol
- **Search**: IRC-based book search with local filtering
- **Configuration**: TOML config file with CLI overrides
- **Async**: Non-blocking I/O, concurrent downloads, graceful shutdown
- **Error Handling**: Custom error types with descriptive messages
- **Logging**: Structured logging routed into the TUI message pane
- **TUI**: Full-screen ratatui interface — message/book/user panes, scrollbars, mouse, live download gauges

## Installation

### Prebuilt binaries

Download the latest release for your platform from the
[releases page](https://github.com/arpith99/airc/releases/latest):

- **Linux (x86_64)**: `airc-v0.1.1-x86_64-linux`
- **Windows (x86_64)**: `airc-v0.1.1-x86_64-windows.exe`

On Linux, make it executable after downloading:

```bash
chmod +x airc-v0.1.1-x86_64-linux
```

### Build from source

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
tls = false          # Set true to connect over TLS
port = 6667          # Optional; defaults to 6667, or 6697 when tls = true
```

Override with CLI arguments:
```bash
airc --server irc.example.com --channel "#mychannel" --username myuser
airc --server irc.libera.chat --tls          # connects on 6697
airc --server irc.example.com --tls --port 7000
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
- **src/tui/** - ratatui TUI: App state, rendering, event application, log routing
- **src/error.rs** - Custom error types
- **src/config.rs** - Configuration management

## Testing

```bash
cargo test
```

64 tests covering error handling, config merging, IP conversion, regex patterns, command processing, DCC task draining, and TUI state/scroll/input-event logic.

## Dependencies

- tokio (async runtime)
- thiserror (error handling)
- tracing (logging)
- clap (CLI parsing)
- serde + toml (configuration)
- regex + once_cell (pattern matching)
- ratatui + crossterm (terminal UI)
- chrono (timestamps)

## License

See LICENSE file.
