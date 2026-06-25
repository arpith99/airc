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
