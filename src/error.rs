
#[derive(Debug, thiserror::Error)]
pub enum AircError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    
    #[error("IRC protocol error: {0}")]
    IrcProtocolError(String),
    
    #[error("File operation failed: {0}")]
    FileOperationFailed(String),
    
    #[error("Download error: {0}")]
    DownloadError(String),
    
    #[error("Configuration error: {0}")]
    ConfigError(String),
    
    #[error("UI error: {0}")]
    UiError(String),
    
    #[error("Channel communication error: {0}")]
    ChannelError(String),
    
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    
    #[error("Regex error: {0}")]
    RegexError(#[from] regex::Error),
    
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
    
    #[error("Zip error: {0}")]
    ZipError(#[from] zip::result::ZipError),
}

pub type Result<T> = std::result::Result<T, AircError>;