use std::io;
use std::time::Duration;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, MouseEvent, MouseEventKind, MouseButton},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame, Terminal,
};
use log::{debug, info, error};
use chrono::{DateTime, Local};

use crate::error::{AircError, Result};
use crate::config::Config;
use crate::download::{DownloadProgress, DownloadStatus};

const MAX_SCROLL_AMOUNT: usize = 5;

#[derive(Clone, Debug)]
pub struct Message {
    pub content: String,
    pub timestamp: DateTime<Local>,
    pub message_type: MessageType,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MessageType {
    Sent,      // TX messages
    Received,  // RX messages
    System,    // System messages
    Info,      // Info log messages
    Debug,     // Debug log messages
    Error,     // Error log messages
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
        
        format!("[{}] {}: {}", 
            self.timestamp.format("%H:%M:%S"),
            prefix,
            self.content
        )
    }
}

pub struct App {
    pub should_quit: bool,
    pub input: String,
    pub messages: Vec<Message>,
    pub book_list: Vec<String>,
    pub user_list: Vec<String>,
    pub cursor_position: usize,
    pub downloads: Vec<DownloadProgress>,
    pub current_channel: String,
    pub connected: bool,
    
    // Scrolling state
    pub message_scroll: usize,
    pub max_scroll: usize,
    pub book_scroll: usize,
    pub book_scroll_state: ScrollbarState,
    pub user_scroll: usize,
    pub user_scroll_state: ScrollbarState,
    pub message_scroll_state: ScrollbarState,
    
    // UI state
    pub last_click_position: Option<(u16, u16)>,
    pub active_panel: ActivePanel,
    pub config: Config,
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum ActivePanel {
    Messages,
    Books,
    Users,
    Input,
}

pub struct Areas {
    pub message_area: Rect,
    pub book_area: Rect,
    pub user_area: Rect,
    pub input_area: Rect,
}

impl App {
    pub fn new(config: Config) -> Self {
        Self {
            should_quit: false,
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
            book_scroll: 0,
            book_scroll_state: ScrollbarState::default(),
            user_scroll: 0,
            user_scroll_state: ScrollbarState::default(),
            message_scroll_state: ScrollbarState::default(),
            
            last_click_position: None,
            active_panel: ActivePanel::Input,
            config,
        }
    }
    
    pub fn add_message(&mut self, message: String) {
        self.add_message_with_type(message, MessageType::System);
    }
    
    pub fn add_message_with_type(&mut self, content: String, message_type: MessageType) {
        self.messages.push(Message::new(content, message_type));
        if self.messages.len() > self.config.max_message_history {
            self.messages.remove(0);
        }
        self.scroll_to_bottom();
    }
    
    pub fn add_book(&mut self, book: String) {
        if !self.book_list.contains(&book) {
            self.book_list.push(book);
        }
    }
    
    pub fn add_user(&mut self, user: String) {
        if !self.user_list.contains(&user) {
            self.user_list.push(user);
        }
    }
    
    pub fn set_users(&mut self, users: Vec<String>) {
        self.user_list = users;
    }
    
    pub fn add_download(&mut self, filename: String, size: u32) {
        self.downloads.push(DownloadProgress::new(filename, size));
    }
    
    pub fn update_download(&mut self, filename: &str, current_size: u32) {
        if let Some(download) = self.downloads.iter_mut().find(|d| d.filename == filename) {
            download.update_progress(current_size);
        }
    }
    
    pub fn complete_download(&mut self, filename: &str) {
        if let Some(download) = self.downloads.iter_mut().find(|d| d.filename == filename) {
            download.mark_completed();
        }
    }
    
    pub fn fail_download(&mut self, filename: &str, error: String) {
        if let Some(download) = self.downloads.iter_mut().find(|d| d.filename == filename) {
            download.mark_failed(error);
        }
    }
    
    pub fn mark_download_extracting(&mut self, filename: &str) {
        if let Some(download) = self.downloads.iter_mut().find(|d| d.filename == filename) {
            download.mark_extracting();
        }
    }
    
    // Scrolling functionality
    pub fn scroll_to_bottom(&mut self) {
        self.message_scroll = 0; // 0 means showing the bottom (latest messages)
        self.update_message_scrollbar();
    }
    
    pub fn update_max_scroll(&mut self, viewport_height: usize) {
        if self.messages.len() > viewport_height {
            self.max_scroll = self.messages.len() - viewport_height;
        } else {
            self.max_scroll = 0;
        }
        
        // Ensure scroll position doesn't exceed maximum
        if self.message_scroll > self.max_scroll {
            self.message_scroll = self.max_scroll;
        }
    }
    
    pub fn scroll_up(&mut self, amount: usize) {
        // Scrolling up shows older messages (increases scroll value)
        if self.message_scroll + amount <= self.max_scroll {
            self.message_scroll += amount;
        } else {
            self.message_scroll = self.max_scroll;
        }
        self.update_message_scrollbar();
    }
    
    pub fn scroll_down(&mut self, amount: usize) {
        // Scrolling down shows newer messages (decreases scroll value)
        if self.message_scroll >= amount {
            self.message_scroll -= amount;
        } else {
            self.message_scroll = 0;
        }
        self.update_message_scrollbar();
    }
    
    pub fn update_message_scrollbar(&mut self) {
        self.message_scroll_state = self.message_scroll_state
            .content_length(self.messages.len())
            .position(self.message_scroll);
    }
    
    // Book list scrolling
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
    
    // User list scrolling
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
    
    pub fn handle_mouse_event(&mut self, event: MouseEvent, areas: &Areas) -> Result<()> {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.last_click_position = Some((event.column, event.row));
                
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
            MouseEventKind::ScrollDown => {
                match self.active_panel {
                    ActivePanel::Messages => self.scroll_down(1),
                    ActivePanel::Books => self.scroll_books_down(1),
                    ActivePanel::Users => self.scroll_users_down(1),
                    _ => {}
                }
            }
            MouseEventKind::ScrollUp => {
                match self.active_panel {
                    ActivePanel::Messages => self.scroll_up(1),
                    ActivePanel::Books => self.scroll_books_up(1),
                    ActivePanel::Users => self.scroll_users_up(1),
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }
    
    pub fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) -> Result<Option<String>> {
        match key.code {
            KeyCode::Char('q') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                self.should_quit = true;
                Ok(None)
            }
            KeyCode::PageUp => {
                self.scroll_up(MAX_SCROLL_AMOUNT);
                Ok(None)
            }
            KeyCode::PageDown => {
                self.scroll_down(MAX_SCROLL_AMOUNT);
                Ok(None)
            }
            KeyCode::Up if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                self.scroll_up(1);
                Ok(None)
            }
            KeyCode::Down if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                self.scroll_down(1);
                Ok(None)
            }
            KeyCode::Home => {
                // Home scrolls to the oldest messages (top)
                self.message_scroll = self.max_scroll;
                self.update_message_scrollbar();
                Ok(None)
            }
            KeyCode::End => {
                // End scrolls to the newest messages (bottom)
                self.scroll_to_bottom();
                Ok(None)
            }
            KeyCode::Char(c) => {
                self.input.insert(self.cursor_position, c);
                self.cursor_position += 1;
                Ok(None)
            }
            KeyCode::Backspace => {
                if self.cursor_position > 0 {
                    self.cursor_position -= 1;
                    self.input.remove(self.cursor_position);
                }
                Ok(None)
            }
            KeyCode::Delete => {
                if self.cursor_position < self.input.len() {
                    self.input.remove(self.cursor_position);
                }
                Ok(None)
            }
            KeyCode::Left => {
                if self.cursor_position > 0 {
                    self.cursor_position -= 1;
                }
                Ok(None)
            }
            KeyCode::Right => {
                if self.cursor_position < self.input.len() {
                    self.cursor_position += 1;
                }
                Ok(None)
            }
            KeyCode::Enter => {
                if !self.input.is_empty() {
                    let input = self.input.clone();
                    self.input.clear();
                    self.cursor_position = 0;
                    self.scroll_to_bottom();
                    Ok(Some(input))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None)
        }
    }
    
    pub fn handle_events(&mut self, areas: &Areas) -> Result<Option<String>> {
        if event::poll(Duration::from_millis(10))
            .map_err(|e| AircError::UiError(format!("Event poll error: {}", e)))? {
            
            match event::read()
                .map_err(|e| AircError::UiError(format!("Event read error: {}", e)))? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    return self.handle_key_event(key);
                }
                Event::Mouse(mouse) => {
                    self.handle_mouse_event(mouse, areas)?;
                }
                _ => {}
            }
        }
        Ok(None)
    }
}

// Helper trait for rectangle point checking
pub trait RectExt {
    fn contains_point(&self, x: u16, y: u16) -> bool;
}

impl RectExt for Rect {
    fn contains_point(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

pub fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()
        .map_err(|e| AircError::UiError(format!("Failed to enable raw mode: {}", e)))?;
    
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture)
        .map_err(|e| AircError::UiError(format!("Failed to setup terminal: {}", e)))?;
    
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)
        .map_err(|e| AircError::UiError(format!("Failed to create terminal: {}", e)))?;
    
    Ok(terminal)
}

pub fn cleanup_terminal(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    disable_raw_mode()
        .map_err(|e| AircError::UiError(format!("Failed to disable raw mode: {}", e)))?;
    
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )
    .map_err(|e| AircError::UiError(format!("Failed to cleanup terminal: {}", e)))?;
    
    terminal.show_cursor()
        .map_err(|e| AircError::UiError(format!("Failed to show cursor: {}", e)))?;
    
    Ok(())
}

pub fn render_ui(f: &mut Frame, app: &mut App) -> Areas {
    let size = f.area();
    
    // Create layout
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(4),
        ])
        .split(size);
    
    let main_area = vertical_chunks[0];
    let footer_area = vertical_chunks[1];
    
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
        .constraints([
            Constraint::Percentage(70),
            Constraint::Percentage(30),
        ])
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
        .constraints([
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(footer_area);
    
    let areas = Areas {
        message_area: bottom_chunks[2],
        book_area: middle_chunks[0],
        user_area: main_chunks[2],
        input_area: footer_chunks[0],
    };
    
    // Render components
    render_server_info(f, app, main_chunks[0]);
    render_book_list(f, app, &areas.book_area);
    render_user_list(f, app, &areas.user_area);
    render_download_progress(f, app, &bottom_chunks[0..2]);
    render_message_log(f, app, &areas.message_area);
    render_input_box(f, app, &areas.input_area);
    render_help_text(f, footer_chunks[1]);
    
    areas
}

fn render_server_info(f: &mut Frame, app: &App, area: Rect) {
    let title = format!("Server: {}", app.current_channel);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue));
    
    let status = if app.connected { "Connected" } else { "Disconnected" };
    let items = vec![
        ListItem::new(status),
        ListItem::new("Type /join to join"),
    ];
    
    let list = List::new(items)
        .block(block)
        .style(Style::default().fg(Color::Cyan));
    
    f.render_widget(list, area);
}

fn render_book_list(f: &mut Frame, app: &mut App, area: &Rect) {
    let block = Block::default()
        .title("Book list")
        .borders(Borders::ALL)
        .border_style(if app.active_panel == ActivePanel::Books {
            Style::default().fg(Color::LightBlue)
        } else {
            Style::default().fg(Color::LightYellow)
        });
    
    let viewport_height = area.height as usize - 2;
    app.update_book_scroll(viewport_height);
    
    let visible_books = app.book_list.iter()
        .skip(app.book_scroll)
        .take(viewport_height)
        .collect::<Vec<_>>();
    
    let items: Vec<ListItem> = visible_books
        .iter()
        .map(|book| ListItem::new(book.as_str()))
        .collect();
    
    let list = List::new(items)
        .block(block)
        .style(Style::default().fg(Color::LightYellow));
    
    f.render_widget(list, *area);
    
    // Render scrollbar
    if !app.book_list.is_empty() {
        let scrollbar = Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        
        f.render_stateful_widget(
            scrollbar,
            Rect {
                x: area.x + area.width - 1,
                y: area.y + 1,
                width: 1,
                height: area.height - 2,
            },
            &mut app.book_scroll_state,
        );
    }
}

fn render_user_list(f: &mut Frame, app: &mut App, area: &Rect) {
    let block = Block::default()
        .title("User list")
        .borders(Borders::ALL)
        .border_style(if app.active_panel == ActivePanel::Users {
            Style::default().fg(Color::LightBlue)
        } else {
            Style::default().fg(Color::Red)
        });
    
    let viewport_height = area.height as usize - 2;
    app.update_user_scroll(viewport_height);
    
    let visible_users = app.user_list.iter()
        .skip(app.user_scroll)
        .take(viewport_height)
        .collect::<Vec<_>>();
    
    let items: Vec<ListItem> = visible_users
        .iter()
        .map(|user| ListItem::new(user.as_str()))
        .collect();
    
    let list = List::new(items)
        .block(block)
        .style(Style::default().fg(Color::Red));
    
    f.render_widget(list, *area);
    
    // Render scrollbar
    if !app.user_list.is_empty() {
        let scrollbar = Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        
        f.render_stateful_widget(
            scrollbar,
            Rect {
                x: area.x + area.width - 1,
                y: area.y + 1,
                width: 1,
                height: area.height - 2,
            },
            &mut app.user_scroll_state,
        );
    }
}

fn render_download_progress(f: &mut Frame, app: &App, areas: &[Rect]) {
    let downloads = app.downloads.iter().take(2).collect::<Vec<_>>();
    
    for (i, area) in areas.iter().enumerate() {
        if let Some(download) = downloads.get(i) {
            let title = format!("Download: {} ({})", download.filename, 
                match &download.status {
                    DownloadStatus::Starting => "Starting",
                    DownloadStatus::InProgress => "In Progress",
                    DownloadStatus::Completed => "Completed",
                    DownloadStatus::Failed(_e) => "Failed",
                    DownloadStatus::Extracting => "Extracting",
                });
            
            let block = Block::default()
                .title(title)
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

fn render_message_log(f: &mut Frame, app: &mut App, area: &Rect) {
    let message_height = area.height as usize - 2;
    
    app.update_max_scroll(message_height);
    app.update_message_scrollbar();
    
    let visible_messages = if app.messages.is_empty() {
        &[]
    } else {
        // Calculate which messages to show
        let total_messages = app.messages.len();
        
        if total_messages <= message_height {
            // Show all messages if they fit
            &app.messages[..]
        } else {
            // Show a window of messages
            // message_scroll = 0 means show latest messages (bottom)
            // message_scroll > 0 means scroll up to show older messages
            let start = if app.message_scroll >= app.max_scroll {
                0 // Show oldest messages
            } else {
                total_messages - message_height - app.message_scroll
            };
            
            let end = if app.message_scroll == 0 {
                total_messages // Show up to latest message
            } else {
                total_messages - app.message_scroll
            };
            
            &app.messages[start..end]
        }
    };
    
    let title = if app.message_scroll > 0 {
        format!("IRC Messages (Scrolled up {}/{})", app.message_scroll, app.max_scroll)
    } else if app.max_scroll > 0 {
        "IRC Messages (Latest)".to_string()
    } else {
        "IRC Messages".to_string()
    };
    
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(if app.active_panel == ActivePanel::Messages {
            Style::default().fg(Color::LightBlue)
        } else {
            Style::default().fg(Color::Magenta)
        });
    
    let messages: Vec<Line> = visible_messages
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
    
    let text = Text::from(messages);
    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(ratatui::widgets::Wrap { trim: true });
    
    f.render_widget(paragraph, *area);
    
    // Render scrollbar
    if !app.messages.is_empty() {
        let scrollbar = Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        
        f.render_stateful_widget(
            scrollbar,
            Rect {
                x: area.x + area.width - 1,
                y: area.y + 1,
                width: 1,
                height: area.height - 2,
            },
            &mut app.message_scroll_state,
        );
    }
}

fn render_input_box(f: &mut Frame, app: &App, area: &Rect) {
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
    
    f.render_widget(input, *area);
    
    // Set cursor position
    f.set_cursor_position(Position {
        x: area.x + 1 + app.cursor_position as u16,
        y: area.y + 1,
    });
}

fn render_help_text(f: &mut Frame, area: Rect) {
    let help_text = vec![
        Span::styled("/join", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": Join channel | "),
        Span::styled("/s query", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": Search books | "),
        Span::styled("↑/↓/PgUp/PgDn", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": Scroll | "),
        Span::styled("Home/End", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": Top/Bottom | "),
        Span::styled("Ctrl+Q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": Quit"),
    ];
    
    let paragraph = Paragraph::new(Line::from(help_text))
        .style(Style::default().fg(Color::White));
    
    f.render_widget(paragraph, area);
}