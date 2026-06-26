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
