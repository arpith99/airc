use std::io;
use std::num::ParseIntError;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, AircError>;

#[derive(Error, Debug)]
pub enum AircError {
    #[error("Connection error: {0}")]
    Connection(String),

    #[error("Connection timed out: {0}")]
    Timeout(String),

    #[error("IRC protocol error: {0}")]
    Protocol(String),

    #[error("DCC transfer error: {0}")]
    DccTransfer(String),

    #[error("File operation error: {0}")]
    FileOperation(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),

    #[error("Zip error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("TOML deserialization error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("Parse error: {0}")]
    Parse(#[from] ParseIntError),

    #[error("Channel send error")]
    ChannelSend,

    #[error("Task join error: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),

    #[error("{0}")]
    Other(String),
}

impl<T> From<tokio::sync::mpsc::error::SendError<T>> for AircError {
    fn from(_: tokio::sync::mpsc::error::SendError<T>) -> Self {
        AircError::ChannelSend
    }
}

impl From<String> for AircError {
    fn from(s: String) -> Self {
        AircError::Other(s)
    }
}

impl From<&str> for AircError {
    fn from(s: &str) -> Self {
        AircError::Other(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn test_error_from_string() {
        let err: AircError = "test error".to_string().into();
        assert_eq!(err.to_string(), "test error");
    }

    #[test]
    fn test_error_from_str() {
        let err: AircError = "test error".into();
        assert_eq!(err.to_string(), "test error");
    }

    #[test]
    fn test_connection_error() {
        let err = AircError::Connection("failed to connect".to_string());
        assert_eq!(err.to_string(), "Connection error: failed to connect");
    }

    #[test]
    fn test_timeout_error() {
        let err = AircError::Timeout("operation timed out".to_string());
        assert_eq!(err.to_string(), "Connection timed out: operation timed out");
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let err: AircError = io_err.into();
        assert!(err.to_string().contains("file not found"));
    }

    #[test]
    fn test_parse_int_error_conversion() {
        let parse_result = "not_a_number".parse::<i32>();
        assert!(parse_result.is_err());
        let err: AircError = parse_result.unwrap_err().into();
        assert!(err.to_string().contains("Parse error"));
    }

    #[test]
    fn test_config_error() {
        let err = AircError::Config("invalid config".to_string());
        assert_eq!(err.to_string(), "Configuration error: invalid config");
    }

    #[test]
    fn test_channel_send_error() {
        let err = AircError::ChannelSend;
        assert_eq!(err.to_string(), "Channel send error");
    }
}
