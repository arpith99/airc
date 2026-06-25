# Design: ratatui TUI for airc

Date: 2026-06-25

## Goal

Replace airc's line-oriented stdout interface with a full-screen terminal user
interface (TUI) built on `ratatui` + `crossterm`, at feature parity with the
existing `tui` branch: a four-pane layout (messages, book list, user list,
input) with scrollbars, mouse support, and live download progress bars.

The TUI is ported **onto the current architecture** (the refactored
`client`/`dcc`/`commands`/`net`/`ui` modules plus the TLS work), reusing the
`tui` branch only as a design reference. The current robust IRC/DCC/TLS logic is
kept; the cruder IRC/DCC parsing on the `tui` branch is **not** adopted.

## Background

Today the IRC receive loop, DCC handlers, and command processing write directly
to stdout through `ui.rs` (`print_line`, `print_received_line`, etc.). A TUI
owns the terminal, so those code paths cannot share stdout. The central change
is to **stop printing and start emitting events** into a state object that a
render loop owns and draws.

## Architecture

### Event flow

```
init task ─┐
write task ─┤ own the socket (unchanged)
receive_loop┘ ──emit──> UiEvent ──┐
dcc_receive / unzip ──emit──>      ├─ mpsc::Sender<UiEvent> ─> render loop ─> App ─> terminal.draw()
tracing layer ──emit──>            ┘
                                  render loop ──outgoing IRC strings──> client.sender ─> write task ─> socket
```

- **One mpsc channel** carries every UI-bound event. Background tasks are
  producers; the render loop is the sole consumer and the sole owner of `App`.
- **Outgoing** IRC messages still flow through the existing `client.sender`
  channel into the `write` task, which owns the socket writer. The render loop
  builds outgoing strings (from user input) and sends them there.

### UiEvent enum (new, in `tui` module)

```rust
enum UiEvent {
    Received(String),                 // raw RX line from the server
    System(String),                   // app-generated status text
    Log(MessageType, String),         // forwarded tracing record
    BookList(Vec<String>),            // SearchBot results loaded
    UserList(Vec<String>),            // RPL_NAMREPLY (353)
    UserJoined(String),
    UserLeft(String),
    DownloadStarted { filename: String, size: u64 },
    DownloadProgress { filename: String, received: u64 },
    DownloadCompleted(String),
    DownloadFailed { filename: String, error: String },
    DownloadExtracting(String),
    DownloadExtracted(String),
    Connected,
    Disconnected,
}
```

### New module: `tui`

Adapted from the `tui` branch's `ui.rs`. Contains:

- `App` — all UI state: `messages: Vec<Message>`, `book_list`, `user_list`,
  `downloads: Vec<DownloadProgress>`, `input`, `cursor_position`, scroll state
  per pane (`ScrollbarState`), `active_panel`, `connected`, `current_channel`,
  `should_quit`, and `config`.
- `Message` + `MessageType` (Sent/Received/System/Info/Debug/Error) with
  timestamped formatting.
- `DownloadProgress` + `DownloadStatus` (Starting/InProgress/Completed/
  Failed/Extracting) with percent computation for the gauge.
- Terminal lifecycle: `setup_terminal` (raw mode, alternate screen, mouse
  capture), `cleanup_terminal` (reverse).
- Event handling: `handle_events` (poll ~10ms), `handle_key_event`,
  `handle_mouse_event` (click-to-focus pane, wheel scroll).
- `render_ui` — four-pane layout + server-info pane + download gauges +
  help line + scrollbars.
- `apply_event(&mut App, UiEvent)` — mutates state from a drained event.

### Changes to existing modules (logic preserved, side effects rerouted)

- **`client.rs`**: `IrcClient` gains `ui_tx: Sender<UiEvent>`.
  - `receive_loop` sends `Received` instead of `print_received_line`; parses
    `353` → `UserList`, `JOIN`/`PART`/`QUIT` → `UserJoined`/`UserLeft`. PING/PONG
    and DCC dispatch logic unchanged.
  - `handle_dcc_file` emits `BookList(entries)` for SearchBot results instead of
    printing the numbered list.
  - The stdin `cli` task is **removed**; input is handled by the render loop.
  - `init`/`write`/`receive_loop` are otherwise unchanged. The `exit(0)` QUIT
    short-circuit in `write` is removed (see Shutdown).
- **`dcc.rs`**: `dcc_receive` takes a `ui_tx` and emits `DownloadStarted`, then
  `DownloadProgress` (throttled — at most once per ~1% or ~200ms), then
  `DownloadCompleted`/`DownloadFailed`. `unzip_file` emits
  `DownloadExtracting`/`DownloadExtracted`. Streaming + ACK logic untouched.
- **`commands.rs`**: `process_command` stays a pure `input → Option<IRC string>`
  mapper. The `/<n>` book-number lookup moves into the render loop (which owns
  `book_list`); selecting entry _n_ sends the corresponding `book_list[n]` line.
- **`ui.rs`** (current stdout printer): removed/absorbed; its role is replaced by
  the `tui` module. Any remaining shared helpers move into `tui`.

### Logging

A custom `tracing` writer (a `MakeWriter`) forwards formatted log records as
`UiEvent::Log(level, text)` into the message pane, color-coded by level. This
keeps logs visible without corrupting the alternate screen. The current
`tracing_subscriber::fmt()` stdout setup in `main` is replaced by this layer.

### Main loop & shutdown

`main`:
1. Build config (unchanged), then the `UiEvent` channel and the tracing layer.
2. `setup_terminal()`; install a **panic hook** that restores the terminal
   before the default hook runs (so a panic never leaves a broken shell).
3. `IrcClient::new` (with `ui_tx`), then spawn `init`, `write`, `receive_loop`.
4. Run the render loop on the main task:
   - `handle_events` → on submitted input, interpret and send outgoing IRC
     string(s) via `client.sender`; push a `Sent` message.
   - Drain all pending `UiEvent`s into `App`.
   - `terminal.draw(render_ui)`.
   - `tokio::time::sleep(16ms)` (~60fps) to avoid busy-spin.
   - Exit when `app.should_quit` (set by `/quit`, `/q`, `Ctrl+Q`) or on
     `Disconnected`.
5. On exit: `cleanup_terminal()`, then drain in-flight DCC tasks
   (`drain_dcc_tasks`) so downloads finish before the process ends. This
   **replaces** the `exit(0)`-based shutdown; the drain-before-exit guarantee is
   preserved and no longer skips terminal cleanup.

### Dependencies

Add `ratatui` and `crossterm` (current major versions). `tokio-rustls`/TLS and
all existing dependencies stay.

## Keybindings & mouse (parity with `tui` branch)

- Type to edit input; Left/Right/Home-equivalent cursor moves; Backspace/Delete.
- Enter submits the input line.
- PageUp/PageDown and Ctrl+Up/Ctrl+Down scroll the active pane; Home/End jump to
  oldest/newest messages.
- Mouse: left-click focuses a pane (and positions the input cursor); wheel
  scrolls the focused pane.
- Ctrl+Q quits.

## Testing

The TUI render/terminal code is inherently hard to unit-test, but the ported
state logic is not. Unit tests (following TDD where logic is non-trivial):

- `App` scrolling math: `scroll_up`/`scroll_down`/`update_max_scroll` clamp
  correctly at top/bottom and when content is shorter than the viewport.
- `apply_event`: `BookList`/`UserList` replace state; `UserJoined`/`UserLeft`
  add/remove without duplicates; download events transition `DownloadStatus`
  through Starting → InProgress → Completed/Failed.
- `DownloadProgress` percent computation (including the zero-size edge case).
- `/<n>` selection maps to the correct `book_list` entry and is a no-op when out
  of range.
- Existing 23 tests continue to pass; the IRC/DCC/TLS/config logic is unchanged.

Terminal setup/teardown, mouse hit-testing, and `render_ui` are verified
manually.

## Out of scope / non-goals

- No change to IRC/DCC/TLS protocol logic beyond rerouting side effects.
- No SASL, multi-channel, or message persistence.
- No adoption of the `tui` branch's `irc.rs`/`download.rs` parsing.

## Risks

- **Blocking input poll inside async:** `crossterm::event::poll` briefly blocks a
  worker thread. Acceptable on the multi-thread tokio runtime (the `tui` branch
  ships this). Alternative `crossterm::EventStream` + `tokio::select!` is noted
  but not used, to keep the proven simple loop.
- **Terminal restoration on error/panic:** mitigated by the panic hook and by
  running cleanup on every exit path.
