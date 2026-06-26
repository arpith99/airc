pub(crate) mod app;
mod download;
mod log_writer;
mod render;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::Receiver;
use tokio::task::JoinHandle;

use crate::client::{IrcClient, drain_dcc_tasks};
use crate::commands::{contains_ignore_case, entry_number, local_search_term, process_command};
use crate::error::Result;

use app::App;
use render::{cleanup_terminal, install_panic_hook, render_ui, setup_terminal};

pub(crate) use app::MessageType;
pub(crate) use log_writer::UiMakeWriter;

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

    // Absorb late UI events first so in-flight DCC tasks never block on a full
    // channel while we wait for the write task and downloads to finish.
    tokio::spawn(async move { while ui_rx.recv().await.is_some() {} });

    // Ensure the server sees a QUIT so the write task ends (Ctrl+Q sends none).
    if !app.quit_sent {
        let _ = client.send("QUIT\r\n".to_string()).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(WRITE_DRAIN_TIMEOUT_SECS), write_handle).await;

    // The terminal is restored, so report the wait directly if downloads are
    // still finishing — draining blocks until every transfer completes.
    let pending = client.dcc_tasks.lock().await.len();
    if pending > 0 {
        eprintln!("Waiting for {pending} in-flight download(s) to finish...");
    }
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
            app.add_message_with_type(
                format!("No results found for '{}'", term),
                MessageType::System,
            );
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
            app.add_message_with_type(
                format!("Entry number {} not found", n),
                MessageType::System,
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use super::download::DownloadStatus;

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
