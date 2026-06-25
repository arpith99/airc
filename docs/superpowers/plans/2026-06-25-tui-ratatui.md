# ratatui TUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace airc's line-oriented stdout interface with a full-screen `ratatui` + `crossterm` TUI (message/book/user/input panes, scrollbars, mouse, live download gauges) while keeping the existing IRC/DCC/TLS logic intact.

**Architecture:** Background tasks (`init`/`write`/`receive_loop`/DCC) stop printing and instead emit `UiEvent`s onto a single `mpsc` channel. A render loop on the main task owns the `App` state, drains events via `apply_event`, draws the UI, and routes user input back through the existing `client.send`/`write` path. Logs are forwarded into the message pane through a custom `tracing` `MakeWriter`.

**Tech Stack:** Rust 2024, tokio, ratatui 0.29, crossterm 0.28, tracing, chrono.

**Spec:** `docs/superpowers/specs/2026-06-25-tui-ratatui-design.md`

---

## File Structure

New `tui` directory module (focused files, each one responsibility):

- `src/tui/mod.rs` — `UiEvent` enum, `apply_event`, the `run` render loop, input dispatch. Module glue.
- `src/tui/app.rs` — `App` state, `Message`/`MessageType`, `ActivePanel`, `Areas`, `RectExt`, scroll math, key/mouse handling.
- `src/tui/download.rs` — `DownloadProgress`/`DownloadStatus` (gauge state).
- `src/tui/render.rs` — `setup_terminal`/`cleanup_terminal`/`install_panic_hook`, `render_ui` + pane renderers.
- `src/tui/log_writer.rs` — `UiMakeWriter` tracing writer + level→`MessageType` mapping.

Modified existing files:

- `src/client.rs` — add `ui_tx` field + `send` method; rewrite `write`/`receive_loop`; emit events from DCC handlers; remove `cli`, `search_results`, `exit(0)`; add IRC parse helpers.
- `src/dcc.rs` — thread `ui_tx` into `dcc_receive`/`unzip_file`; emit download events; drop prints.
- `src/commands.rs` — make `process_command` a pure `(input, channel) -> Option<String>`; add `entry_number`/`local_search_term`; expose `contains_ignore_case`; drop `handle_search_results`.
- `src/main.rs` — render-loop orchestration; tracing-into-UI wiring; remove `cli` task and `ui` module.
- `src/Cargo.toml` — add `ratatui`/`crossterm`; remove `colored`.
- `src/ui.rs` — **deleted** (replaced by `tui`).
- `README.md`, `Requirements.txt` — doc refresh.

**Key types (defined once, referenced everywhere):**

```rust
// src/tui/mod.rs
pub(crate) enum UiEvent {
    Received(String),
    System(String),
    Log(MessageType, String),
    BookList(Vec<String>),
    UserList(Vec<String>),
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

`MessageType` (in `app.rs`): `Sent | Received | System | Info | Debug | Error`.
`DownloadStatus` (in `download.rs`): `Starting | InProgress | Completed | Failed(String) | Extracting`.

**Build-order note:** Tasks 1–8 add new, independently-compiling code (the `tui` module + pure helpers) and stay green/committable. Tasks 9–11 are the coordinated "switch" — `cargo build` will not pass until all three land, so they share one verification + commit at the end of Task 11. Task 12 finalizes docs.

---

## Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add ratatui + crossterm**

Edit `Cargo.toml` `[dependencies]`: add the two TUI crates. **Keep `colored = "3.0"` for now** — `src/ui.rs` still imports it and is not deleted until Task 11, so removing it here would break the build. `colored` is removed in Task 11 alongside `ui.rs`. Resulting block:

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
rand = "0.9"
regex = "1"
clap = { version = "4", features = ["derive"] }
zip = "2"
chrono = "0.4"
once_cell = "1.19"
thiserror = "1.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
serde = { version = "1.0", features = ["derive"] }
toml = "0.8"
colored = "3.0"
directories = "5.0"
tokio-rustls = { version = "0.26.4", default-features = false, features = ["ring", "tls12", "logging"] }
webpki-roots = "1.0.8"
ratatui = "0.29"
crossterm = "0.28"
```

- [ ] **Step 2: Fetch and verify**

Run: `cargo fetch`
Expected: ratatui 0.29.x and crossterm 0.28.x resolve and download with no error. (`cargo build` will still pass at this point because nothing references the new crates yet and `colored` is still imported only by `ui.rs`, which is unchanged.)

Run: `cargo build`
Expected: builds clean (a warning about unused `colored` is acceptable here and disappears in Task 11).

- [ ] **Step 3: Commit**

```bash
jj commit -m "Add ratatui and crossterm dependencies"
```

---

## Task 2: Download progress state (`tui/download.rs`)

**Files:**
- Create: `src/tui/download.rs`
- Create: `src/tui/mod.rs` (minimal, to register the submodule)
- Modify: `src/main.rs` (register `mod tui;`)

- [ ] **Step 1: Register the module so tests can run**

Add to `src/main.rs` module list (keep alphabetical, after `mod net;`):

```rust
mod tui;
```

Create `src/tui/mod.rs` with just the submodule declaration for now:

```rust
mod download;
```

- [ ] **Step 2: Write the failing test**

Create `src/tui/download.rs`:

```rust
//! Per-download progress state backing the TUI download gauges.

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DownloadStatus {
    Starting,
    InProgress,
    Completed,
    Failed(String),
    Extracting,
}

#[derive(Debug, Clone)]
pub(crate) struct DownloadProgress {
    pub filename: String,
    pub progress: u16, // 0-100, drives the Gauge widget
    pub total_size: u64,
    pub current_size: u64,
    pub status: DownloadStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_percent_half() {
        let mut d = DownloadProgress::new("a.txt".to_string(), 200);
        d.update_progress(100);
        assert_eq!(d.progress, 50);
        assert_eq!(d.status, DownloadStatus::InProgress);
    }

    #[test]
    fn test_progress_percent_zero_size() {
        // A zero-size download must never divide by zero; percent stays 0.
        let mut d = DownloadProgress::new("empty".to_string(), 0);
        d.update_progress(0);
        assert_eq!(d.progress, 0);
    }

    #[test]
    fn test_progress_clamps_over_100() {
        let mut d = DownloadProgress::new("a".to_string(), 100);
        d.update_progress(150);
        assert_eq!(d.progress, 100);
    }

    #[test]
    fn test_mark_completed_sets_full() {
        let mut d = DownloadProgress::new("a".to_string(), 100);
        d.mark_completed();
        assert_eq!(d.progress, 100);
        assert_eq!(d.status, DownloadStatus::Completed);
    }

    #[test]
    fn test_mark_failed_and_extracting() {
        let mut d = DownloadProgress::new("a".to_string(), 100);
        d.mark_failed("boom".to_string());
        assert_eq!(d.status, DownloadStatus::Failed("boom".to_string()));
        d.mark_extracting();
        assert_eq!(d.status, DownloadStatus::Extracting);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib tui::download`
Expected: FAIL — `no function or associated item named 'new' found` (the `impl` block doesn't exist yet).

- [ ] **Step 4: Write minimal implementation**

Insert the `impl` block in `src/tui/download.rs` between the struct definition and the `#[cfg(test)]` module:

```rust
impl DownloadProgress {
    pub fn new(filename: String, total_size: u64) -> Self {
        Self {
            filename,
            progress: 0,
            total_size,
            current_size: 0,
            status: DownloadStatus::Starting,
        }
    }

    pub fn update_progress(&mut self, current_size: u64) {
        self.current_size = current_size;
        if self.total_size > 0 {
            let pct = ((current_size as f64 / self.total_size as f64) * 100.0).round() as u16;
            self.progress = pct.min(100);
        }
        self.status = DownloadStatus::InProgress;
    }

    pub fn mark_completed(&mut self) {
        self.status = DownloadStatus::Completed;
        self.progress = 100;
    }

    pub fn mark_failed(&mut self, error: String) {
        self.status = DownloadStatus::Failed(error);
    }

    pub fn mark_extracting(&mut self) {
        self.status = DownloadStatus::Extracting;
    }
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib tui::download`
Expected: PASS (5 tests). Warnings about unused items elsewhere are fine for now.

- [ ] **Step 6: Commit**

```bash
jj commit -m "Add DownloadProgress state for TUI gauges"
```

---

## Task 3: App state + scroll math (`tui/app.rs`)

**Files:**
- Create: `src/tui/app.rs`
- Modify: `src/tui/mod.rs`

- [ ] **Step 1: Register submodule**

Update `src/tui/mod.rs`:

```rust
pub(crate) mod app;
mod download;
```

- [ ] **Step 2: Write the App with a failing test**

Create `src/tui/app.rs` with the full state type. (This is a new file; the complete content follows. Scroll/user-list logic is exercised by the tests at the bottom.)

```rust
//! TUI application state: messages, panes, scrolling, and input handling.

use std::time::Duration;

use chrono::{DateTime, Local};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::widgets::ScrollbarState;

use crate::config::Config;
use crate::error::Result;
use super::download::DownloadProgress;

/// Maximum messages retained before the oldest are dropped.
const MAX_MESSAGE_HISTORY: usize = 1000;
/// Lines moved per PageUp/PageDown.
const PAGE_SCROLL_AMOUNT: usize = 5;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MessageType {
    Sent,
    Received,
    System,
    Info,
    Debug,
    Error,
}

#[derive(Clone, Debug)]
pub(crate) struct Message {
    pub content: String,
    pub timestamp: DateTime<Local>,
    pub message_type: MessageType,
}

impl Message {
    pub fn new(content: String, message_type: MessageType) -> Self {
        Self {
            content,
            timestamp: Local::now(),
            message_type,
        }
    }

    pub fn formatted(&self) -> String {
        let prefix = match self.message_type {
            MessageType::Sent => "TX",
            MessageType::Received => "RX",
            MessageType::System => "SYS",
            MessageType::Info => "INFO",
            MessageType::Debug => "DEBUG",
            MessageType::Error => "ERROR",
        };
        format!(
            "[{}] {}: {}",
            self.timestamp.format("%H:%M:%S"),
            prefix,
            self.content
        )
    }
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub(crate) enum ActivePanel {
    Messages,
    Books,
    Users,
    Input,
}

/// Pane rectangles captured during render, used for mouse hit-testing.
#[derive(Default, Clone, Copy)]
pub(crate) struct Areas {
    pub message_area: Rect,
    pub book_area: Rect,
    pub user_area: Rect,
    pub input_area: Rect,
}

pub(crate) trait RectExt {
    fn contains_point(&self, x: u16, y: u16) -> bool;
}

impl RectExt for Rect {
    fn contains_point(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

pub(crate) struct App {
    pub should_quit: bool,
    pub quit_sent: bool,
    pub input: String,
    pub messages: Vec<Message>,
    pub book_list: Vec<String>,
    pub user_list: Vec<String>,
    pub cursor_position: usize,
    pub downloads: Vec<DownloadProgress>,
    pub current_channel: String,
    pub connected: bool,

    pub message_scroll: usize,
    pub max_scroll: usize,
    pub message_scroll_state: ScrollbarState,
    pub book_scroll: usize,
    pub book_scroll_state: ScrollbarState,
    pub user_scroll: usize,
    pub user_scroll_state: ScrollbarState,

    pub active_panel: ActivePanel,
    pub config: Config,
}

impl App {
    pub fn new(config: Config) -> Self {
        Self {
            should_quit: false,
            quit_sent: false,
            input: String::new(),
            messages: Vec::new(),
            book_list: Vec::new(),
            user_list: Vec::new(),
            cursor_position: 0,
            downloads: Vec::new(),
            current_channel: config.channel.clone(),
            connected: false,
            message_scroll: 0,
            max_scroll: 0,
            message_scroll_state: ScrollbarState::default(),
            book_scroll: 0,
            book_scroll_state: ScrollbarState::default(),
            user_scroll: 0,
            user_scroll_state: ScrollbarState::default(),
            active_panel: ActivePanel::Input,
            config,
        }
    }

    pub fn add_message_with_type(&mut self, content: String, message_type: MessageType) {
        self.messages.push(Message::new(content, message_type));
        if self.messages.len() > MAX_MESSAGE_HISTORY {
            self.messages.remove(0);
        }
        self.scroll_to_bottom();
    }

    pub fn add_user(&mut self, user: String) {
        if !self.user_list.contains(&user) {
            self.user_list.push(user);
        }
    }

    pub fn remove_user(&mut self, user: &str) {
        self.user_list.retain(|u| u != user);
    }

    pub fn set_users(&mut self, users: Vec<String>) {
        self.user_list = users;
    }

    pub fn add_download(&mut self, filename: String, size: u64) {
        if let Some(d) = self.downloads.iter_mut().find(|d| d.filename == filename) {
            *d = DownloadProgress::new(filename, size);
        } else {
            self.downloads.push(DownloadProgress::new(filename, size));
        }
    }

    pub fn update_download(&mut self, filename: &str, current: u64) {
        if let Some(d) = self.downloads.iter_mut().find(|d| d.filename == filename) {
            d.update_progress(current);
        }
    }

    pub fn complete_download(&mut self, filename: &str) {
        if let Some(d) = self.downloads.iter_mut().find(|d| d.filename == filename) {
            d.mark_completed();
        }
    }

    pub fn fail_download(&mut self, filename: &str, error: String) {
        if let Some(d) = self.downloads.iter_mut().find(|d| d.filename == filename) {
            d.mark_failed(error);
        }
    }

    pub fn mark_download_extracting(&mut self, filename: &str) {
        if let Some(d) = self.downloads.iter_mut().find(|d| d.filename == filename) {
            d.mark_extracting();
        }
    }

    // --- Message-pane scrolling (scroll value counts lines back from newest) ---

    pub fn scroll_to_bottom(&mut self) {
        self.message_scroll = 0;
        self.update_message_scrollbar();
    }

    pub fn update_max_scroll(&mut self, viewport_height: usize) {
        self.max_scroll = self.messages.len().saturating_sub(viewport_height);
        if self.message_scroll > self.max_scroll {
            self.message_scroll = self.max_scroll;
        }
    }

    pub fn scroll_up(&mut self, amount: usize) {
        self.message_scroll = (self.message_scroll + amount).min(self.max_scroll);
        self.update_message_scrollbar();
    }

    pub fn scroll_down(&mut self, amount: usize) {
        self.message_scroll = self.message_scroll.saturating_sub(amount);
        self.update_message_scrollbar();
    }

    pub fn update_message_scrollbar(&mut self) {
        self.message_scroll_state = self
            .message_scroll_state
            .content_length(self.messages.len())
            .position(self.message_scroll);
    }

    // --- Book-list scrolling ---

    pub fn update_book_scroll(&mut self, viewport_height: usize) {
        let max_scroll = self.book_list.len().saturating_sub(viewport_height);
        self.book_scroll = self.book_scroll.min(max_scroll);
        self.book_scroll_state = self.book_scroll_state.content_length(self.book_list.len());
    }

    pub fn scroll_books_up(&mut self, amount: usize) {
        self.book_scroll = self.book_scroll.saturating_sub(amount);
        self.book_scroll_state = self.book_scroll_state.position(self.book_scroll);
    }

    pub fn scroll_books_down(&mut self, amount: usize) {
        let max_scroll = self.book_list.len().saturating_sub(1);
        self.book_scroll = (self.book_scroll + amount).min(max_scroll);
        self.book_scroll_state = self.book_scroll_state.position(self.book_scroll);
    }

    // --- User-list scrolling ---

    pub fn update_user_scroll(&mut self, viewport_height: usize) {
        let max_scroll = self.user_list.len().saturating_sub(viewport_height);
        self.user_scroll = self.user_scroll.min(max_scroll);
        self.user_scroll_state = self.user_scroll_state.content_length(self.user_list.len());
    }

    pub fn scroll_users_up(&mut self, amount: usize) {
        self.user_scroll = self.user_scroll.saturating_sub(amount);
        self.user_scroll_state = self.user_scroll_state.position(self.user_scroll);
    }

    pub fn scroll_users_down(&mut self, amount: usize) {
        let max_scroll = self.user_list.len().saturating_sub(1);
        self.user_scroll = (self.user_scroll + amount).min(max_scroll);
        self.user_scroll_state = self.user_scroll_state.position(self.user_scroll);
    }

    // --- Input / events ---

    pub fn handle_events(&mut self, areas: &Areas) -> Result<Option<String>> {
        if event::poll(Duration::from_millis(10))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    return Ok(self.handle_key_event(key));
                }
                Event::Mouse(mouse) => self.handle_mouse_event(mouse, areas),
                _ => {}
            }
        }
        Ok(None)
    }

    /// Returns Some(line) when the user submits input with Enter.
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Option<String> {
        match key.code {
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
                None
            }
            KeyCode::PageUp => {
                self.scroll_up(PAGE_SCROLL_AMOUNT);
                None
            }
            KeyCode::PageDown => {
                self.scroll_down(PAGE_SCROLL_AMOUNT);
                None
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_up(1);
                None
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_down(1);
                None
            }
            KeyCode::Home => {
                self.message_scroll = self.max_scroll;
                self.update_message_scrollbar();
                None
            }
            KeyCode::End => {
                self.scroll_to_bottom();
                None
            }
            KeyCode::Char(c) => {
                self.input.insert(self.cursor_position, c);
                self.cursor_position += 1;
                None
            }
            KeyCode::Backspace => {
                if self.cursor_position > 0 {
                    self.cursor_position -= 1;
                    self.input.remove(self.cursor_position);
                }
                None
            }
            KeyCode::Delete => {
                if self.cursor_position < self.input.len() {
                    self.input.remove(self.cursor_position);
                }
                None
            }
            KeyCode::Left => {
                self.cursor_position = self.cursor_position.saturating_sub(1);
                None
            }
            KeyCode::Right => {
                if self.cursor_position < self.input.len() {
                    self.cursor_position += 1;
                }
                None
            }
            KeyCode::Enter => {
                if self.input.is_empty() {
                    None
                } else {
                    let input = std::mem::take(&mut self.input);
                    self.cursor_position = 0;
                    self.scroll_to_bottom();
                    Some(input)
                }
            }
            _ => None,
        }
    }

    pub fn handle_mouse_event(&mut self, event: MouseEvent, areas: &Areas) {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if areas.message_area.contains_point(event.column, event.row) {
                    self.active_panel = ActivePanel::Messages;
                } else if areas.book_area.contains_point(event.column, event.row) {
                    self.active_panel = ActivePanel::Books;
                } else if areas.user_area.contains_point(event.column, event.row) {
                    self.active_panel = ActivePanel::Users;
                } else if areas.input_area.contains_point(event.column, event.row) {
                    self.active_panel = ActivePanel::Input;
                    let input_x = event.column.saturating_sub(areas.input_area.x + 1);
                    self.cursor_position = (input_x as usize).min(self.input.len());
                }
            }
            MouseEventKind::ScrollDown => match self.active_panel {
                ActivePanel::Messages => self.scroll_down(1),
                ActivePanel::Books => self.scroll_books_down(1),
                ActivePanel::Users => self.scroll_users_down(1),
                _ => {}
            },
            MouseEventKind::ScrollUp => match self.active_panel {
                ActivePanel::Messages => self.scroll_up(1),
                ActivePanel::Books => self.scroll_books_up(1),
                ActivePanel::Users => self.scroll_users_up(1),
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new(Config::default())
    }

    fn fill_messages(app: &mut App, n: usize) {
        for i in 0..n {
            app.add_message_with_type(format!("m{i}"), MessageType::System);
        }
    }

    #[test]
    fn test_scroll_up_clamps_at_max() {
        let mut a = app();
        fill_messages(&mut a, 20);
        a.update_max_scroll(5); // 20 - 5 = 15
        a.scroll_up(100);
        assert_eq!(a.message_scroll, 15);
    }

    #[test]
    fn test_scroll_down_clamps_at_zero() {
        let mut a = app();
        fill_messages(&mut a, 20);
        a.update_max_scroll(5);
        a.scroll_up(10);
        a.scroll_down(100);
        assert_eq!(a.message_scroll, 0);
    }

    #[test]
    fn test_update_max_scroll_zero_when_content_fits() {
        let mut a = app();
        fill_messages(&mut a, 3);
        a.update_max_scroll(10);
        assert_eq!(a.max_scroll, 0);
    }

    #[test]
    fn test_add_message_resets_scroll_to_bottom() {
        let mut a = app();
        fill_messages(&mut a, 20);
        a.update_max_scroll(5);
        a.scroll_up(5);
        assert_eq!(a.message_scroll, 5);
        a.add_message_with_type("new".to_string(), MessageType::System);
        assert_eq!(a.message_scroll, 0);
    }

    #[test]
    fn test_add_user_dedups() {
        let mut a = app();
        a.add_user("alice".to_string());
        a.add_user("alice".to_string());
        assert_eq!(a.user_list, vec!["alice".to_string()]);
    }

    #[test]
    fn test_remove_user() {
        let mut a = app();
        a.set_users(vec!["alice".to_string(), "bob".to_string()]);
        a.remove_user("alice");
        assert_eq!(a.user_list, vec!["bob".to_string()]);
    }

    #[test]
    fn test_add_download_replaces_same_filename() {
        let mut a = app();
        a.add_download("f".to_string(), 100);
        a.update_download("f", 50);
        a.add_download("f".to_string(), 200); // restart same name
        assert_eq!(a.downloads.len(), 1);
        assert_eq!(a.downloads[0].total_size, 200);
        assert_eq!(a.downloads[0].current_size, 0);
    }

    #[test]
    fn test_history_cap() {
        let mut a = app();
        fill_messages(&mut a, MAX_MESSAGE_HISTORY + 50);
        assert_eq!(a.messages.len(), MAX_MESSAGE_HISTORY);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail first**

Run: `cargo test --lib tui::app`
Expected: the file is written complete, so this compiles and the tests PASS immediately. **This is acceptable only because `app.rs` is a faithful, mechanical port of the proven `tui`-branch state machine.** If any test fails, fix the implementation (not the test) until green. (The scroll tests are the meaningful assertions; confirm they exercise the clamps.)

Run: `cargo test --lib tui::app`
Expected: PASS (8 tests).

- [ ] **Step 4: Commit**

```bash
jj commit -m "Add TUI App state with scroll and input handling"
```

---

## Task 4: Log writer (`tui/log_writer.rs`)

**Files:**
- Create: `src/tui/log_writer.rs`
- Modify: `src/tui/mod.rs`

- [ ] **Step 1: Register submodule + UiEvent stub**

`UiMakeWriter` builds `UiEvent::Log`, so `UiEvent` must exist. Replace `src/tui/mod.rs` with:

```rust
pub(crate) mod app;
mod download;
mod log_writer;

pub(crate) use app::MessageType;

#[derive(Debug, Clone)]
pub(crate) enum UiEvent {
    Received(String),
    System(String),
    Log(MessageType, String),
    BookList(Vec<String>),
    UserList(Vec<String>),
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

- [ ] **Step 2: Write the failing test**

Create `src/tui/log_writer.rs`:

```rust
//! A tracing `MakeWriter` that forwards formatted log lines into the message
//! pane as `UiEvent::Log`, color-coded by level.

use std::io::Write;

use tokio::sync::mpsc::Sender;
use tracing_subscriber::fmt::MakeWriter;

use super::app::MessageType;
use super::UiEvent;

/// Classify a formatted log line by the level token tracing emits.
pub(crate) fn message_type_from_log_line(line: &str) -> MessageType {
    if line.contains("ERROR") || line.contains("WARN") {
        MessageType::Error
    } else if line.contains("DEBUG") || line.contains("TRACE") {
        MessageType::Debug
    } else {
        MessageType::Info
    }
}

#[derive(Clone)]
pub(crate) struct UiMakeWriter {
    tx: Sender<UiEvent>,
}

impl UiMakeWriter {
    pub fn new(tx: Sender<UiEvent>) -> Self {
        Self { tx }
    }
}

pub(crate) struct UiLogWriter {
    tx: Sender<UiEvent>,
}

impl Write for UiLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf).trim_end().to_string();
        if !text.is_empty() {
            let mt = message_type_from_log_line(&text);
            // Best-effort: drop the line rather than block the logging path.
            let _ = self.tx.try_send(UiEvent::Log(mt, text));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for UiMakeWriter {
    type Writer = UiLogWriter;
    fn make_writer(&'a self) -> Self::Writer {
        UiLogWriter {
            tx: self.tx.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_and_warn_map_to_error() {
        assert_eq!(
            message_type_from_log_line("2026 ERROR airc: boom"),
            MessageType::Error
        );
        assert_eq!(
            message_type_from_log_line("2026 WARN airc: careful"),
            MessageType::Error
        );
    }

    #[test]
    fn test_debug_and_trace_map_to_debug() {
        assert_eq!(
            message_type_from_log_line("2026 DEBUG airc: detail"),
            MessageType::Debug
        );
        assert_eq!(
            message_type_from_log_line("2026 TRACE airc: noisy"),
            MessageType::Debug
        );
    }

    #[test]
    fn test_info_is_default() {
        assert_eq!(
            message_type_from_log_line("2026 INFO airc: hello"),
            MessageType::Info
        );
    }
}
```

- [ ] **Step 3: Run test to verify it fails, then passes**

Run: `cargo test --lib tui::log_writer`
Expected: This compiles (complete file) and PASSES (3 tests). The meaningful logic under test is `message_type_from_log_line`. If a test fails, fix the mapping.

- [ ] **Step 4: Commit**

```bash
jj commit -m "Add tracing-to-message-pane log writer"
```

---

## Task 5: Rendering + terminal lifecycle (`tui/render.rs`)

**Files:**
- Create: `src/tui/render.rs`
- Modify: `src/tui/mod.rs`

Rendering is not unit-tested (it needs a real terminal); this task is build-verified.

- [ ] **Step 1: Register submodule**

Add to `src/tui/mod.rs` (top, with the other `mod` lines):

```rust
mod render;
```

- [ ] **Step 2: Write the renderer**

Create `src/tui/render.rs` (complete content — port of the proven layout):

```rust
//! Terminal setup/teardown and the four-pane render pass.

use std::io::{self, Stdout};

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Gauge, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, Wrap,
};
use ratatui::{Frame, Terminal};

use crate::error::Result;
use super::app::{ActivePanel, App, Areas, MessageType};
use super::download::DownloadStatus;

pub(crate) type Tui = Terminal<CrosstermBackend<Stdout>>;

pub(crate) fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

pub(crate) fn cleanup_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Restore the terminal from a panic before the default hook prints the message,
/// so a crash never leaves the user in a broken alternate screen.
pub(crate) fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original(info);
    }));
}

pub(crate) fn render_ui(f: &mut Frame, app: &mut App) -> Areas {
    let size = f.area();

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(4)])
        .split(size);
    let main_area = vertical[0];
    let footer_area = vertical[1];

    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(10),
            Constraint::Percentage(80),
            Constraint::Percentage(10),
        ])
        .split(main_area);

    let middle_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(main_chunks[1]);

    let bottom_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(middle_chunks[1]);

    let footer_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Length(1)])
        .split(footer_area);

    let areas = Areas {
        message_area: bottom_chunks[2],
        book_area: middle_chunks[0],
        user_area: main_chunks[2],
        input_area: footer_chunks[0],
    };

    render_server_info(f, app, main_chunks[0]);
    render_book_list(f, app, areas.book_area);
    render_user_list(f, app, areas.user_area);
    render_download_progress(f, app, &bottom_chunks[0..2]);
    render_message_log(f, app, areas.message_area);
    render_input_box(f, app, areas.input_area);
    render_help_text(f, footer_chunks[1]);

    areas
}

fn vertical_scrollbar(f: &mut Frame, area: Rect, state: &mut ratatui::widgets::ScrollbarState) {
    let scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .begin_symbol(Some("↑"))
        .end_symbol(Some("↓"));
    f.render_stateful_widget(
        scrollbar,
        Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        },
        state,
    );
}

fn render_server_info(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(format!("Server: {}", app.config.server))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue));
    let status = if app.connected {
        "Connected"
    } else {
        "Disconnected"
    };
    let items = vec![
        ListItem::new(status),
        ListItem::new(app.current_channel.clone()),
        ListItem::new("/join to join"),
    ];
    let list = List::new(items)
        .block(block)
        .style(Style::default().fg(Color::Cyan));
    f.render_widget(list, area);
}

fn render_book_list(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .title("Book list")
        .borders(Borders::ALL)
        .border_style(if app.active_panel == ActivePanel::Books {
            Style::default().fg(Color::LightBlue)
        } else {
            Style::default().fg(Color::LightYellow)
        });

    let viewport_height = (area.height as usize).saturating_sub(2);
    app.update_book_scroll(viewport_height);

    let items: Vec<ListItem> = app
        .book_list
        .iter()
        .skip(app.book_scroll)
        .take(viewport_height)
        .map(|b| ListItem::new(b.as_str()))
        .collect();

    let list = List::new(items)
        .block(block)
        .style(Style::default().fg(Color::LightYellow));
    f.render_widget(list, area);

    if !app.book_list.is_empty() {
        vertical_scrollbar(f, area, &mut app.book_scroll_state);
    }
}

fn render_user_list(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .title("Users")
        .borders(Borders::ALL)
        .border_style(if app.active_panel == ActivePanel::Users {
            Style::default().fg(Color::LightBlue)
        } else {
            Style::default().fg(Color::Red)
        });

    let viewport_height = (area.height as usize).saturating_sub(2);
    app.update_user_scroll(viewport_height);

    let items: Vec<ListItem> = app
        .user_list
        .iter()
        .skip(app.user_scroll)
        .take(viewport_height)
        .map(|u| ListItem::new(u.as_str()))
        .collect();

    let list = List::new(items)
        .block(block)
        .style(Style::default().fg(Color::Red));
    f.render_widget(list, area);

    if !app.user_list.is_empty() {
        vertical_scrollbar(f, area, &mut app.user_scroll_state);
    }
}

fn render_download_progress(f: &mut Frame, app: &App, areas: &[Rect]) {
    let downloads: Vec<_> = app.downloads.iter().rev().take(areas.len()).collect();
    for (i, area) in areas.iter().enumerate() {
        if let Some(download) = downloads.get(i) {
            let status = match &download.status {
                DownloadStatus::Starting => "Starting",
                DownloadStatus::InProgress => "In Progress",
                DownloadStatus::Completed => "Completed",
                DownloadStatus::Failed(_) => "Failed",
                DownloadStatus::Extracting => "Extracting",
            };
            let block = Block::default()
                .title(format!("{} ({})", download.filename, status))
                .borders(Borders::ALL)
                .border_style(match &download.status {
                    DownloadStatus::Completed => Style::default().fg(Color::Green),
                    DownloadStatus::Failed(_) => Style::default().fg(Color::Red),
                    DownloadStatus::Extracting => Style::default().fg(Color::Yellow),
                    _ => Style::default().fg(Color::Blue),
                });
            let gauge = Gauge::default()
                .block(block)
                .gauge_style(Style::default().fg(Color::Green))
                .percent(download.progress);
            f.render_widget(gauge, *area);
        } else {
            let block = Block::default()
                .title("No active downloads")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray));
            f.render_widget(block, *area);
        }
    }
}

fn render_message_log(f: &mut Frame, app: &mut App, area: Rect) {
    let viewport = (area.height as usize).saturating_sub(2);
    app.update_max_scroll(viewport);
    app.update_message_scrollbar();

    let total = app.messages.len();
    let visible: &[super::app::Message] = if total == 0 {
        &[]
    } else if total <= viewport {
        &app.messages[..]
    } else {
        let end = total - app.message_scroll;
        let start = end.saturating_sub(viewport);
        &app.messages[start..end]
    };

    let title = if app.message_scroll > 0 {
        format!("Messages (↑ {}/{})", app.message_scroll, app.max_scroll)
    } else {
        "Messages".to_string()
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(if app.active_panel == ActivePanel::Messages {
            Style::default().fg(Color::LightBlue)
        } else {
            Style::default().fg(Color::Magenta)
        });

    let lines: Vec<Line> = visible
        .iter()
        .map(|m| {
            let color = match m.message_type {
                MessageType::Sent => Color::Green,
                MessageType::Received => Color::Yellow,
                MessageType::System => Color::White,
                MessageType::Info => Color::Cyan,
                MessageType::Debug => Color::Gray,
                MessageType::Error => Color::Red,
            };
            Line::from(Span::styled(m.formatted(), Style::default().fg(color)))
        })
        .collect();

    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);

    if !app.messages.is_empty() {
        vertical_scrollbar(f, area, &mut app.message_scroll_state);
    }
}

fn render_input_box(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title("Input")
        .borders(Borders::ALL)
        .border_style(if app.active_panel == ActivePanel::Input {
            Style::default().fg(Color::LightBlue)
        } else {
            Style::default().fg(Color::Green)
        });
    let input = Paragraph::new(app.input.as_str())
        .style(Style::default().fg(Color::White))
        .block(block);
    f.render_widget(input, area);
    f.set_cursor_position(Position {
        x: area.x + 1 + app.cursor_position as u16,
        y: area.y + 1,
    });
}

fn render_help_text(f: &mut Frame, area: Rect) {
    let help = vec![
        Span::styled("/s query", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": search | "),
        Span::styled("/N", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": request | "),
        Span::styled("PgUp/PgDn/Home/End", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": scroll | "),
        Span::styled("Ctrl+Q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": quit"),
    ];
    let paragraph = Paragraph::new(Line::from(help)).style(Style::default().fg(Color::White));
    f.render_widget(paragraph, area);
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build`
Expected: compiles. Dead-code/unused warnings for not-yet-wired `tui` items (`render_ui`, `setup_terminal`, etc.) are expected and clear up in Task 11. If there are **errors** (e.g. a ratatui 0.29 API mismatch), fix the call site to match the installed version.

- [ ] **Step 4: Commit**

```bash
jj commit -m "Add TUI rendering and terminal lifecycle"
```

---

## Task 6: Event application + render loop (`tui/mod.rs`)

**Files:**
- Modify: `src/tui/mod.rs`

`apply_event` is pure state mutation and is TDD'd. `run`/`handle_input` reference `IrcClient::send`, `process_command`, `entry_number`, `local_search_term`, and `contains_ignore_case`, which do not exist yet — so `run`/`handle_input` are added in Task 9's switch, NOT here. This task adds `apply_event` and its tests only.

- [ ] **Step 1: Write the failing test**

Append to `src/tui/mod.rs` (after the `UiEvent` enum), pulling `App` and `DownloadStatus` into scope:

```rust
use app::App;

pub(crate) fn apply_event(app: &mut App, event: UiEvent) {
    match event {
        UiEvent::Received(line) => {
            app.add_message_with_type(line.trim_end().to_string(), MessageType::Received)
        }
        UiEvent::System(text) => app.add_message_with_type(text, MessageType::System),
        UiEvent::Log(mt, text) => app.add_message_with_type(text, mt),
        UiEvent::BookList(books) => app.book_list = books,
        UiEvent::UserList(users) => app.set_users(users),
        UiEvent::UserJoined(user) => app.add_user(user),
        UiEvent::UserLeft(user) => app.remove_user(&user),
        UiEvent::DownloadStarted { filename, size } => app.add_download(filename, size),
        UiEvent::DownloadProgress { filename, received } => app.update_download(&filename, received),
        UiEvent::DownloadCompleted(filename) => app.complete_download(&filename),
        UiEvent::DownloadFailed { filename, error } => {
            app.fail_download(&filename, error.clone());
            app.add_message_with_type(
                format!("Download failed: {} - {}", filename, error),
                MessageType::Error,
            );
        }
        UiEvent::DownloadExtracting(filename) => app.mark_download_extracting(&filename),
        UiEvent::DownloadExtracted(filename) => {
            app.add_message_with_type(format!("Extracted: {}", filename), MessageType::System)
        }
        UiEvent::Connected => {
            app.connected = true;
            app.add_message_with_type("Connected".to_string(), MessageType::System);
        }
        UiEvent::Disconnected => {
            app.connected = false;
            app.should_quit = true;
            app.add_message_with_type("Disconnected from server".to_string(), MessageType::System);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use app::App;
    use download::DownloadStatus;

    fn app() -> App {
        App::new(Config::default())
    }

    #[test]
    fn test_book_list_replaces() {
        let mut a = app();
        a.book_list = vec!["old".to_string()];
        apply_event(&mut a, UiEvent::BookList(vec!["x".to_string(), "y".to_string()]));
        assert_eq!(a.book_list, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn test_user_list_join_and_part() {
        let mut a = app();
        apply_event(&mut a, UiEvent::UserList(vec!["alice".to_string()]));
        apply_event(&mut a, UiEvent::UserJoined("bob".to_string()));
        apply_event(&mut a, UiEvent::UserJoined("bob".to_string())); // dedup
        assert_eq!(a.user_list, vec!["alice".to_string(), "bob".to_string()]);
        apply_event(&mut a, UiEvent::UserLeft("alice".to_string()));
        assert_eq!(a.user_list, vec!["bob".to_string()]);
    }

    #[test]
    fn test_download_lifecycle() {
        let mut a = app();
        apply_event(
            &mut a,
            UiEvent::DownloadStarted { filename: "f".to_string(), size: 100 },
        );
        assert_eq!(a.downloads[0].status, DownloadStatus::Starting);
        apply_event(
            &mut a,
            UiEvent::DownloadProgress { filename: "f".to_string(), received: 50 },
        );
        assert_eq!(a.downloads[0].status, DownloadStatus::InProgress);
        assert_eq!(a.downloads[0].progress, 50);
        apply_event(&mut a, UiEvent::DownloadCompleted("f".to_string()));
        assert_eq!(a.downloads[0].status, DownloadStatus::Completed);
    }

    #[test]
    fn test_download_failed_sets_status_and_message() {
        let mut a = app();
        apply_event(
            &mut a,
            UiEvent::DownloadStarted { filename: "f".to_string(), size: 100 },
        );
        apply_event(
            &mut a,
            UiEvent::DownloadFailed { filename: "f".to_string(), error: "boom".to_string() },
        );
        assert_eq!(
            a.downloads[0].status,
            DownloadStatus::Failed("boom".to_string())
        );
        assert!(a.messages.iter().any(|m| m.content.contains("boom")));
    }

    #[test]
    fn test_disconnected_requests_quit() {
        let mut a = app();
        apply_event(&mut a, UiEvent::Disconnected);
        assert!(a.should_quit);
        assert!(!a.connected);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail then pass**

Run: `cargo test --lib tui::tests`
Expected: compiles and PASSES (5 tests). If a transition assertion fails, fix `apply_event`.

- [ ] **Step 3: Commit**

```bash
jj commit -m "Add apply_event for TUI state transitions"
```

---

## Task 7: Pure command helpers (`commands.rs`)

**Files:**
- Modify: `src/commands.rs`

Make `process_command` a pure `(input, channel) -> Option<String>`, add `entry_number`/`local_search_term`, and expose `contains_ignore_case`. Drop `handle_search_results` (its job — local `/ss` filtering — moves to the render loop, which owns `book_list`). The old `process_command` printed and looked up entry numbers via the client; both responsibilities move to the render loop.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/commands.rs`:

```rust
    #[test]
    fn test_process_command_join_and_quit() {
        assert_eq!(
            process_command("/join", "#bookz"),
            Some("JOIN #bookz\r\n".to_string())
        );
        assert_eq!(process_command("/q", "#bookz"), Some("QUIT\r\n".to_string()));
        assert_eq!(
            process_command("/quit bye now", "#bookz"),
            Some("QUIT :bye now\r\n".to_string())
        );
    }

    #[test]
    fn test_process_command_search_and_raw() {
        assert_eq!(
            process_command("/s rust", "#bookz"),
            Some("PRIVMSG #bookz :@search rust\r\n".to_string())
        );
        assert_eq!(
            process_command("/whois bob", "#bookz"),
            Some("whois bob\r\n".to_string())
        );
    }

    #[test]
    fn test_entry_number() {
        assert_eq!(entry_number("/5"), Some(5));
        assert_eq!(entry_number("/0"), Some(0));
        assert_eq!(entry_number("/abc"), None);
        assert_eq!(entry_number("5"), None);
    }

    #[test]
    fn test_local_search_term() {
        assert_eq!(local_search_term("/ss async"), Some("async"));
        assert_eq!(local_search_term("/s async"), None);
        assert_eq!(local_search_term("hello"), None);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib commands`
Expected: FAIL to compile — `process_command` currently takes `(Arc<IrcClient>, &str)` and is `async`; `entry_number`/`local_search_term` don't exist.

- [ ] **Step 3: Rewrite the module body**

Replace the entire non-test portion of `src/commands.rs` (everything above `#[cfg(test)]`) with:

```rust
use once_cell::sync::Lazy;
use regex::Regex;

// Compile regexes once at startup
static SEARCH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/s(earch)? (?P<search_term>.*)").unwrap());

static ENTRY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^/(?P<entry_num>\d+)$").unwrap());

/// Case-insensitive substring search without allocating per-char.
pub(crate) fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let haystack_lower: String = haystack.chars().flat_map(|c| c.to_lowercase()).collect();
    let needle_lower: String = needle.chars().flat_map(|c| c.to_lowercase()).collect();
    haystack_lower.contains(&needle_lower)
}

/// `/ss <term>` — local filter over already-downloaded results. Returns the term.
pub(crate) fn local_search_term(input: &str) -> Option<&str> {
    input.strip_prefix("/ss ").map(str::trim)
}

/// `/<n>` — request the n-th book entry. Returns the parsed index.
pub(crate) fn entry_number(input: &str) -> Option<usize> {
    ENTRY_RE
        .captures(input)?
        .name("entry_num")?
        .as_str()
        .parse()
        .ok()
}

/// Map a user input line to the IRC string to send, or `None` if nothing
/// should be sent. Pure: `/ss` and `/<n>` are handled by the caller (the render
/// loop) since they depend on the in-memory book list.
pub(crate) fn process_command(command: &str, channel: &str) -> Option<String> {
    // JOIN
    if command == "/join" || command == "/j" {
        return Some(format!("JOIN {}\r\n", channel));
    }

    // QUIT (with optional message)
    if command.starts_with("/quit") || command.starts_with("/q ") || command == "/q" {
        let quit_msg = command
            .strip_prefix("/quit ")
            .or_else(|| command.strip_prefix("/q "))
            .unwrap_or("");
        return if quit_msg.is_empty() {
            Some("QUIT\r\n".to_string())
        } else {
            Some(format!("QUIT :{}\r\n", quit_msg))
        };
    }

    // SEARCH
    if let Some(caps) = SEARCH_RE.captures(command) {
        let search_term = caps.name("search_term").unwrap().as_str();
        return Some(format!("PRIVMSG {} :@search {}\r\n", channel, search_term));
    }

    // Default: raw IRC command (strip a single leading slash)
    Some(format!(
        "{}\r\n",
        command.strip_prefix('/').unwrap_or(command).trim()
    ))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib commands`
Expected: PASS — the 4 new tests plus the existing `test_search_regex`/`test_entry_regex`. The crate as a whole will NOT build yet (Task 9 still references the old `process_command`/`handle_search_results` in `client.rs`); `--lib commands` only compiles what's needed for these unit tests if the rest compiles. If the full crate fails to build due to `client.rs`, that's expected — proceed; this module's logic is correct. (To check this module in isolation without the broken callers, it is acceptable to defer running until after Task 9. If so, note it and move on.)

- [ ] **Step 5: Commit**

```bash
jj commit -m "Make process_command pure; add entry/local-search helpers"
```

Note: the working copy may not compile as a whole between Tasks 7–11 because callers are updated in Task 9. Commit anyway to keep logical units separate; the green build lands at the end of Task 11.

---

## Task 8: IRC parse helpers (`client.rs`)

**Files:**
- Modify: `src/client.rs`

Add pure helpers `parse_names_reply` (RPL_NAMREPLY / 353) and `parse_membership` (JOIN/PART/QUIT) with tests. These are used by the rewritten `receive_loop` in Task 9.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/client.rs`:

```rust
    #[test]
    fn test_parse_names_reply() {
        let line = ":irc.example.com 353 mynick = #bookz :alice @bob +carol\r\n";
        assert_eq!(
            parse_names_reply(line),
            Some(vec![
                "alice".to_string(),
                "bob".to_string(),
                "carol".to_string()
            ])
        );
        assert_eq!(parse_names_reply(":x PRIVMSG #c :hi"), None);
    }

    #[test]
    fn test_parse_membership_join() {
        let line = ":alice!~a@host JOIN :#bookz\r\n";
        assert!(matches!(parse_membership(line), Some(Membership::Joined(n)) if n == "alice"));
    }

    #[test]
    fn test_parse_membership_part_and_quit() {
        assert!(matches!(
            parse_membership(":bob!~b@host PART #bookz :bye\r\n"),
            Some(Membership::Left(n)) if n == "bob"
        ));
        assert!(matches!(
            parse_membership(":carol!~c@host QUIT :Ping timeout\r\n"),
            Some(Membership::Left(n)) if n == "carol"
        ));
    }

    #[test]
    fn test_parse_membership_ignores_other() {
        assert!(parse_membership(":irc 353 mynick = #c :a b").is_none());
        assert!(parse_membership("PING :server").is_none());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib client::tests::test_parse 2>&1 | head -30`
Expected: FAIL — `cannot find function parse_names_reply` / `cannot find type Membership`.

- [ ] **Step 3: Add the helpers**

Insert into `src/client.rs` just above `pub(crate) async fn receive_loop` (near the other free functions):

```rust
// IRC membership parsing for the user-list pane.
pub(crate) enum Membership {
    Joined(String),
    Left(String),
}

// Parse an RPL_NAMREPLY (353): the trailing " :" segment is the space-separated
// nick list; strip channel-status prefixes.
pub(crate) fn parse_names_reply(line: &str) -> Option<Vec<String>> {
    if !line.contains(" 353 ") {
        return None;
    }
    let names = line.rsplit_once(" :")?.1;
    let users: Vec<String> = names
        .split_whitespace()
        .map(|n| n.trim_start_matches(['@', '+', '%', '&', '~']).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if users.is_empty() {
        None
    } else {
        Some(users)
    }
}

// Parse a JOIN/PART/QUIT line into the nick that joined or left.
pub(crate) fn parse_membership(line: &str) -> Option<Membership> {
    let rest = line.strip_prefix(':')?;
    let mut parts = rest.split_whitespace();
    let source = parts.next()?;
    let command = parts.next()?;
    let nick = source.split('!').next()?.to_string();
    if nick.is_empty() {
        return None;
    }
    match command {
        "JOIN" => Some(Membership::Joined(nick)),
        "PART" | "QUIT" => Some(Membership::Left(nick)),
        _ => None,
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib client::tests::test_parse 2>&1 | head -30`
Expected: PASS (4 tests). (Full-crate build may still fail until Task 9 finishes the rewrite; the helper logic is verified.)

- [ ] **Step 5: Commit**

```bash
jj commit -m "Add IRC names/membership parse helpers"
```

---

## Task 9: Rewire client to emit events (`client.rs`)

**Files:**
- Modify: `src/client.rs`
- Modify: `src/tui/mod.rs` (add `run`/`handle_input`)

This is the first of three coordinated switch tasks. After Task 11 the crate compiles again.

- [ ] **Step 1: Update imports and the struct**

In `src/client.rs`, replace the top `use` block (lines importing `ui`, etc.) so it reads:

```rust
use crate::commands::process_command;
use crate::config::Config;
use crate::dcc::{
    DCC_SEND_RE, SEARCHBOT_RESULTS_PREFIX, ZIP_EXTENSION, dcc_receive, read_lines_to_vec,
    unzip_file,
};
use crate::error::{AircError, Result};
use crate::net::retry_with_backoff;
use crate::tui::UiEvent;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::{debug, info, warn};
```

(`process_command` import stays because nothing in client uses it now — remove it if the compiler warns; it is actually used only by `tui`. Drop `use crate::commands::process_command;` line. Also drop `AsyncBufReadExt` only if unused after removing `cli`; keep it — `read_until` needs it.) Net: remove the `process_command`/`handle_search_results` import and the entire `use crate::ui::...` line; remove `use std::process::exit;`.

Add the `ui_tx` field and drop `search_results` in the struct:

```rust
#[derive(Clone)]
pub(crate) struct IrcClient {
    pub(crate) config: Config,
    username: String,
    nickname: String,
    realname: String,
    reader: Arc<tokio::sync::Mutex<BufReader<BoxedReader>>>,
    writer: Arc<tokio::sync::Mutex<BoxedWriter>>,
    sender: Sender<String>,
    pub(crate) ui_tx: Sender<UiEvent>,
    pub(crate) dcc_tasks: Arc<tokio::sync::Mutex<tokio::task::JoinSet<()>>>,
}
```

- [ ] **Step 2: Update `new` to take and store `ui_tx`**

Change the signature and construction in `IrcClient::new`:

```rust
    pub(crate) async fn new(
        config: Config,
        username: &str,
        nickname: &str,
        realname: &str,
        ui_tx: Sender<UiEvent>,
    ) -> Result<(Arc<IrcClient>, Receiver<String>)> {
```

In the returned struct literal, replace the `search_results: ...` line with `ui_tx,` and keep `dcc_tasks`:

```rust
            Arc::new(IrcClient {
                config,
                username: username.to_string(),
                nickname: nickname.to_string(),
                realname: realname.to_string(),
                reader: Arc::new(tokio::sync::Mutex::new(reader)),
                writer: Arc::new(tokio::sync::Mutex::new(writer)),
                sender,
                ui_tx,
                dcc_tasks: Arc::new(tokio::sync::Mutex::new(tokio::task::JoinSet::new())),
            }),
```

Add a `send` method right after the `new` function's closing brace, inside `impl IrcClient`:

```rust
    // Queue an outgoing IRC line for the write task.
    pub(crate) async fn send(&self, message: String) -> Result<()> {
        self.sender.send(message).await?;
        Ok(())
    }
```

- [ ] **Step 3: Rewrite `write` (no stdout, no exit)**

Replace the whole `write` function body with:

```rust
pub(crate) async fn write(client: Arc<IrcClient>, mut receiver: Receiver<String>) -> Result<()> {
    while let Some(message) = receiver.recv().await {
        {
            let mut writer = client.writer.lock().await;
            writer.write_all(message.as_bytes()).await?;
            writer.flush().await?;
        }

        // On QUIT, stop the write loop so shutdown can proceed cleanly.
        if message.len() >= 4 && message[..4].eq_ignore_ascii_case("QUIT") {
            tokio::time::sleep(tokio::time::Duration::from_millis(QUIT_DELAY_MS)).await;
            break;
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Delete `cli`**

Remove the entire `pub(crate) async fn cli(...) { ... }` function.

- [ ] **Step 5: Rewrite DCC handlers to emit events**

Replace `handle_dcc_file` with:

```rust
async fn handle_dcc_file(client: Arc<IrcClient>, fpath: String) -> Result<()> {
    let path = PathBuf::from(&fpath);
    if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
        if filename.starts_with(SEARCHBOT_RESULTS_PREFIX) && filename.ends_with(ZIP_EXTENSION) {
            let txt_file = unzip_file(&fpath, client.ui_tx.clone()).await?;
            let book_entries = read_lines_to_vec(&txt_file).await?;
            let _ = client.ui_tx.send(UiEvent::BookList(book_entries)).await;
        } else {
            let _ = client
                .ui_tx
                .send(UiEvent::System(format!("Downloaded file: {}", filename)))
                .await;
        }
    }
    Ok(())
}
```

Replace the spawn block inside `handle_dcc_send_request` (the `if let (Some(filename), ...)` arm) so it threads `ui_tx` and reports failures. Replace from `info!("✓ DCC SEND...` through the end of that arm with:

```rust
            info!("✓ DCC SEND: {}, {} bytes", filename, size);

            let client_clone = client.clone();
            let ui_tx = client.ui_tx.clone();
            let download_path = client.config.download_path.clone();
            let connection_timeout = client.config.connection_timeout_secs;
            let transfer_timeout = client.config.dcc_timeout_secs;

            let mut tasks = client.dcc_tasks.lock().await;
            tasks.spawn(async move {
                match dcc_receive(
                    &filename,
                    &ip,
                    &port,
                    &size,
                    &download_path,
                    connection_timeout,
                    transfer_timeout,
                    ui_tx.clone(),
                )
                .await
                {
                    Ok(fpath) => {
                        if let Err(e) = handle_dcc_file(client_clone, fpath).await {
                            warn!("Error handling DCC file: {}", e);
                        }
                    }
                    Err(e) => {
                        warn!("Error receiving DCC file: {}", e);
                        let _ = ui_tx
                            .send(UiEvent::DownloadFailed {
                                filename: filename.clone(),
                                error: e.to_string(),
                            })
                            .await;
                    }
                }
            });
```

- [ ] **Step 6: Rewrite `receive_loop` to emit events**

In `receive_loop`, replace the `print_received_line(&line);` call and the disconnect/break handling. Specifically:

Replace the `if bytes_read == 0 { warn!(...); break; }` block with:

```rust
        if bytes_read == 0 {
            warn!("Connection closed by server (0 bytes read)");
            let _ = client.ui_tx.send(UiEvent::Disconnected).await;
            break;
        }
```

Replace `message_count += 1;` and `print_received_line(&line);` with:

```rust
        message_count += 1;
        let _ = client.ui_tx.send(UiEvent::Received(line.clone())).await;

        if let Some(users) = parse_names_reply(&line) {
            let _ = client.ui_tx.send(UiEvent::UserList(users)).await;
        } else if let Some(membership) = parse_membership(&line) {
            let event = match membership {
                Membership::Joined(nick) => UiEvent::UserJoined(nick),
                Membership::Left(nick) => UiEvent::UserLeft(nick),
            };
            let _ = client.ui_tx.send(event).await;
        }
```

Leave PING/PONG and the DCC dispatch (`handle_dcc_send_request`) unchanged. Remove the two `info!("📢 ...")`/`info!("📥 ...")` NOTICE/PRIVMSG log lines or keep them — harmless either way; keeping them is fine.

- [ ] **Step 7: Add `run`/`handle_input` to `tui/mod.rs`**

Append to `src/tui/mod.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::Receiver;
use tokio::task::JoinHandle;

use crate::client::{drain_dcc_tasks, IrcClient};
use crate::commands::{contains_ignore_case, entry_number, local_search_term, process_command};
use crate::error::Result;
use render::{cleanup_terminal, install_panic_hook, render_ui, setup_terminal};

const RENDER_TICK_MS: u64 = 16;
const WRITE_DRAIN_TIMEOUT_SECS: u64 = 2;

/// Run the TUI render loop until the user quits or the server disconnects, then
/// restore the terminal and drain in-flight DCC transfers.
pub(crate) async fn run(
    client: Arc<IrcClient>,
    mut ui_rx: Receiver<UiEvent>,
    write_handle: JoinHandle<Result<()>>,
) -> Result<()> {
    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let mut app = App::new(client.config.clone());
    let mut areas = app::Areas::default();

    while !app.should_quit {
        if let Some(input) = app.handle_events(&areas)? {
            handle_input(&client, &mut app, input).await?;
        }
        while let Ok(event) = ui_rx.try_recv() {
            apply_event(&mut app, event);
        }
        terminal.draw(|f| areas = render_ui(f, &mut app))?;
        tokio::time::sleep(Duration::from_millis(RENDER_TICK_MS)).await;
    }

    cleanup_terminal(&mut terminal)?;

    // Ensure the server sees a QUIT so the write task ends and the connection
    // closes cleanly (Ctrl+Q does not send one itself).
    if !app.quit_sent {
        let _ = client.send("QUIT\r\n".to_string()).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(WRITE_DRAIN_TIMEOUT_SECS), write_handle).await;

    // Absorb any late UI events so DCC tasks never block on a full channel.
    tokio::spawn(async move { while ui_rx.recv().await.is_some() {} });
    drain_dcc_tasks(&client.dcc_tasks).await;
    Ok(())
}

async fn handle_input(client: &Arc<IrcClient>, app: &mut App, input: String) -> Result<()> {
    app.add_message_with_type(input.clone(), MessageType::Sent);

    if let Some(term) = local_search_term(&input) {
        let matches: Vec<String> = app
            .book_list
            .iter()
            .enumerate()
            .filter(|(_, line)| contains_ignore_case(line, term))
            .map(|(i, line)| format!("{}: {}", i, line))
            .collect();
        if matches.is_empty() {
            app.add_message_with_type(format!("No results found for '{}'", term), MessageType::System);
        } else {
            for m in matches {
                app.add_message_with_type(m, MessageType::System);
            }
        }
    } else if let Some(n) = entry_number(&input) {
        if let Some(entry) = app.book_list.get(n).cloned() {
            client
                .send(format!("PRIVMSG {} :{}\r\n", app.current_channel, entry))
                .await?;
        } else {
            app.add_message_with_type(format!("Entry number {} not found", n), MessageType::System);
        }
    } else if let Some(msg) = process_command(&input, &app.current_channel) {
        let is_quit = msg.trim_start().to_uppercase().starts_with("QUIT");
        client.send(msg).await?;
        if is_quit {
            app.should_quit = true;
            app.quit_sent = true;
        }
    }
    Ok(())
}
```

(`drain_dcc_tasks` and `IrcClient` are already `pub(crate)` in `client.rs`.)

- [ ] **Step 8: Defer build**

Do not build yet — `dcc_receive`/`unzip_file` signatures and `main.rs` still need Tasks 10–11. Commit this coherent slice:

```bash
jj commit -m "Rewire IrcClient to emit UiEvents; add TUI run loop"
```

---

## Task 10: Thread events through DCC (`dcc.rs`)

**Files:**
- Modify: `src/dcc.rs`

- [ ] **Step 1: Update imports**

In `src/dcc.rs`, remove `use crate::ui::print_line;` and add:

```rust
use crate::tui::UiEvent;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
```

(Keep existing imports. `Duration` may already be reachable via `tokio::time`; add the `std::time::Duration` import for the throttle timer.)

- [ ] **Step 2: Emit extract events in `unzip_file`**

Change the signature and add event emission. Replace the `unzip_file` signature line and the trailing `print_line(...)`/return:

New signature:

```rust
pub(crate) async fn unzip_file(filename: &str, ui_tx: Sender<UiEvent>) -> Result<String> {
    let base = std::path::Path::new(filename)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(filename)
        .to_string();
    let _ = ui_tx
        .send(UiEvent::DownloadExtracting(base.clone()))
        .await;
```

Then keep the existing `spawn_blocking` body unchanged. Replace the final `print_line(...)` block + `Ok(outpath_str)` with:

```rust
    let _ = ui_tx.send(UiEvent::DownloadExtracted(base)).await;
    Ok(outpath_str)
```

(Delete the `print_line(&format!("File {} extracted ...` call entirely.)

- [ ] **Step 3: Emit start/progress/completed in `dcc_receive`**

Change the `dcc_receive` signature to add `ui_tx`:

```rust
pub(crate) async fn dcc_receive(
    filename: &str,
    ip: &str,
    port: &str,
    size: &str,
    download_path: &str,
    connection_timeout: u64,
    transfer_timeout: u64,
    ui_tx: Sender<UiEvent>,
) -> Result<String> {
```

After `let file_size: u64 = size.trim().parse()...?;` and the size-limit check, emit the start event (replacing the later `print_line("Connecting...")`). Add right after the size validation block:

```rust
    let _ = ui_tx
        .send(UiEvent::DownloadStarted {
            filename: filename.to_string(),
            size: file_size,
        })
        .await;
```

Delete the `print_line(&format!("Connecting to {}:{}...\n", ip_addr, port_num), true);` line.

Inside the streaming `while total_bytes < file_size` loop, after `total_bytes += bytes_read as u64;` and before/after the ACK write, add throttled progress. Insert a throttle state before the loop:

```rust
    let mut last_emit = std::time::Instant::now();
    let mut last_percent = 0u16;
```

and inside the loop, after `total_bytes += bytes_read as u64;`:

```rust
        let percent = if file_size > 0 {
            ((total_bytes as f64 / file_size as f64) * 100.0) as u16
        } else {
            0
        };
        if percent != last_percent || last_emit.elapsed() >= Duration::from_millis(200) {
            let _ = ui_tx
                .send(UiEvent::DownloadProgress {
                    filename: filename.to_string(),
                    received: total_bytes,
                })
                .await;
            last_percent = percent;
            last_emit = std::time::Instant::now();
        }
```

After the loop, replace the final `print_line(&format!("Received file: ...` call with a completed event:

```rust
    let _ = ui_tx
        .send(UiEvent::DownloadCompleted(filename.to_string()))
        .await;
    Ok(file_path.to_string_lossy().to_string())
```

- [ ] **Step 4: Defer build**

Commit this slice (full build still pending main.rs):

```bash
jj commit -m "Emit download progress events from DCC transfers"
```

---

## Task 11: Render-loop main + delete `ui.rs`

**Files:**
- Modify: `src/main.rs`
- Delete: `src/ui.rs`

- [ ] **Step 1: Rewrite `main.rs`**

Replace the entire contents of `src/main.rs` with:

```rust
mod client;
mod commands;
mod config;
mod dcc;
mod error;
mod net;
mod tui;

use clap::Parser;
use client::IrcClient;
use config::Config;
use error::Result;
use rand::Rng;
use tokio::sync::mpsc;
use tracing::info;

use crate::tui::{UiEvent, UiMakeWriter};

// Constants
const DEFAULT_REALNAME: &str = "Book Worm";
const NICKNAME_PREFIX: &str = "bworm";
const MAX_NICKNAME_SUFFIX: u32 = 99999;
const UI_EVENT_BUFFER: usize = 256;

#[derive(Parser, Debug)]
struct Args {
    /// The IRC server to connect to
    #[clap(short, long)]
    server: Option<String>,
    /// The IRC channel to join
    #[clap(short, long)]
    channel: Option<String>,
    /// The username to use
    #[clap(short, long)]
    username: Option<String>,
    /// Download path for DCC files
    #[clap(short, long)]
    download_path: Option<String>,
    /// Connect using TLS/SSL
    #[clap(long)]
    tls: bool,
    /// Port to connect on (defaults to 6667, or 6697 with --tls)
    #[clap(short, long)]
    port: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let config = Config::load()
        .await
        .unwrap_or_else(|_| Config::default())
        .merge_with_args(
            args.server,
            args.channel,
            args.username,
            args.download_path,
            args.tls,
            args.port,
        );

    // One channel carries every UI-bound event: logs, RX lines, downloads.
    let (ui_tx, ui_rx) = mpsc::channel::<UiEvent>(UI_EVENT_BUFFER);

    // Route tracing logs into the message pane instead of stdout.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .without_time()
        .with_writer(UiMakeWriter::new(ui_tx.clone()))
        .init();

    info!("Starting airc IRC client");

    let (username, nickname, realname) = if let Some(ref user) = config.username {
        (user.clone(), user.clone(), user.clone())
    } else {
        let mut rng = rand::rng();
        let random_nickname = format!(
            "{}{}",
            NICKNAME_PREFIX,
            rng.random_range(0..=MAX_NICKNAME_SUFFIX)
        );
        (
            random_nickname.clone(),
            random_nickname.clone(),
            DEFAULT_REALNAME.to_string(),
        )
    };

    let (client, receiver) =
        IrcClient::new(config, &username, &nickname, &realname, ui_tx.clone()).await?;
    let _ = ui_tx.send(UiEvent::Connected).await;

    let _init = tokio::spawn(client::init(client.clone()));
    let write_handle = tokio::spawn(client::write(client.clone(), receiver));
    let _receive = tokio::spawn(client::receive_loop(client.clone()));

    tui::run(client, ui_rx, write_handle).await
}
```

- [ ] **Step 2: Export `UiMakeWriter` from the tui module**

Ensure `src/tui/mod.rs` re-exports it. Confirm the `pub(crate) use` lines near the top include both `MessageType` and the writer. Update/add:

```rust
pub(crate) use app::MessageType;
pub(crate) use log_writer::UiMakeWriter;
```

- [ ] **Step 3: Delete the old stdout printer and its dependency**

```bash
rm src/ui.rs
```

(Removing the file is enough; jj will record the deletion. The `mod ui;` line is already gone from the new `main.rs`.)

Now that `ui.rs` is gone, remove its only consumer — the `colored` crate — from `Cargo.toml`. Delete the `colored = "3.0"` line from `[dependencies]`. (`chrono` stays — `tui/app.rs` uses it.)

- [ ] **Step 4: Build the whole crate**

Run: `cargo build 2>&1 | tail -40`
Expected: compiles cleanly. Fix any straggling references:
- If `process_command` import in `client.rs` triggers an "unused import" error-as-warning, delete that line.
- If `AsyncBufReadExt` is reported unused in `client.rs`, it is still needed by `read_until`; keep it. If genuinely unused, remove it.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -30`
Expected: all tests pass — the original suite (error/config/dcc/commands-regex/client-pong/drain) plus the new `tui::*`, `commands` helper, and `client` parse tests. Confirm count increased and there are no failures.

- [ ] **Step 6: Lint**

Run: `cargo clippy --all-targets 2>&1 | tail -30`
Expected: no warnings. Address any (e.g., `needless_return`, unused imports).

- [ ] **Step 7: Manual smoke test**

Run: `cargo run -- --help`
Expected: clap help prints (no terminal takeover for `--help`).

Then, against a real server (manual, interactive — requires network):
Run: `cargo run -- --server irc.undernet.org --channel '#bookz'`
Verify: alternate-screen TUI appears; four panes render; typing shows in the input box; `Ctrl+Q` exits and the shell prompt is restored intact (no leftover raw mode). If a panic occurs, confirm the terminal is still restored (panic hook).

- [ ] **Step 8: Commit**

```bash
jj commit -m "Switch main to ratatui render loop; remove stdout UI"
```

---

## Task 12: Documentation refresh

**Files:**
- Modify: `README.md`
- Modify: `Requirements.txt`

- [ ] **Step 1: Update README architecture + features**

In `README.md`:
- Under **Features**, add a line: `- **TUI**: Full-screen ratatui interface — message/book/user panes, scrollbars, mouse, live download gauges`.
- In the **Architecture** list, replace the `src/ui.rs` line with:
  ```
  - **src/tui/** - ratatui TUI: App state, rendering, event application, log routing
  ```
- Update the **Dependencies** list: remove `colored`, add `ratatui + crossterm (terminal UI)`.
- Update the **Testing** line count to the actual number reported by `cargo test` (replace "20 tests"/"23 tests" with the new total).
- In **Commands**, confirm `/ss` and `/<number>` rows still describe local filtering and entry requests (unchanged behavior).

- [ ] **Step 2: Update Requirements.txt**

Add a bullet reflecting the TUI (match existing file style): e.g. `- Terminal user interface (ratatui) with panes, scrolling, mouse, and download progress`.

- [ ] **Step 3: Verify build still clean**

Run: `cargo build`
Expected: clean (docs-only changes).

- [ ] **Step 4: Commit**

```bash
jj commit -m "Document ratatui TUI in README and requirements"
```

---

## Self-Review

**Spec coverage:**
- Event flow / single mpsc channel → Task 11 (`ui_tx`/`ui_rx`), Task 9 (`run` loop sole consumer). ✓
- `UiEvent` enum → Task 4/6 (defined in `mod.rs`). ✓
- `tui` module (App, Message/MessageType, DownloadProgress/Status, terminal lifecycle, handle_events, render_ui, apply_event) → Tasks 2,3,5,6. ✓
- client.rs reroute (Received, 353→UserList, JOIN/PART/QUIT, BookList, remove cli, remove exit(0)) → Tasks 8,9. ✓
- dcc.rs (Started/Progress throttled/Completed/Failed, Extracting/Extracted) → Task 10 + Task 9 (Failed in spawn). ✓
- commands.rs pure mapper + `/<n>` and `/ss` in render loop → Tasks 7,9. ✓
- ui.rs absorbed/removed → Task 11. ✓
- Logging via MakeWriter → Tasks 4,11. ✓
- Main loop + panic hook + drain-before-exit → Tasks 5,9,11. ✓
- Deps ratatui+crossterm → Task 1. ✓
- Keybindings/mouse parity → Task 3. ✓
- Testing strategy (scroll math, apply_event, percent, /<n>) → Tasks 2,3,6,7. ✓

**Type consistency:** `ui_tx: Sender<UiEvent>` is the same name in `IrcClient`, `dcc_receive`, `unzip_file`, `UiMakeWriter`. `DownloadProgress` uses `u64` sizes consistently (matches `dcc.rs` `file_size: u64`). `process_command(&str, &str) -> Option<String>` signature matches its sole caller in `tui::handle_input`. `apply_event`/`App` method names (`add_download`, `update_download`, `complete_download`, `fail_download`, `mark_download_extracting`, `set_users`, `add_user`, `remove_user`) are defined in Task 3 and used in Task 6. `Membership`/`parse_membership`/`parse_names_reply` defined in Task 8, used in Task 9.

**Behavior preserved deliberately:** plain (non-slash) input is sent as a raw IRC line (current semantics), not wrapped as PRIVMSG — matches "keep current logic" in the spec. `/quit msg` keeps its custom message; Ctrl+Q sends a bare QUIT at shutdown.

**Placeholder scan:** none — every code step contains complete code.
