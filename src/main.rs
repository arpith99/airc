use std::{io, time::Duration, sync::Arc, error::Error};
use std::fs;
use std::path::PathBuf;
use std::io::{copy};

use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::io::{AsyncWriteExt, AsyncReadExt, AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::{self, Sender, Receiver};
use tokio::sync::Mutex as TokioMutex;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph},
    Frame, Terminal,
};

use regex::Regex;
use clap::Parser;
use rand::Rng;
use zip::ZipArchive;
use dirs;

const DOWNLOAD_PATH: &str = "/home/arpith/Downloads/Books/";
const MAX_MESSAGE_HISTORY: usize = 100;

// Command line arguments
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
}

// IRC Client structure with connection details and communications
#[derive(Clone)]
struct IrcClient {
    server: String,
    username: String,
    nickname: String,
    realname: String,
    reader: Arc<TokioMutex<BufReader<OwnedReadHalf>>>,
    writer: Arc<TokioMutex<OwnedWriteHalf>>,
    sender: Sender<String>,
}

// TUI App structure to manage UI state
struct App {
    should_quit: bool,
    input: String,
    messages: Vec<String>,
    book_list: Vec<String>,
    user_list: Vec<String>,
    cursor_position: usize,
    downloads: Vec<DownloadProgress>,
    current_channel: String,
    connected: bool,
    irc_sender: Option<Sender<String>>,
    // New fields for scrolling functionality
    message_scroll: usize,
    max_scroll: usize,
}

// Structure to track download progress
struct DownloadProgress {
    filename: String,
    progress: u16,
    total_size: u32,
    current_size: u32,
}

impl DownloadProgress {
    fn new(filename: String, total_size: u32) -> Self {
        Self {
            filename,
            progress: 0,
            total_size,
            current_size: 0,
        }
    }

    fn update(&mut self, current_size: u32) {
        self.current_size = current_size;
        if self.total_size > 0 {
            self.progress = ((current_size as f64 / self.total_size as f64) * 100.0) as u16;
        }
    }
}

impl IrcClient {
    async fn new(server: &str, username: &str, nickname: &str, realname: &str) 
        -> Result<(Arc<IrcClient>, Receiver<String>), Box<dyn Error + Send + Sync>> {
        let stream = TcpStream::connect(format!("{}:6667", server)).await?;
        let (sender, receiver) = mpsc::channel(100);
        let (reader, writer) = stream.into_split();
        let reader = BufReader::new(reader);
        
        Ok((Arc::new(IrcClient {
            server: server.to_string(),
            username: username.to_string(),
            nickname: nickname.to_string(),
            realname: realname.to_string(),
            reader: Arc::new(TokioMutex::new(reader)),
            writer: Arc::new(TokioMutex::new(writer)),
            sender,
        }), receiver))
    }
}

impl App {
    fn new() -> Self {
        Self {
            should_quit: false,
            input: String::new(),
            messages: Vec::new(),
            book_list: Vec::new(),
            user_list: Vec::new(),
            cursor_position: 0,
            downloads: Vec::new(),
            current_channel: String::new(),
            connected: false,
            irc_sender: None,
            // Initialize scroll fields
            message_scroll: 0,
            max_scroll: 0,
        }
    }

    fn set_irc_sender(&mut self, sender: Sender<String>) {
        self.irc_sender = Some(sender);
    }

    fn add_message(&mut self, message: String) {
        self.messages.push(message);
        if self.messages.len() > MAX_MESSAGE_HISTORY {
            self.messages.remove(0);
        }
        self.scroll_to_bottom();
    }

    fn add_book(&mut self, book: String) {
        self.book_list.push(book);
    }

    fn add_user(&mut self, user: String) {
        self.user_list.push(user);
    }

    fn add_download(&mut self, filename: String, size: u32) {
        self.downloads.push(DownloadProgress::new(filename, size));
    }

    fn update_download(&mut self, filename: &str, current_size: u32) {
        if let Some(download) = self.downloads.iter_mut().find(|d| d.filename == filename) {
            download.update(current_size);
        }
    }

    // Scrolling functionality
    fn scroll_to_bottom(&mut self) {
        self.message_scroll = self.max_scroll;
    }

    fn update_max_scroll(&mut self, viewport_height: usize) {
        if self.messages.len() > viewport_height {
            self.max_scroll = self.messages.len() - viewport_height;
        } else {
            self.max_scroll = 0;
        }
        
        // Ensure current scroll position doesn't exceed maximum
        if self.message_scroll > self.max_scroll {
            self.message_scroll = self.max_scroll;
        }
    }

    fn scroll_up(&mut self, amount: usize) {
        if self.message_scroll >= amount {
            self.message_scroll -= amount;
        } else {
            self.message_scroll = 0;
        }
    }

    fn scroll_down(&mut self, amount: usize) {
        if self.message_scroll + amount <= self.max_scroll {
            self.message_scroll += amount;
        } else {
            self.message_scroll = self.max_scroll;
        }
    }

    fn handle_events(&mut self) -> io::Result<()> {
        if event::poll(Duration::from_millis(10))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                            self.should_quit = true;
                        }
                        // Scrolling controls
                        KeyCode::PageUp => {
                            self.scroll_up(5); // Scroll up by 5 lines
                        }
                        KeyCode::PageDown => {
                            self.scroll_down(5); // Scroll down by 5 lines
                        }
                        KeyCode::Up if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                            self.scroll_up(1); // Scroll up by 1 line
                        }
                        KeyCode::Down if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                            self.scroll_down(1); // Scroll down by 1 line
                        }
                        KeyCode::Home => {
                            self.message_scroll = 0; // Scroll to the top
                        }
                        KeyCode::End => {
                            self.scroll_to_bottom(); // Scroll to the bottom
                        }
                        // Original key handling
                        KeyCode::Char(c) => {
                            self.input.insert(self.cursor_position, c);
                            self.cursor_position += 1;
                        }
                        KeyCode::Backspace => {
                            if self.cursor_position > 0 {
                                self.cursor_position -= 1;
                                self.input.remove(self.cursor_position);
                            }
                        }
                        KeyCode::Delete => {
                            if self.cursor_position < self.input.len() {
                                self.input.remove(self.cursor_position);
                            }
                        }
                        KeyCode::Left => {
                            if self.cursor_position > 0 {
                                self.cursor_position -= 1;
                            }
                        }
                        KeyCode::Right => {
                            if self.cursor_position < self.input.len() {
                                self.cursor_position += 1;
                            }
                        }
                        KeyCode::Enter => {
                            if !self.input.is_empty() {
                                let input = self.input.clone();
                                self.messages.push(format!("TX: {}", input));
                                
                                // Process the command
                                let command = process_user_input(&input, self.current_channel.clone());
                                
                                // Send the command through tokio runtime
                                let sender = self.irc_sender.clone();
                                if let Some(sender) = sender {
                                    // Using tokio's blocking to send the command
                                    tokio::spawn(async move {
                                        if let Err(e) = sender.send(command).await {
                                            eprintln!("Failed to send command: {}", e);
                                        }
                                    });
                                }
                                
                                self.input.clear();
                                self.cursor_position = 0;
                                // Auto-scroll to bottom when sending a message
                                self.scroll_to_bottom();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }
}

// Process user input to create IRC commands
fn process_user_input(input: &str, channel: String) -> String {
    if input.starts_with("/") {
        if input.starts_with("/join ") || input == "/join" {
            let channel = if input == "/join" { 
                channel 
            } else { 
                input[6..].trim().to_string() 
            };
            format!("JOIN {}\r\n", channel)
        } else if input.starts_with("/quit") || input == "/q" {
            "QUIT\r\n".to_string()
        } else if input.starts_with("/s ") || input.starts_with("/search ") {
            let search_term = if input.starts_with("/s ") {
                &input[3..]
            } else {
                &input[8..]
            };
            format!("PRIVMSG {} :@search {}\r\n", channel, search_term)
        } else {
            // Other commands
            format!("{}\r\n", &input[1..])
        }
    } else {
        // Regular message
        format!("PRIVMSG {} :{}\r\n", channel, input)
    }
}

async fn init(client: Arc<IrcClient>) -> Result<(), Box<dyn Error + Send + Sync>> {
    client.sender.send(format!("CAP LS\r\n")).await?;
    client.sender.send(format!("NICK {}\r\n", client.nickname)).await?;
    client.sender.send(format!("USER {} {} {} :{}\r\n", client.username, client.nickname, client.server, client.realname)).await?;
    Ok(())
}

async fn write(client: Arc<IrcClient>, mut receiver: Receiver<String>) -> Result<(), Box<dyn Error + Send + Sync>> {
    while let Some(message) = receiver.recv().await {
        let mut writer = client.writer.lock().await;
        writer.write_all(format!("{}", message).as_bytes()).await?;
        if message.starts_with("QUIT") {
            break;
        }
    }
    Ok(())
}

async fn read(client: Arc<IrcClient>, app_sender: Sender<UIMessage>) -> Result<(), Box<dyn Error + Send + Sync>> {
    loop {
        let mut line = String::new();
        let bytes_read = {
            let mut reader = client.reader.lock().await;
            reader.read_line(&mut line).await?
        };
        
        if bytes_read == 0 {
            break;
        }
        
        // Send message to UI
        app_sender.send(UIMessage::Message(format!("RX: {}", line))).await?;
        
        // Process IRC protocol
        if line.starts_with("PING") {
            let pong = line.replace("PING", "PONG");
            client.sender.send(pong).await?;
        }
        
        // Process book search results
        if line.contains("PRIVMSG") && line.contains("!") {
            // Simple parsing for book information
            // This would need to be adapted to actual format
            app_sender.send(UIMessage::Book(line.clone())).await?;
        }
        
        // Process user list
        if line.contains("353") {  // RPL_NAMREPLY
            if let Some(names_part) = line.split(':').nth(2) {
                for name in names_part.split_whitespace() {
                    app_sender.send(UIMessage::User(name.to_string())).await?;
                }
            }
        }
        
        // Process DCC SEND
        if line.contains("DCC SEND") {
            process_dcc_send(&line, app_sender.clone()).await?;
        }
    }
    Ok(())
}

async fn process_dcc_send(line: &str, app_sender: Sender<UIMessage>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let re = Regex::new(r".*DCC SEND (?P<filename>[^ ]+) (?P<ip>\d+) (?P<port>\d+) (?P<size>\d+)").unwrap();
    if let Some(caps) = re.captures(&line) {
        let filename = caps.name("filename").unwrap().as_str().to_string();
        let ip = caps.name("ip").unwrap().as_str().to_string();
        let port = caps.name("port").unwrap().as_str().to_string();
        let size = caps.name("size").unwrap().as_str().to_string();
        
        // Add download to UI
        let size_num: u32 = size[..size.len()-2].to_string().trim().parse().unwrap();
        app_sender.send(UIMessage::AddDownload(filename.clone(), size_num)).await?;
        
        // Start download in background
        let app_sender_clone = app_sender.clone();
        let filename_clone = filename.clone();
        
        tokio::spawn(async move {
            if let Err(e) = dcc_receive(&filename_clone, &ip, &port, &size, app_sender_clone).await {
                eprintln!("Download error: {}", e);
            }
        });
    }
    Ok(())
}

async fn dcc_receive(filename: &str, ip: &str, port: &str, size: &str, app_sender: Sender<UIMessage>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut stream = TcpStream::connect(format!("{}:{}", ip, port)).await?;
    let size: u32 = size[..size.len()-2].to_string().trim().parse().unwrap();
    
    let mut buffer = vec![0u8; 1024];
    let mut received = 0;
    let fname = format!("{}{}", DOWNLOAD_PATH, filename);
    let mut file = tokio::fs::File::create(&fname).await?;
    
    loop {
        let n = stream.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        
        received += n as u32;
        app_sender.send(UIMessage::UpdateDownload(filename.to_string(), received)).await?;
        
        file.write_all(&buffer[0..n]).await?;
        
        if received >= size {
            break;
        }
    }
    
    app_sender.send(UIMessage::Message(format!("Download complete: {}", filename))).await?;
    
    // Unzip if it's a zip file
    if filename.ends_with(".zip") {
        app_sender.send(UIMessage::Message(format!("Extracting: {}", filename))).await?;
        
        tokio::task::spawn_blocking(move || {
            match unzip_file(&fname) {
                Ok(()) => app_sender.blocking_send(UIMessage::Message("Extraction complete".into())).ok(),
                Err(e) => app_sender.blocking_send(UIMessage::Message(format!("Extraction error: {}", e))).ok(),
            }
        });
    }
    
    Ok(())
}

fn unzip_file(filename: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    let file = fs::File::open(filename)?;
    let mut archive = ZipArchive::new(file)?;
    let mut file = archive.by_index(0)?;
    let outpath = PathBuf::from(filename.strip_suffix(".zip").unwrap_or(filename));
    let mut outfile = fs::File::create(&outpath)?;
    copy(&mut file, &mut outfile)?;
    Ok(())
}

fn get_download_path() -> PathBuf {
    dirs::download_dir()
        .unwrap_or_else(|| PathBuf::from("~/Downloads"))
        .join("Books")
}

// Messages for UI updates
enum UIMessage {
    Message(String),
    Book(String),
    User(String),
    AddDownload(String, u32),
    UpdateDownload(String, u32),
}

async fn run_tui_app() -> Result<(), Box<dyn Error + Send + Sync>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Parse command line args
    let args = Args::parse();
    
    let server = args.server.unwrap_or_else(|| "irc.undernet.org".to_string());
    let channel = args.channel.unwrap_or_else(|| "#bookz".to_string());
    
    // Generate username/nickname if not provided
    let (username, nickname, realname) = if let Some(username) = args.username {
        (username.clone(), username.clone(), username)
    } else {
        let mut rng = rand::rng();
        let random_num = rng.random_range(0..=99999);
        let random_nickname = format!("bworm{}", random_num);
        (random_nickname.clone(), random_nickname, "Book Worm".to_string())
    };

    // Create application state
    let mut app = App::new();
    app.current_channel = channel.clone();
    
    // Create channels for UI updates
    let (ui_sender, mut ui_receiver) = mpsc::channel::<UIMessage>(100);
    
    // Setup IRC client in the background
    let ui_sender_clone = ui_sender.clone();
    let irc_handle = tokio::spawn(async move {
        match IrcClient::new(&server, &username, &nickname, &realname).await {
            Ok((client, irc_receiver)) => {
                // Send connection success message to UI
                ui_sender_clone.send(UIMessage::Message(format!("Connected to {} as {}", server, nickname))).await.ok();
                
                // Initialize IRC connection
                let init_client = client.clone();
                tokio::spawn(async move {
                    if let Err(e) = init(init_client.clone()).await {
                        let error_message = format!("Init error: {}", e);
                        ui_sender.send(UIMessage::Message(error_message)).await.ok();
                    }
                });
                
                // Spawn writer task
                let write_client = client.clone();
                tokio::spawn(async move {
                    if let Err(e) = write(write_client, irc_receiver).await {
                        eprintln!("Write error: {}", e);
                    }
                });
                
                // Spawn reader task
                let read_client = client.clone();
                tokio::spawn(async move {
                    if let Err(e) = read(read_client, ui_sender_clone).await {
                        eprintln!("Read error: {}", e);
                    }
                });
                
                // Return the sender to allow sending commands
                Ok(client.sender.clone())
            },
            Err(e) => {
                ui_sender_clone.send(UIMessage::Message(format!("Connection error: {}", e))).await.ok();
                Err(e)
            }
        }
    });
    
    // Wait for IRC client to connect and get the sender
    let timeout = Duration::from_secs(5);
    let irc_sender = match tokio::time::timeout(timeout, irc_handle).await {
        Ok(result) => match result {
            Ok(sender_result) => match sender_result {
                Ok(sender) => {
                    app.connected = true;
                    Some(sender)
                },
                Err(_) => None,
            },
            Err(_) => None,
        },
        Err(_) => {
            app.add_message("Connection timeout. Check server address.".to_string());
            None
        },
    };
    
    if let Some(sender) = irc_sender {
        app.set_irc_sender(sender);
    }

    // Main UI loop
    let ui_task = tokio::spawn(async move {
        while !app.should_quit {
            // Process UI events
            if let Err(e) = app.handle_events() {
                eprintln!("UI event error: {}", e);
                break;
            }
            
            // Process IRC messages
            while let Ok(message) = ui_receiver.try_recv() {
                match message {
                    UIMessage::Message(msg) => app.add_message(msg),
                    UIMessage::Book(book) => app.add_book(book),
                    UIMessage::User(user) => app.add_user(user),
                    UIMessage::AddDownload(filename, size) => app.add_download(filename, size),
                    UIMessage::UpdateDownload(filename, progress) => app.update_download(&filename, progress),
                }
            }
            
            // Draw UI
            terminal.draw(|f| ui(f, &mut app))?;
            
            // Prevent CPU spinning
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        
        // Clean up the terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;
        
        Ok::<(), io::Error>(())
    });
    
    // Wait for UI task to complete
    match ui_task.await {
        Ok(result) => result?,
        Err(e) => return Err(Box::new(io::Error::new(io::ErrorKind::Other, format!("UI task error: {}", e)))),
    }
    
    Ok(())
}

fn ui(f: &mut Frame, app: &mut App) {
    let size = f.area();

    // First split the screen into main content and footer
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Main content
            Constraint::Length(4), // Footer (input + help)
        ])
        .split(size);

    let main_area = vertical_chunks[0];
    let footer_area = vertical_chunks[1];

    // Main horizontal layout within the main area
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(10), // Server list
            Constraint::Percentage(80), // Middle section
            Constraint::Percentage(10), // User list
        ])
        .split(main_area);

    // Middle section split vertically
    let middle_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(70), // Book list
            Constraint::Percentage(30), // Bottom section
        ])
        .split(main_chunks[1]);

    // Bottom section split vertically
    let bottom_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Download progress 1
            Constraint::Length(3), // Download progress 2
            Constraint::Min(1),   // Rx/Tx message log
        ])
        .split(middle_chunks[1]);

    // Footer section
    let footer_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Input box
            Constraint::Length(1), // Help/key hints
        ])
        .split(footer_area);

    // Draw server info
    let server_title = format!("Server: {}", app.current_channel);
    let server_block = Block::default()
        .title(server_title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue));
    
    let connection_status = if app.connected {
        "Connected"
    } else {
        "Disconnected"
    };
    
    let server_items = vec![
        ListItem::new(connection_status),
        ListItem::new("Type /join to join"),
    ];
    
    let server_list = List::new(server_items)
        .block(server_block)
        .style(Style::default().fg(Color::Cyan));
    f.render_widget(server_list, main_chunks[0]);

    // Draw book list
    let book_block = Block::default()
        .title("Book list")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::LightYellow));
    
    let book_items: Vec<ListItem> = app.book_list
        .iter()
        .map(|book| ListItem::new(book.as_str()))
        .collect();
    
    let book_list = List::new(book_items)
        .block(book_block)
        .style(Style::default().fg(Color::LightYellow));
    f.render_widget(book_list, middle_chunks[0]);

    // Draw user list
    let user_block = Block::default()
        .title("User list")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    
    let user_items: Vec<ListItem> = app.user_list
        .iter()
        .map(|user| ListItem::new(user.as_str()))
        .collect();
    
    let user_list = List::new(user_items)
        .block(user_block)
        .style(Style::default().fg(Color::Red));
    f.render_widget(user_list, main_chunks[2]);

    // Draw download progress bars (up to 2)
    let downloads = app.downloads.iter().take(2).collect::<Vec<_>>();
    
    if downloads.len() > 0 {
        let download = &downloads[0];
        let download_title = format!("Download: {}", download.filename);
        let download_block = Block::default()
            .title(download_title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green));
        
        let gauge = Gauge::default()
            .block(download_block)
            .gauge_style(Style::default().fg(Color::Green))
            .percent(download.progress);
        f.render_widget(gauge, bottom_chunks[0]);
    } else {
        let empty_block = Block::default()
            .title("No active downloads")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty_block, bottom_chunks[0]);
    }
    
    if downloads.len() > 1 {
        let download = &downloads[1];
        let download_title = format!("Download: {}", download.filename);
        let download_block = Block::default()
            .title(download_title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green));
        
        let gauge = Gauge::default()
            .block(download_block)
            .gauge_style(Style::default().fg(Color::Green))
            .percent(download.progress);
        f.render_widget(gauge, bottom_chunks[1]);
    } else {
        let empty_block = Block::default()
            .title("No second download")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty_block, bottom_chunks[1]);
    }

    // Draw message log with scrolling
    // Calculate the viewport height for messages
    let message_height = bottom_chunks[2].height as usize - 2; // Subtract 2 for borders
    
    // Update max scroll based on current viewport
    app.update_max_scroll(message_height);
    
    // Calculate visible slice of messages based on scroll position
    let visible_messages = if app.messages.is_empty() {
        &[]
    } else {
        let start = if app.message_scroll < app.messages.len() {
            app.messages.len() - app.message_scroll - 
                std::cmp::min(message_height, app.messages.len() - app.message_scroll)
        } else {
            0
        };
        
        let end = if app.message_scroll < app.messages.len() {
            app.messages.len() - app.message_scroll
        } else {
            0
        };
        
        &app.messages[start..end]
    };

    // Add scroll indicator to the title if scrolling is active
    let message_title = if app.message_scroll > 0 {
        format!("IRC Messages (↑{}/{}↓)", app.message_scroll, app.max_scroll)
    } else if app.max_scroll > 0 {
        format!("IRC Messages (↓{})", app.max_scroll)
    } else {
        "IRC Messages".to_string()
    };

    let message_block = Block::default()
        .title(message_title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));

    // Convert the visible messages to styled Lines
    let messages: Vec<Line> = visible_messages
        .iter()
        .map(|m| {
            if m.starts_with("TX: ") {
                Line::from(Span::styled(m, Style::default().fg(Color::Green)))
            } else if m.starts_with("RX: ") {
                Line::from(Span::styled(m, Style::default().fg(Color::Yellow)))
            } else {
                Line::from(Span::styled(m, Style::default().fg(Color::White)))
            }
        })
        .collect();

    let message_text = Text::from(messages);
    let message_paragraph = Paragraph::new(message_text)
        .block(message_block)
        .wrap(ratatui::widgets::Wrap { trim: true });

    f.render_widget(message_paragraph, bottom_chunks[2]);

    // Draw input box with cursor
    let input_block = Block::default()
        .title("Input")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let input = Paragraph::new(app.input.as_str())
        .style(Style::default().fg(Color::White))
        .block(input_block);
    
    f.render_widget(input, footer_chunks[0]);
    
    // Make the cursor visible and position it
    f.set_cursor_position(Position {
        x: footer_chunks[0].x + 1 + app.cursor_position as u16,
        y: footer_chunks[0].y + 1,
    });

    // Update help text to include scrolling instructions
    let help_text = vec![
        Span::styled("/join", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": Join channel | "),
        Span::styled("/s query", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": Search books | "),
        Span::styled("PgUp/PgDn", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": Scroll | "),
        Span::styled("Ctrl+Q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": Quit"),
    ];
    
    let hints = Paragraph::new(Line::from(help_text))
        .style(Style::default().fg(Color::White));
    f.render_widget(hints, footer_chunks[1]);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    // Ensure download directory exists
    if let Err(e) = fs::create_dir_all(get_download_path()) {
        eprintln!("Error creating download directory: {}", e);
    }
    
    // Start the TUI application
    run_tui_app().await
}
