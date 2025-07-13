//! AIRC - Advanced IRC Client
//! 
//! A modern, feature-rich IRC client with TUI interface built in Rust.
//! Features include:
//! - Terminal User Interface with mouse support
//! - DCC file transfers with progress tracking
//! - Configurable settings
//! - Logging support
//! - Book search functionality for IRC book channels

mod error;
mod config;
mod irc;
mod download;
mod ui;
mod logger;

use std::time::Duration;
use tokio::sync::mpsc::{self};
use clap::Parser;
use log::{info, error, debug};

use crate::error::{AircError, Result};
use crate::config::Config;
use crate::irc::{IrcClient, IrcMessage, write_task, read_task, process_user_input};
use crate::download::{Downloader, DownloadMessage};
use crate::ui::{App, setup_terminal, cleanup_terminal, render_ui, MessageType};
use crate::logger::AppLogger;

/// Command line arguments for the AIRC application
#[derive(Parser, Debug)]
struct Args {
    /// The IRC server to connect to
    #[clap(short, long)]
    server: Option<String>,
    /// The IRC channel to join
    #[clap(short, long)]
    channel: Option<String>,
    /// The username to use
    #[clap(short, long)]
    username: Option<String>,
    /// Enable verbose logging
    #[clap(short, long)]
    verbose: bool,
}

enum AppMessage {
    Irc(IrcMessage),
    Download(DownloadMessage),
    UserInput(String),
    Quit,
}

fn setup_logging(verbose: bool) -> tokio::sync::mpsc::UnboundedReceiver<crate::logger::LogMessage> {
    let log_level = if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    
    // Initialize our custom logger that routes to the app
    let log_receiver = AppLogger::init();
    
    // Set the log level
    log::set_max_level(log_level);
    
    log_receiver
}

async fn run_application() -> Result<()> {
    // Parse command line arguments
    let args = Args::parse();
    
    // Setup logging
    let mut log_receiver = setup_logging(args.verbose);
    
    info!("Starting AIRC - Advanced IRC Client");
    
    // Load configuration
    let config = Config::load_or_default()?
        .with_args(args.server, args.channel, args.username);
    
    debug!("Configuration loaded: {:?}", config);
    
    // Create communication channels
    let (_app_sender, _app_receiver) = mpsc::channel::<AppMessage>(100);
    let (irc_message_sender, mut irc_message_receiver) = mpsc::channel::<IrcMessage>(100);
    let (download_sender, mut download_receiver) = mpsc::channel::<DownloadMessage>(100);
    
    // Setup terminal
    let mut terminal = setup_terminal()?;
    
    // Create application state
    let mut app = App::new(config.clone());
    
    // Connect to IRC server
    let (irc_client, irc_receiver) = IrcClient::new(&config).await?;
    
    // Initialize IRC connection
    irc_client.initialize().await?;
    app.connected = true;
    app.add_message_with_type(format!("Connected to {} as {}", config.server, config.nickname), MessageType::System);
    
    // Start IRC tasks
    let write_client = irc_client.clone();
    let read_client = irc_client.clone();
    
    tokio::spawn(async move {
        if let Err(e) = write_task(write_client, irc_receiver).await {
            error!("IRC write task error: {}", e);
        }
    });
    
    tokio::spawn(async move {
        if let Err(e) = read_task(read_client, irc_message_sender).await {
            error!("IRC read task error: {}", e);
        }
    });
    
    // Create downloader
    let downloader = Downloader::new(config.clone(), download_sender.clone());
    
    // Join the configured channel
    irc_client.join_channel(&config.channel).await?;
    
    // Test log messages to verify they appear in the UI
    info!("Successfully joined channel: {}", config.channel);
    debug!("Application setup complete, entering main loop");
    
    // Test error message
    if config.username.starts_with("test") {
        error!("Test error message to verify error log routing");
    }
    
    // Main application loop
    let mut areas = ui::Areas {
        message_area: ratatui::layout::Rect::default(),
        book_area: ratatui::layout::Rect::default(),
        user_area: ratatui::layout::Rect::default(),
        input_area: ratatui::layout::Rect::default(),
    };
    
    while !app.should_quit {
        // Handle UI events
        if let Some(user_input) = app.handle_events(&areas)? {
            app.add_message_with_type(user_input.clone(), MessageType::Sent);
            
            // Process user input
            let command = process_user_input(&user_input, &app.current_channel);
            
            // Handle special commands
            if user_input.starts_with("/search ") || user_input.starts_with("/s ") {
                let search_term = if user_input.starts_with("/s ") {
                    &user_input[3..]
                } else {
                    &user_input[8..]
                };
                irc_client.search_books(&app.current_channel, search_term).await?;
            } else if user_input.starts_with("/join ") {
                let channel = &user_input[6..].trim();
                irc_client.join_channel(channel).await?;
                app.current_channel = channel.to_string();
            } else if user_input == "/quit" || user_input == "/q" {
                irc_client.quit().await?;
                app.should_quit = true;
            } else if !user_input.starts_with('/') {
                irc_client.send_message(&app.current_channel, &user_input).await?;
            } else {
                // Send raw IRC command
                irc_client.send_raw(&command).await?;
            }
        }
        
        // Handle IRC messages
        while let Ok(irc_msg) = irc_message_receiver.try_recv() {
            match irc_msg {
                IrcMessage::Message(msg) => {
                    // Parse the message to determine if it's received or system
                    if msg.starts_with("RX: ") {
                        app.add_message_with_type(msg[4..].to_string(), MessageType::Received);
                    } else {
                        app.add_message_with_type(msg, MessageType::System);
                    }
                },
                IrcMessage::Book(book) => app.add_book(book),
                IrcMessage::User(user) => app.add_user(user),
                IrcMessage::UserList(users) => app.set_users(users),
                IrcMessage::DccSend { filename, ip, port, size } => {
                    app.add_download(filename.clone(), size);
                    
                    // Start download in background
                    let downloader_clone = downloader.clone();
                    let filename_clone = filename.clone();
                    let ip_clone = ip.clone();
                    let port_clone = port.clone();
                    
                    tokio::spawn(async move {
                        if let Err(e) = downloader_clone.download_dcc(&filename_clone, &ip_clone, &port_clone, size).await {
                            error!("Download failed: {}", e);
                        }
                    });
                }
                IrcMessage::Ping(pong) => {
                    irc_client.send_raw(&pong).await?;
                }
                IrcMessage::Connected => {
                    app.connected = true;
                    app.add_message_with_type("Connected to IRC server".to_string(), MessageType::System);
                }
                IrcMessage::Disconnected => {
                    app.connected = false;
                    app.add_message_with_type("Disconnected from IRC server".to_string(), MessageType::System);
                }
            }
        }
        
        // Handle download messages
        while let Ok(download_msg) = download_receiver.try_recv() {
            match download_msg {
                DownloadMessage::Started(filename, size) => {
                    app.add_download(filename.clone(), size);
                    app.add_message_with_type(format!("Download started: {}", filename), MessageType::System);
                }
                DownloadMessage::Progress(filename, current) => {
                    app.update_download(&filename, current);
                }
                DownloadMessage::Completed(filename) => {
                    app.complete_download(&filename);
                    app.add_message_with_type(format!("Download completed: {}", filename), MessageType::System);
                }
                DownloadMessage::Failed(filename, error) => {
                    app.fail_download(&filename, error.clone());
                    app.add_message_with_type(format!("Download failed: {} - {}", filename, error), MessageType::System);
                }
                DownloadMessage::Extracting(filename) => {
                    app.mark_download_extracting(&filename);
                    app.add_message_with_type(format!("Extracting: {}", filename), MessageType::System);
                }
                DownloadMessage::Extracted(filename) => {
                    app.add_message_with_type(format!("Extracted: {}", filename), MessageType::System);
                }
            }
        }
        
        // Handle log messages
        while let Ok(log_msg) = log_receiver.try_recv() {
            app.add_message_with_type(log_msg.content, log_msg.message_type);
        }
        
        // Draw UI
        terminal.draw(|f| {
            areas = render_ui(f, &mut app);
        }).map_err(|e| AircError::UiError(format!("Draw error: {}", e)))?;
        
        // Prevent CPU spinning
        tokio::time::sleep(Duration::from_millis(16)).await; // ~60 FPS
    }
    
    // Cleanup
    cleanup_terminal(&mut terminal)?;
    info!("Application shutdown complete");
    Ok(())
}



#[tokio::main]
async fn main() -> Result<()> {
    if let Err(e) = run_application().await {
        error!("Application error: {}", e);
        return Err(e);
    }
    Ok(())
}
