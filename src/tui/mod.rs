pub(crate) mod app;
mod download;
mod log_writer;
mod render;

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
