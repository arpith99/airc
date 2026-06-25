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
