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
        ListItem::new("/j join"),
        ListItem::new("/q quit"),
    ];
    let list = List::new(items)
        .block(block)
        .style(Style::default().fg(Color::Cyan));
    f.render_widget(list, area);
}

/// The book-list lines as shown in the pane: each prefixed with its absolute
/// index so it matches the `/<n>` request command and stays correct while
/// the pane is scrolled.
pub(crate) fn visible_numbered_books(books: &[String], scroll: usize, viewport: usize) -> Vec<String> {
    books
        .iter()
        .enumerate()
        .skip(scroll)
        .take(viewport)
        .map(|(i, b)| format!("{}: {}", i, b))
        .collect()
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

    let items: Vec<ListItem> =
        visible_numbered_books(&app.book_list, app.book_scroll, viewport_height)
            .into_iter()
            .map(ListItem::new)
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

/// Top-of-viewport line index for the message scrollbar. The message pane's
/// scroll counts up from the newest line (0 = bottom), so map it to a normal
/// top-down track position: newest → bottom, oldest → top.
pub(crate) fn message_scrollbar_position(
    total: usize,
    viewport: usize,
    scroll_from_bottom: usize,
) -> usize {
    total
        .saturating_sub(viewport)
        .saturating_sub(scroll_from_bottom)
}

fn render_message_log(f: &mut Frame, app: &mut App, area: Rect) {
    let viewport = (area.height as usize).saturating_sub(2);
    app.update_max_scroll(viewport);

    let total = app.messages.len();
    app.message_scroll_state = app
        .message_scroll_state
        .content_length(total)
        .viewport_content_length(viewport)
        .position(message_scrollbar_position(total, viewport, app.message_scroll));

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
        Span::styled("/j", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": join | "),
        Span::styled("/s query", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": search | "),
        Span::styled("/N", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": request | "),
        Span::styled("PgUp/PgDn/Home/End", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": scroll | "),
        Span::styled("/q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" or "),
        Span::styled("Ctrl+Q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": quit"),
    ];
    let paragraph = Paragraph::new(Line::from(help)).style(Style::default().fg(Color::White));
    f.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_scrollbar_position_orientation() {
        // 20 lines, 5 visible. Newest (scroll 0) sits at the bottom of the track.
        assert_eq!(message_scrollbar_position(20, 5, 0), 15);
        // Fully scrolled up (scroll == max_scroll 15) sits at the top.
        assert_eq!(message_scrollbar_position(20, 5, 15), 0);
        // Midway.
        assert_eq!(message_scrollbar_position(20, 5, 10), 5);
        // Content shorter than the viewport pins to the top.
        assert_eq!(message_scrollbar_position(3, 5, 0), 0);
    }

    #[test]
    fn test_visible_numbered_books_uses_absolute_index() {
        let books = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(
            visible_numbered_books(&books, 0, 2),
            vec!["0: a".to_string(), "1: b".to_string()]
        );
        // Scrolled: numbers stay absolute so `/<n>` still selects the right entry.
        assert_eq!(
            visible_numbered_books(&books, 1, 2),
            vec!["1: b".to_string(), "2: c".to_string()]
        );
    }
}
