mod config;
mod error;

use chrono::Local;
use clap::Parser;
use colored::Colorize;
use config::Config;
use error::{AircError, Result};
use once_cell::sync::Lazy;
use rand::Rng;
use regex::Regex;
use std::fs;
use std::io::{Write, copy, stdout};
use std::path::PathBuf;
use std::process::exit;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::{info, warn, debug};
use zip::ZipArchive;

// Constants
const IRC_PORT: u16 = 6667;
const CHANNEL_BUFFER_SIZE: usize = 100;
const DCC_CHUNK_SIZE: usize = 65536; // 64KB
const DEFAULT_REALNAME: &str = "Book Worm";
const NICKNAME_PREFIX: &str = "bworm";
const MAX_NICKNAME_SUFFIX: u32 = 99999;
const QUIT_DELAY_MS: u64 = 100;
const SEARCHBOT_RESULTS_PREFIX: &str = "SearchBot_results";
const ZIP_EXTENSION: &str = ".zip";
const MAX_FILE_SIZE_BYTES: u64 = 10 * 1024 * 1024 * 1024; // 10GB max file size
const MAX_FILENAME_LENGTH: usize = 255;
const MAX_RETRY_ATTEMPTS: u32 = 3;
const RETRY_DELAY_MS: u64 = 1000;

// Compile regexes once at startup
static SEARCH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/s(earch)? (?P<search_term>.*)").unwrap());

static ENTRY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^/(?P<entry_num>\d+)$").unwrap());

static DCC_SEND_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i).*DCC SEND (?P<filename>\S+) (?P<ip>\d+) (?P<port>\d+) (?P<size>\d+)").unwrap()
});

#[derive(Clone)]
struct IrcClient {
    config: Config,
    username: String,
    nickname: String,
    realname: String,
    reader: Arc<tokio::sync::Mutex<BufReader<OwnedReadHalf>>>,
    writer: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    sender: Sender<String>,
    search_results: Arc<tokio::sync::Mutex<Vec<String>>>,
    dcc_tasks: Arc<tokio::sync::Mutex<tokio::task::JoinSet<()>>>,
}

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
    /// Download path for DCC files
    #[clap(short, long)]
    download_path: Option<String>,
}

// Retry a network operation with exponential backoff
async fn retry_with_backoff<F, Fut, T>(
    operation: F,
    operation_name: &str,
) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempts = 0;
    loop {
        attempts += 1;
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) if attempts >= MAX_RETRY_ATTEMPTS => {
                return Err(AircError::Connection(format!(
                    "{} failed after {} attempts: {}",
                    operation_name, MAX_RETRY_ATTEMPTS, e
                )));
            }
            Err(e) => {
                let delay = RETRY_DELAY_MS * 2u64.pow(attempts - 1);
                warn!(
                    "{} attempt {}/{} failed: {}. Retrying in {}ms...",
                    operation_name, attempts, MAX_RETRY_ATTEMPTS, e, delay
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
            }
        }
    }
}

impl IrcClient {
    async fn new(
        config: Config,
        username: &str,
        nickname: &str,
        realname: &str,
    ) -> Result<(Arc<IrcClient>, Receiver<String>)> {
        info!("Connecting to {} ({})", config.server, config.channel);

        // Connect with timeout and retry logic
        let server_addr = format!("{}:{}", config.server, IRC_PORT);
        let timeout_secs = config.connection_timeout_secs;
        let server_name = config.server.clone();

        let stream = retry_with_backoff(
            || async {
                tokio::time::timeout(
                    tokio::time::Duration::from_secs(timeout_secs),
                    TcpStream::connect(&server_addr)
                )
                .await
                .map_err(|_| AircError::Timeout(format!(
                    "Connection to {} timed out after {} seconds",
                    server_name, timeout_secs
                )))?
                .map_err(|e| AircError::Connection(format!(
                    "Failed to connect to {}: {}",
                    server_name, e
                )))
            },
            &format!("IRC connection to {}", config.server),
        )
        .await?;

        let (sender, receiver) = mpsc::channel(CHANNEL_BUFFER_SIZE);
        let (reader, writer) = stream.into_split();
        let reader = BufReader::new(reader);

        // Ensure download directory exists
        tokio::fs::create_dir_all(&config.download_path).await?;

        Ok((
            Arc::new(IrcClient {
                config,
                username: username.to_string(),
                nickname: nickname.to_string(),
                realname: realname.to_string(),
                reader: Arc::new(tokio::sync::Mutex::new(reader)),
                writer: Arc::new(tokio::sync::Mutex::new(writer)),
                sender,
                search_results: Arc::new(tokio::sync::Mutex::new(Vec::new())),
                dcc_tasks: Arc::new(tokio::sync::Mutex::new(tokio::task::JoinSet::new())),
            }),
            receiver,
        ))
    }
}

async fn init(client: Arc<IrcClient>) -> Result<()> {
    debug!("Sending IRC registration");
    client.sender.send("CAP END\r\n".to_string()).await?;

    client
        .sender
        .send(format!("NICK {}\r\n", client.nickname))
        .await?;

    client
        .sender
        .send(format!(
            "USER {} {} {} :{}\r\n",
            client.username, client.nickname, client.config.server, client.realname
        ))
        .await?;

    Ok(())
}

async fn write(
    client: Arc<IrcClient>,
    mut receiver: Receiver<String>,
) -> Result<()> {
    while let Some(message) = receiver.recv().await {
        print_sent_line(&message);

        {
            let mut writer = client.writer.lock().await;
            writer.write_all(message.as_bytes()).await?;
            writer.flush().await?;
        } // Lock released here

        // Check if this is a QUIT command (case-insensitive, no allocation)
        if message.len() >= 4 && message[..4].eq_ignore_ascii_case("QUIT") {
            print_line("Exiting...\n", true);
            print_line("Goodbye!\n", true);
            // Give other tasks time to clean up
            tokio::time::sleep(tokio::time::Duration::from_millis(QUIT_DELAY_MS)).await;
            exit(0);
        }
    }

    Ok(())
}

// Case-insensitive substring search without allocation
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }

    let haystack_lower: String = haystack.chars().flat_map(|c| c.to_lowercase()).collect();
    let needle_lower: String = needle.chars().flat_map(|c| c.to_lowercase()).collect();
    haystack_lower.contains(&needle_lower)
}

// Local command to search the search results
async fn handle_search_results(client: Arc<IrcClient>, search_term: &str) {
    // Collect matching indices and lines to avoid holding lock during I/O
    let matches: Vec<(usize, String)> = {
        let list = client.search_results.lock().await;
        list.iter()
            .enumerate()
            .filter(|(_, book_line)| contains_ignore_case(book_line, search_term))
            .map(|(i, book_line)| (i, book_line.clone()))
            .collect()
    };

    if matches.is_empty() {
        print_line(&format!("No results found for '{}'\n", search_term), true);
    } else {
        for (i, book_line) in matches {
            print_line(&format!("{}: {}\n", i, book_line), true);
        }
    }
}

// Returns Option<String> - Some(msg) if should send to IRC, None if shouldn't
async fn process_command(client: Arc<IrcClient>, command: &str) -> Option<String> {
    // JOIN command
    if command == "/join" || command == "/j" {
        return Some(format!("JOIN {}\r\n", client.config.channel));
    }

    // QUIT command
    if command.starts_with("/quit") || command.starts_with("/q ") || command == "/q" {
        // Extract optional quit message
        let quit_msg = command
            .strip_prefix("/quit ")
            .or_else(|| command.strip_prefix("/q "))
            .unwrap_or("");

        return if quit_msg.is_empty() {
            Some("QUIT\r\n".to_string())
        } else {
            Some(format!("QUIT :{}\r\n", quit_msg))
        };
    }

    // SEARCH command
    if let Some(caps) = SEARCH_RE.captures(command) {
        let search_term = caps.name("search_term").unwrap().as_str();
        print_line(&format!("Searching for: {}\n", search_term), true);
        return Some(format!("PRIVMSG {} :@search {}\r\n", client.config.channel, search_term));
    }

    // ENTRY NUMBER selection
    if let Some(caps) = ENTRY_RE.captures(command) {
        let entry_str = caps.name("entry_num").unwrap().as_str();

        match entry_str.parse::<usize>() {
            Ok(entry_num) => {
                print_line(&format!("Requesting entry number: {}\n", entry_num), true);

                // Look up the actual book entry
                let results = client.search_results.lock().await;
                if let Some(book_entry) = results.get(entry_num) {
                    print_line(&format!("Book entry: {}\n", book_entry), true);
                    return Some(format!("PRIVMSG {} :{}\r\n", client.config.channel, book_entry));
                } else {
                    print_line(
                        &format!("Entry number {} not found in search results\n", entry_num),
                        true,
                    );
                    return None; // Don't send anything
                }
            }
            Err(_) => {
                print_line(
                    &format!("Invalid entry number: '{}'\n", entry_str),
                    true,
                );
                return None;
            }
        }
    }

    // Default: treat as raw IRC command
    Some(format!(
        "{}\r\n",
        command.strip_prefix('/').unwrap_or(command).trim()
    ))
}

async fn cli(client: Arc<IrcClient>) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut command = String::new();

    loop {
        command.clear();
        let bytes_read = reader.read_line(&mut command).await?;
        if bytes_read == 0 {
            break; // EOF
        }
        let trimmed = command.trim();

        // Handle local commands (don't send to IRC)
        if let Some(search_term) = trimmed.strip_prefix("/ss ") {
            handle_search_results(client.clone(), search_term).await;
            continue;
        }

        // Handle IRC commands (generate messages to send)
        if let Some(message) = process_command(client.clone(), trimmed).await {
            client.sender.send(message).await?;
        }
    }

    Ok(())
}

fn print_timestamp() {
    let local = Local::now();
    print!(
        "{}",
        format!("{}", local.format("[%H:%M:%S] ")).bright_black()
    );
}

fn print_sent_line(line: &str) {
    print_timestamp();
    print!("{}", format!("{}: ", "TX").red());
    print_line(line, false);
}

fn print_received_line(line: &str) {
    print_timestamp();
    print!("{}", format!("{}: ", "RX").green());
    print_line(line, false);
}

fn print_line(line: &str, ts_flag: bool) {
    let colon_index = line.find(" :").unwrap_or(0);
    let prefix = &line[..colon_index];
    let message = &line[colon_index..];
    if ts_flag {
        print_timestamp();
    }
    print!("{}", prefix.yellow());
    print!("{}", message.white());
    let _ = stdout().flush(); // Ignore flush errors (e.g., terminal closed)
}

async fn unzip_file(filename: &str) -> Result<String> {
    let filename_owned = filename.to_string();

    // Run blocking zip operations in a separate thread pool
    let result = tokio::task::spawn_blocking(move || -> Result<(String, u64)> {
        let file = fs::File::open(&filename_owned)
            .map_err(|e| format!("Failed to open zip file '{}': {}", filename_owned, e))?;

        let mut archive = ZipArchive::new(file)
            .map_err(|e| format!("Failed to read zip archive '{}': {}", filename_owned, e))?;

        let mut zipped_file = archive.by_index(0)
            .map_err(|e| format!("Failed to access first file in archive: {}", e))?;

        let file_size = zipped_file.size();

        let outpath = PathBuf::from(
            filename_owned.strip_suffix(ZIP_EXTENSION)
                .ok_or(format!("Filename doesn't end with {}", ZIP_EXTENSION))?
        );

        let mut outfile = fs::File::create(&outpath)
            .map_err(|e| format!("Failed to create output file '{}': {}", outpath.display(), e))?;

        copy(&mut zipped_file, &mut outfile)
            .map_err(|e| format!("Failed to extract file: {}", e))?;

        let outpath_str = outpath.to_str()
            .ok_or("Output path contains invalid UTF-8")?
            .to_string();

        Ok((outpath_str, file_size))
    }).await?;

    let (outpath_str, file_size) = result?;

    print_line(
        &format!(
            "File {} extracted to \"{}\" ({} bytes)\n",
            filename,
            outpath_str,
            file_size,
        ),
        true,
    );

    Ok(outpath_str)
}

// Convert DCC IP (32-bit integer) to dotted-quad notation
// This function is primarily for testing; actual code inlines the conversion
#[cfg_attr(not(test), allow(dead_code))]
fn decode_dcc_ip_address(ip_str: &str) -> Result<String> {
    let ip_num: u32 = ip_str.parse()?;
    Ok(std::net::Ipv4Addr::from(ip_num).to_string())
}

// Sanitize and validate filename for safe filesystem operations
fn sanitize_filename(filename: &str) -> Result<String> {
    // Check length
    if filename.is_empty() {
        return Err("Filename is empty".into());
    }
    if filename.len() > MAX_FILENAME_LENGTH {
        return Err(format!("Filename too long (max {} chars)", MAX_FILENAME_LENGTH).into());
    }

    // Check for null bytes
    if filename.contains('\0') {
        return Err("Filename contains null byte".into());
    }

    // Check for path traversal attempts
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return Err(format!("Invalid filename (contains path separators): {}", filename).into());
    }

    // Check for dangerous characters on Windows
    let dangerous_chars = ['<', '>', ':', '"', '|', '?', '*'];
    if filename.chars().any(|c| dangerous_chars.contains(&c)) {
        return Err(format!("Filename contains invalid characters: {}", filename).into());
    }

    // Check for reserved names on Windows
    let name_upper = filename.to_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4",
                    "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
                    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];

    let base_name = name_upper.split('.').next().unwrap_or("");
    if reserved.contains(&base_name) {
        return Err(format!("Filename is a reserved name: {}", filename).into());
    }

    Ok(filename.to_string())
}

// Construct safe file path within download directory
fn safe_file_path(download_path: &str, filename: &str) -> Result<PathBuf> {
    let sanitized = sanitize_filename(filename)?;

    let base_path = PathBuf::from(download_path);
    let file_path = base_path.join(&sanitized);

    // Ensure the resulting path is still within the download directory
    let canonical_base = base_path.canonicalize()
        .unwrap_or_else(|_| base_path.clone());

    // For new files, we can't canonicalize yet, so we check the parent
    if let Some(parent) = file_path.parent() {
        let canonical_parent = parent.canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());

        if !canonical_parent.starts_with(&canonical_base) {
            return Err(format!(
                "Path traversal detected: {} escapes {}",
                filename, download_path
            ).into());
        }
    }

    Ok(file_path)
}

async fn dcc_receive(
    filename: &str,
    ip: &str,
    port: &str,
    size: &str,
    download_path: &str,
    connection_timeout: u64,
    transfer_timeout: u64,
) -> Result<String> {
    // Convert IP if it's in DCC format (32-bit integer)
    let ip_addr = match ip.parse::<u32>() {
        Ok(ip_num) => std::net::Ipv4Addr::from(ip_num).to_string(),
        Err(_) => ip.to_string(),
    };

    let file_size: u64 = size
        .trim()
        .parse()
        .map_err(|e| format!("Invalid file size '{}': {}", size, e))?;

    // Validate file size
    if file_size > MAX_FILE_SIZE_BYTES {
        return Err(format!(
            "File size {} bytes exceeds maximum allowed size of {} bytes",
            file_size, MAX_FILE_SIZE_BYTES
        ).into());
    }

    let port_num: u16 = port
        .trim()
        .parse()
        .map_err(|e| format!("Invalid port '{}': {}", port, e))?;

    print_line(
        &format!("Connecting to {}:{}...\n", ip_addr, port_num),
        true,
    );

    // Connect with timeout and retry logic
    let dcc_addr = format!("{}:{}", ip_addr, port_num);
    let mut stream = retry_with_backoff(
        || async {
            tokio::time::timeout(
                tokio::time::Duration::from_secs(connection_timeout),
                TcpStream::connect(&dcc_addr)
            )
            .await
            .map_err(|_| AircError::Timeout(format!(
                "DCC connection to {} timed out after {} seconds",
                dcc_addr, connection_timeout
            )))?
            .map_err(|e| AircError::Connection(format!(
                "Failed to connect to {}: {}",
                dcc_addr, e
            )))
        },
        &format!("DCC connection to {}", dcc_addr),
    )
    .await?;

    // Use safe path construction
    let file_path = safe_file_path(download_path, filename)?;

    let mut file = tokio::fs::File::create(&file_path)
        .await
        .map_err(|e| format!("Failed to create file '{}': {}", file_path.display(), e))?;

    // Stream the file instead of loading into memory
    let mut total_bytes = 0u64;
    let mut buffer = vec![0u8; DCC_CHUNK_SIZE];

    while total_bytes < file_size {
        let to_read = std::cmp::min(buffer.len() as u64, file_size - total_bytes) as usize;

        // Read with timeout to detect stalled transfers
        let bytes_read = tokio::time::timeout(
            tokio::time::Duration::from_secs(transfer_timeout),
            stream.read(&mut buffer[..to_read])
        )
        .await
        .map_err(|_| format!("DCC transfer timed out after {} seconds", transfer_timeout))??;

        if bytes_read == 0 {
            return Err(format!(
                "Connection closed after {} of {} bytes",
                total_bytes, file_size
            )
            .into());
        }

        file.write_all(&buffer[..bytes_read]).await?;
        total_bytes += bytes_read as u64;

        // Send DCC ACK (total bytes received in network byte order)
        // DCC protocol uses u32 which wraps around for files >4GB
        let ack_bytes = (total_bytes as u32).to_be_bytes();
        stream.write_all(&ack_bytes).await?;
    }

    file.flush().await?;
    stream.flush().await?;

    // Gracefully close the connection
    stream.shutdown().await?;

    print_line(
        &format!("Received file: {} ({} bytes)\n", filename, total_bytes),
        true,
    );
    Ok(file_path.to_string_lossy().to_string())
}

async fn read_lines_to_vec(path: &str) -> Result<Vec<String>> {
    let file = File::open(path).await?;
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut line_stream = reader.lines();
    while let Some(line) = line_stream.next_line().await? {
        lines.push(line);
    }
    Ok(lines)
}

// Handle received DCC file
async fn handle_dcc_file(client: Arc<IrcClient>, fpath: String) -> Result<()> {
    let path = PathBuf::from(&fpath);

    if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
        if filename.starts_with(SEARCHBOT_RESULTS_PREFIX) && filename.ends_with(ZIP_EXTENSION) {
            let txt_file = unzip_file(&fpath).await?;
            let book_entries = read_lines_to_vec(&txt_file).await?;

            // Update search results and clone for display to avoid holding lock during I/O
            let num_results = book_entries.len();
            let display_list = {
                let mut results = client.search_results.lock().await;
                *results = book_entries;
                results.clone()
            };

            // Display without holding lock
            for (i, book_line) in display_list.iter().enumerate() {
                print_line(&format!("{}: {}\n", i, book_line), true);
            }
            print_line(&format!("Loaded {} books into search results\n", num_results), true);
        } else {
            // Download other files like ebooks
            print_line(&format!("Downloaded file: {}\n", filename), true);
        }
    }

    Ok(())
}

// Generate PONG response for PING messages
fn create_pong_response(line: &str) -> Option<String> {
    line.strip_prefix("PING ").map(|rest| format!("PONG {}", rest))
}

// Handle DCC SEND request by spawning download task
async fn handle_dcc_send_request(client: &Arc<IrcClient>, line: &str) {
    // Only process if line contains DCC SEND to avoid excessive regex checking
    if !line.to_uppercase().contains("DCC SEND") {
        return;
    }

    if let Some(caps) = DCC_SEND_RE.captures(line) {
        // Extract captures safely - regex guarantees these exist if it matched
        let filename = caps.name("filename").map(|m| m.as_str().to_string());
        let ip = caps.name("ip").map(|m| m.as_str().to_string());
        let port = caps.name("port").map(|m| m.as_str().to_string());
        let size = caps.name("size").map(|m| m.as_str().to_string());

        if let (Some(filename), Some(ip), Some(port), Some(size)) = (filename, ip, port, size) {
            info!("✓ DCC SEND: {}, {} bytes", filename, size);

            print_line(
                &format!("Received DCC SEND request for file: {}\n", filename),
                true,
            );
            print_line(
                &format!("IP: {}, Port: {}, Size: {} bytes\n", ip, port, size),
                true,
            );

            // Spawn download task
            let client_clone = client.clone();
            let download_path = client.config.download_path.clone();
            let connection_timeout = client.config.connection_timeout_secs;
            let transfer_timeout = client.config.dcc_timeout_secs;

            // Track the spawned task for graceful shutdown
            let mut tasks = client.dcc_tasks.lock().await;
            tasks.spawn(async move {
                match dcc_receive(&filename, &ip, &port, &size, &download_path, connection_timeout, transfer_timeout).await {
                    Ok(fpath) => {
                        if let Err(e) = handle_dcc_file(client_clone, fpath).await {
                            warn!("Error handling DCC file: {}", e);
                            print_line(&format!("Error handling DCC file: {}\n", e), true);
                        }
                    }
                    Err(e) => {
                        warn!("Error receiving DCC file: {}", e);
                        print_line(&format!("Error receiving DCC file: {}\n", e), true);
                    }
                }
            });
        } else {
            warn!("DCC SEND regex matched but failed to extract all fields");
        }
    } else {
        // Line contains "DCC SEND" but regex failed
        // This is expected for NOTICE announcements
        if !line.contains(" NOTICE ") {
            warn!("DCC SEND regex mismatch: {}", line.trim());
        }
    }
}

async fn receive_loop(client: Arc<IrcClient>) -> Result<()> {
    let mut message_count = 0u64;
    let mut last_heartbeat = std::time::Instant::now();

    loop {
        // Heartbeat every 60 seconds to show we're alive
        if last_heartbeat.elapsed().as_secs() >= 60 {
            info!("❤️  Heartbeat - processed {} messages", message_count);
            last_heartbeat = std::time::Instant::now();
        }

        // Read raw bytes until newline (handles invalid UTF-8)
        let (line, bytes_read) = {
            let mut reader = client.reader.lock().await;

            let mut buffer = Vec::new();
            let bytes = reader.read_until(b'\n', &mut buffer).await?;

            // Convert to String, replacing invalid UTF-8 with � (replacement character)
            let line = String::from_utf8_lossy(&buffer).to_string();

            (line, bytes)
        };

        if bytes_read == 0 {
            warn!("Connection closed by server (0 bytes read)");
            break; // Connection closed
        }

        message_count += 1;
        print_received_line(&line);

        // Handle PING/PONG
        if let Some(pong) = create_pong_response(&line) {
            client.sender.send(pong).await?;
        }

        // Check for DCC SEND messages
        if line.to_uppercase().contains("DCC SEND") {
            // Check if it's a NOTICE (announcement) or PRIVMSG (actual transfer)
            if line.contains(" NOTICE ") {
                info!("📢 DCC SEND announcement (NOTICE)");
            } else if line.contains(" PRIVMSG ") {
                info!("📥 DCC SEND transfer request (PRIVMSG)");
            }
        }

        // Handle DCC SEND requests
        handle_dcc_send_request(&client, &line).await;
    }

    info!("Receive loop ended after {} messages", message_count);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing/logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .init();

    info!("Starting airc IRC client");

    let args = Args::parse();

    // Load config from file, then merge with CLI args
    let config = Config::load()
        .await
        .unwrap_or_else(|e| {
            warn!("Failed to load config: {}, using defaults", e);
            Config::default()
        })
        .merge_with_args(args.server, args.channel, args.username, args.download_path);

    debug!("Using config: {:?}", config);

    let (username, nickname, realname) = if let Some(ref user) = config.username {
        (user.clone(), user.clone(), user.clone())
    } else {
        let mut rng = rand::rng();
        let random_nickname = format!("{}{}", NICKNAME_PREFIX, rng.random_range(0..=MAX_NICKNAME_SUFFIX));
        info!("Generated random nickname: {}", random_nickname);
        (random_nickname.clone(), random_nickname.clone(), DEFAULT_REALNAME.to_string())
    };

    let (client, receiver) =
        IrcClient::new(config, &username, &nickname, &realname).await?;

    info!("Spawning async tasks");
    let init_task = tokio::spawn(init(client.clone()));
    let write_task = tokio::spawn(write(client.clone(), receiver));
    let cli_task = tokio::spawn(cli(client.clone()));
    let receive_task = tokio::spawn(receive_loop(client.clone()));

    let (init_result, write_result, cli_result, receive_result) =
        tokio::join!(init_task, write_task, cli_task, receive_task);

    // Propagate any errors from the tasks
    init_result??;
    write_result??;
    cli_result??;
    receive_result??;

    // Wait for all DCC tasks to complete before exiting
    info!("Main tasks completed, waiting for DCC transfers");
    print_line("Waiting for DCC transfers to complete...\n", true);
    let mut tasks = client.dcc_tasks.lock().await;
    while tasks.join_next().await.is_some() {
        // All tasks joined
    }
    info!("All DCC transfers completed, exiting");
    print_line("All DCC transfers completed.\n", true);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_dcc_ip_address() {
        // Test normal IP conversion
        assert_eq!(decode_dcc_ip_address("2130706433").unwrap(), "127.0.0.1");
        assert_eq!(decode_dcc_ip_address("16777216").unwrap(), "1.0.0.0");
        assert_eq!(decode_dcc_ip_address("3232235777").unwrap(), "192.168.1.1");

        // Test invalid input
        assert!(decode_dcc_ip_address("not_a_number").is_err());
        assert!(decode_dcc_ip_address("").is_err());
    }

    #[test]
    fn test_create_pong_response() {
        // Test PING message
        assert_eq!(
            create_pong_response("PING :server.example.com"),
            Some("PONG :server.example.com".to_string())
        );

        assert_eq!(
            create_pong_response("PING 12345"),
            Some("PONG 12345".to_string())
        );

        // Test non-PING messages
        assert_eq!(create_pong_response("PRIVMSG #test :hello"), None);
        assert_eq!(create_pong_response("PONG :something"), None);
        assert_eq!(create_pong_response(""), None);
    }

    #[test]
    fn test_dcc_send_regex() {
        // Test valid DCC SEND messages (case insensitive)
        let msg1 = ":bot!user@host PRIVMSG nick :DCC SEND file.txt 2130706433 1234 5678";
        assert!(DCC_SEND_RE.is_match(msg1));

        let caps1 = DCC_SEND_RE.captures(msg1).unwrap();
        assert_eq!(caps1.name("filename").unwrap().as_str(), "file.txt");
        assert_eq!(caps1.name("ip").unwrap().as_str(), "2130706433");
        assert_eq!(caps1.name("port").unwrap().as_str(), "1234");
        assert_eq!(caps1.name("size").unwrap().as_str(), "5678");

        // Test case insensitive
        let msg2 = ":bot!user@host PRIVMSG nick :DCC Send file.zip 192 8080 1024";
        assert!(DCC_SEND_RE.is_match(msg2));

        let msg3 = ":bot!user@host PRIVMSG nick :DCC SEND book.epub 3232235777 9999 123456";
        assert!(DCC_SEND_RE.is_match(msg3));

        // Test invalid messages
        assert!(!DCC_SEND_RE.is_match("PRIVMSG #channel :hello"));
        assert!(!DCC_SEND_RE.is_match("DCC SEND"));
    }

    #[test]
    fn test_search_regex() {
        // Test /search and /s commands
        assert!(SEARCH_RE.is_match("/search rust programming"));
        assert!(SEARCH_RE.is_match("/s python"));

        let caps1 = SEARCH_RE.captures("/search rust programming").unwrap();
        assert_eq!(caps1.name("search_term").unwrap().as_str(), "rust programming");

        let caps2 = SEARCH_RE.captures("/s python").unwrap();
        assert_eq!(caps2.name("search_term").unwrap().as_str(), "python");

        // Test edge case - matches but captures empty string
        assert!(SEARCH_RE.is_match("/search "));
        let caps3 = SEARCH_RE.captures("/search ").unwrap();
        assert_eq!(caps3.name("search_term").unwrap().as_str(), "");

        // Test invalid
        assert!(!SEARCH_RE.is_match("/se"));
        assert!(!SEARCH_RE.is_match("/search"));
    }

    #[test]
    fn test_entry_regex() {
        // Test entry number patterns
        assert!(ENTRY_RE.is_match("/123"));
        assert!(ENTRY_RE.is_match("/0"));
        assert!(ENTRY_RE.is_match("/999"));

        let caps = ENTRY_RE.captures("/42").unwrap();
        assert_eq!(caps.name("entry_num").unwrap().as_str(), "42");

        // Test invalid
        assert!(!ENTRY_RE.is_match("/abc"));
        assert!(!ENTRY_RE.is_match("123"));
    }
}
