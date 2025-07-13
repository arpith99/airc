use std::sync::Mutex;
use log::{Level, Log, Metadata, Record};
use once_cell::sync::Lazy;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::ui::MessageType;

pub struct LogMessage {
    pub content: String,
    pub message_type: MessageType,
}

static LOG_SENDER: Lazy<Mutex<Option<UnboundedSender<LogMessage>>>> = Lazy::new(|| Mutex::new(None));

pub struct AppLogger;

impl AppLogger {
    pub fn init() -> UnboundedReceiver<LogMessage> {
        let (sender, receiver) = mpsc::unbounded_channel();
        
        // Store the sender globally
        {
            let mut global_sender = LOG_SENDER.lock().unwrap();
            *global_sender = Some(sender);
        }
        
        // Set up the logger
        log::set_logger(&AppLogger)
            .map(|()| log::set_max_level(log::LevelFilter::Debug))
            .expect("Failed to set logger");
        
        receiver
    }
}

impl Log for AppLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Debug
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let message_type = match record.level() {
                Level::Error => MessageType::Error,
                Level::Warn => MessageType::Error,  // Treat warnings as errors for visibility
                Level::Info => MessageType::Info,
                Level::Debug => MessageType::Debug,
                Level::Trace => MessageType::Debug,  // Treat trace as debug
            };
            
            let content = format!("{}", record.args());
            
            // Send to the app if sender is available
            if let Ok(sender_guard) = LOG_SENDER.lock() {
                if let Some(sender) = sender_guard.as_ref() {
                    let _ = sender.send(LogMessage {
                        content,
                        message_type,
                    });
                }
            }
        }
    }

    fn flush(&self) {
        // No-op for our implementation
    }
}