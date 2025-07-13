use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::io::{AsyncWriteExt, AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::{self, Sender, Receiver};
use tokio::sync::Mutex as TokioMutex;
use regex::Regex;
use log::{debug, info, warn, error};

use crate::error::{AircError, Result};
use crate::config::Config;

#[derive(Clone)]
pub struct IrcClient {
    pub server: String,
    pub username: String,
    pub nickname: String,
    pub realname: String,
    reader: Arc<TokioMutex<BufReader<OwnedReadHalf>>>,
    writer: Arc<TokioMutex<OwnedWriteHalf>>,
    pub sender: Sender<String>,
}

#[derive(Debug, Clone)]
pub enum IrcMessage {
    Message(String),
    Book(String),
    User(String),
    DccSend {
        filename: String,
        ip: String,
        port: String,
        size: u32,
    },
    Ping(String),
    UserList(Vec<String>),
    Connected,
    Disconnected,
}

impl IrcClient {
    pub async fn new(config: &Config) -> Result<(Arc<Self>, Receiver<String>)> {
        info!("Connecting to IRC server {}:{}", config.server, config.port);
        
        let stream = tokio::time::timeout(
            Duration::from_secs(config.connection_timeout_secs),
            TcpStream::connect(format!("{}:{}", config.server, config.port))
        )
        .await
        .map_err(|_| AircError::ConnectionFailed("Connection timeout".to_string()))?
        .map_err(|e| AircError::ConnectionFailed(format!("Failed to connect: {}", e)))?;
        
        let (sender, receiver) = mpsc::channel(100);
        let (reader, writer) = stream.into_split();
        let reader = BufReader::new(reader);
        
        let client = Arc::new(Self {
            server: config.server.clone(),
            username: config.username.clone(),
            nickname: config.nickname.clone(),
            realname: config.realname.clone(),
            reader: Arc::new(TokioMutex::new(reader)),
            writer: Arc::new(TokioMutex::new(writer)),
            sender,
        });
        
        info!("Successfully connected to IRC server");
        Ok((client, receiver))
    }
    
    pub async fn initialize(&self) -> Result<()> {
        debug!("Initializing IRC connection");
        
        self.send_raw("CAP LS").await?;
        self.send_raw(&format!("NICK {}", self.nickname)).await?;
        self.send_raw(&format!("USER {} {} {} :{}", 
            self.username, self.nickname, self.server, self.realname)).await?;
        
        info!("IRC initialization complete");
        Ok(())
    }
    
    pub async fn send_raw(&self, message: &str) -> Result<()> {
        let formatted = format!("{}\r\n", message);
        debug!("Sending IRC message: {}", message);
        
        self.sender.send(formatted).await
            .map_err(|e| AircError::ChannelError(format!("Failed to send message: {}", e)))?;
        
        Ok(())
    }
    
    pub async fn join_channel(&self, channel: &str) -> Result<()> {
        info!("Joining channel: {}", channel);
        self.send_raw(&format!("JOIN {}", channel)).await
    }
    
    pub async fn send_message(&self, channel: &str, message: &str) -> Result<()> {
        debug!("Sending message to {}: {}", channel, message);
        self.send_raw(&format!("PRIVMSG {} :{}", channel, message)).await
    }
    
    pub async fn search_books(&self, channel: &str, query: &str) -> Result<()> {
        info!("Searching for books: {}", query);
        self.send_raw(&format!("PRIVMSG {} :@search {}", channel, query)).await
    }
    
    pub async fn quit(&self) -> Result<()> {
        info!("Quitting IRC");
        self.send_raw("QUIT").await
    }
}

pub async fn write_task(client: Arc<IrcClient>, mut receiver: Receiver<String>) -> Result<()> {
    info!("Starting IRC write task");
    
    while let Some(message) = receiver.recv().await {
        let mut writer = client.writer.lock().await;
        writer.write_all(message.as_bytes()).await
            .map_err(|e| AircError::IoError(e))?;
        
        if message.starts_with("QUIT") {
            info!("Received QUIT command, stopping write task");
            break;
        }
    }
    
    Ok(())
}

pub async fn read_task(client: Arc<IrcClient>, message_sender: Sender<IrcMessage>) -> Result<()> {
    info!("Starting IRC read task");
    
    loop {
        let mut line = String::new();
        let bytes_read = {
            let mut reader = client.reader.lock().await;
            reader.read_line(&mut line).await
                .map_err(|e| AircError::IoError(e))?
        };
        
        if bytes_read == 0 {
            warn!("IRC connection closed by server");
            message_sender.send(IrcMessage::Disconnected).await
                .map_err(|e| AircError::ChannelError(format!("Failed to send disconnect message: {}", e)))?;
            break;
        }
        
        debug!("Received IRC message: {}", line.trim());
        
        if let Err(e) = process_irc_message(&line, &message_sender).await {
            error!("Error processing IRC message: {}", e);
        }
    }
    
    Ok(())
}

async fn process_irc_message(line: &str, sender: &Sender<IrcMessage>) -> Result<()> {
    let trimmed = line.trim();
    
    // Handle PING
    if trimmed.starts_with("PING") {
        let pong = trimmed.replace("PING", "PONG");
        sender.send(IrcMessage::Ping(pong)).await
            .map_err(|e| AircError::ChannelError(format!("Failed to send ping: {}", e)))?;
        return Ok(());
    }
    
    // Handle user list (RPL_NAMREPLY)
    if trimmed.contains("353") {
        if let Some(names_part) = trimmed.split(':').nth(2) {
            let users: Vec<String> = names_part.split_whitespace()
                .map(|name| name.trim_start_matches(['@', '+', '%', '&', '~']))
                .map(String::from)
                .collect();
            
            sender.send(IrcMessage::UserList(users)).await
                .map_err(|e| AircError::ChannelError(format!("Failed to send user list: {}", e)))?;
        }
        return Ok(());
    }
    
    // Handle DCC SEND
    if trimmed.contains("DCC SEND") {
        if let Some(dcc_info) = parse_dcc_send(trimmed)? {
            sender.send(IrcMessage::DccSend {
                filename: dcc_info.filename,
                ip: dcc_info.ip,
                port: dcc_info.port,
                size: dcc_info.size,
            }).await
                .map_err(|e| AircError::ChannelError(format!("Failed to send DCC info: {}", e)))?;
        }
        return Ok(());
    }
    
    // Handle book search results
    if trimmed.contains("PRIVMSG") && trimmed.contains("!") {
        sender.send(IrcMessage::Book(trimmed.to_string())).await
            .map_err(|e| AircError::ChannelError(format!("Failed to send book info: {}", e)))?;
        return Ok(());
    }
    
    // Handle regular messages
    sender.send(IrcMessage::Message(format!("RX: {}", trimmed))).await
        .map_err(|e| AircError::ChannelError(format!("Failed to send message: {}", e)))?;
    
    Ok(())
}

struct DccInfo {
    filename: String,
    ip: String,
    port: String,
    size: u32,
}

fn parse_dcc_send(line: &str) -> Result<Option<DccInfo>> {
    let re = Regex::new(r".*DCC SEND (?P<filename>[^ ]+) (?P<ip>\d+) (?P<port>\d+) (?P<size>\d+)")?;
    
    if let Some(caps) = re.captures(line) {
        let filename = caps.name("filename").unwrap().as_str().to_string();
        let ip = caps.name("ip").unwrap().as_str().to_string();
        let port = caps.name("port").unwrap().as_str().to_string();
        let size_str = caps.name("size").unwrap().as_str();
        
        let size = size_str.trim_end_matches('\r').parse::<u32>()
            .map_err(|e| AircError::IrcProtocolError(format!("Invalid DCC size: {}", e)))?;
        
        Ok(Some(DccInfo { filename, ip, port, size }))
    } else {
        Ok(None)
    }
}

pub fn process_user_input(input: &str, channel: &str) -> String {
    if input.starts_with('/') {
        match input.split_whitespace().next().unwrap_or("") {
            "/join" => {
                if input.len() > 6 {
                    format!("JOIN {}", &input[6..].trim())
                } else {
                    format!("JOIN {}", channel)
                }
            }
            "/quit" | "/q" => "QUIT".to_string(),
            "/s" | "/search" => {
                let search_term = if input.starts_with("/s ") {
                    &input[3..]
                } else if input.starts_with("/search ") {
                    &input[8..]
                } else {
                    ""
                };
                format!("PRIVMSG {} :@search {}", channel, search_term)
            }
            _ => input[1..].to_string(),
        }
    } else {
        format!("PRIVMSG {} :{}", channel, input)
    }
}