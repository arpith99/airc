use async_std::io::stdin;
use chrono::Local;
use clap::Parser;
use colored::Colorize;
use once_cell::sync::Lazy;
use rand::Rng;
use regex::Regex;
use std::error::Error;
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
use zip::ZipArchive;

// Default download path - can be overridden via CLI
const DEFAULT_DOWNLOAD_PATH: &str = "./downloads/";

// Compile regexes once at startup
static SEARCH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/s(earch)? (?P<search_term>.*)").unwrap());

static ENTRY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"/(?P<entry_num>\d+)").unwrap());

static DCC_SEND_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i).*DCC SEND (?P<filename>\S+) (?P<ip>\d+) (?P<port>\d+) (?P<size>\d+)").unwrap()
});

#[derive(Clone)]
struct IrcClient {
    server: String,
    channel: String,
    username: String,
    nickname: String,
    realname: String,
    reader: Arc<tokio::sync::Mutex<BufReader<OwnedReadHalf>>>,
    writer: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    sender: Sender<String>,
    booklist: Arc<tokio::sync::Mutex<Vec<String>>>,
    download_path: String,
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

impl IrcClient {
    async fn new(
        server: &str,
        channel: &str,
        username: &str,
        nickname: &str,
        realname: &str,
        download_path: &str,
    ) -> Result<(Arc<IrcClient>, Receiver<String>), Box<dyn Error + Send + Sync>> {
        let stream = TcpStream::connect(format!("{}:6667", server)).await?;
        let (sender, receiver) = mpsc::channel(100);
        let (reader, writer) = stream.into_split();
        let reader = BufReader::new(reader);

        // Ensure download directory exists
        tokio::fs::create_dir_all(download_path).await?;

        Ok((
            Arc::new(IrcClient {
                server: server.to_string(),
                channel: channel.to_string(),
                username: username.to_string(),
                nickname: nickname.to_string(),
                realname: realname.to_string(),
                reader: Arc::new(tokio::sync::Mutex::new(reader)),
                writer: Arc::new(tokio::sync::Mutex::new(writer)),
                sender,
                booklist: Arc::new(tokio::sync::Mutex::new(Vec::new())),
                download_path: download_path.to_string(),
                dcc_tasks: Arc::new(tokio::sync::Mutex::new(tokio::task::JoinSet::new())),
            }),
            receiver,
        ))
    }
}

async fn init(client: Arc<IrcClient>) -> Result<(), Box<dyn Error + Send + Sync>> {
    client.sender.send("CAP END\r\n".to_string()).await?;
    client
        .sender
        .send(format!("NICK {}\r\n", client.nickname))
        .await?;
    client
        .sender
        .send(format!(
            "USER {} {} {} :{}\r\n",
            client.username, client.nickname, client.server, client.realname
        ))
        .await?;
    Ok(())
}

async fn write(
    client: Arc<IrcClient>,
    mut receiver: Receiver<String>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    while let Some(message) = receiver.recv().await {
        print_sent_line(&message);

        {
            let mut writer = client.writer.lock().await;
            writer.write_all(message.as_bytes()).await?;
            writer.flush().await?;
        } // Lock released here

        // Check if this is a QUIT command (case-insensitive)
        if message.to_uppercase().starts_with("QUIT") {
            print_line("Exiting...\n", true);
            print_line("Goodbye!\n", true);
            // Give other tasks time to clean up
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            exit(0);
        }
    }
    Ok(())
}

// Local command to search the booklist
async fn handle_search_booklist(client: Arc<IrcClient>, search_term: &str) {
    let booklist = client.booklist.lock().await;
    let search_lower = search_term.to_lowercase();
    let mut found = false;

    for (i, book_line) in booklist.iter().enumerate() {
        if book_line.to_lowercase().contains(&search_lower) {
            print_line(&format!("{}: {}\n", i, book_line), true);
            found = true;
        }
    }

    if !found {
        print_line(&format!("No results found for '{}'\n", search_term), true);
    }
}

// Returns Option<String> - Some(msg) if should send to IRC, None if shouldn't
async fn process_command(client: Arc<IrcClient>, command: &str) -> Option<String> {
    // JOIN command
    if command == "/join" || command == "/j" {
        return Some(format!("JOIN {}\r\n", client.channel));
    }

    // QUIT command
    if command.starts_with("/quit") || command.starts_with("/q ") || command == "/q" {
        // Extract optional quit message
        let quit_msg = if command.starts_with("/quit ") {
            command.strip_prefix("/quit ").unwrap_or("")
        } else if command.starts_with("/q ") {
            command.strip_prefix("/q ").unwrap_or("")
        } else {
            ""
        };

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
        return Some(format!("PRIVMSG {} :@search {}\r\n", client.channel, search_term));
    }

    // ENTRY NUMBER selection
    if let Some(caps) = ENTRY_RE.captures(command) {
        let entry_num: usize = caps
            .name("entry_num")
            .unwrap()
            .as_str()
            .parse()
            .unwrap_or(0);
        print_line(&format!("Requesting entry number: {}\n", entry_num), true);

        // Look up the actual book entry
        let booklist = client.booklist.lock().await;
        if let Some(book_entry) = booklist.get(entry_num) {
            print_line(&format!("Book entry: {}\n", book_entry), true);
            return Some(format!("PRIVMSG {} :{}\r\n", client.channel, book_entry));
        } else {
            print_line(
                &format!("Entry number {} not found in booklist\n", entry_num),
                true,
            );
            return None; // Don't send anything
        }
    }

    // Default: treat as raw IRC command
    Some(format!(
        "{}\r\n",
        command.strip_prefix('/').unwrap_or(command).trim()
    ))
}

async fn cli(client: Arc<IrcClient>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut command = String::new();
    while 0 != stdin().read_line(&mut command).await? {
        let trimmed = command.trim();

        // Handle local commands (don't send to IRC)
        if let Some(search_term) = trimmed.strip_prefix("/ss ") {
            handle_search_booklist(client.clone(), search_term).await;
            command.clear();
            continue;
        }

        // Handle IRC commands (generate messages to send)
        if let Some(message) = process_command(client.clone(), trimmed).await {
            client.sender.send(message).await?;
        }

        command.clear();
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

async fn unzip_file(filename: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
    let filename_owned = filename.to_string();

    // Run blocking zip operations in a separate thread pool
    let result = tokio::task::spawn_blocking(move || -> Result<(String, u64), Box<dyn Error + Send + Sync>> {
        let file = fs::File::open(&filename_owned)
            .map_err(|e| format!("Failed to open zip file '{}': {}", filename_owned, e))?;

        let mut archive = ZipArchive::new(file)
            .map_err(|e| format!("Failed to read zip archive '{}': {}", filename_owned, e))?;

        let mut zipped_file = archive.by_index(0)
            .map_err(|e| format!("Failed to access first file in archive: {}", e))?;

        let file_size = zipped_file.size();

        let outpath = PathBuf::from(
            filename_owned.strip_suffix(".zip")
                .ok_or("Filename doesn't end with .zip")?
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

// Convert DCC IP (32-bit integer) to dotted-quad
fn dcc_ip_to_string(ip_str: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
    let ip_num: u32 = ip_str.parse()?;
    let a = (ip_num >> 24) & 0xFF;
    let b = (ip_num >> 16) & 0xFF;
    let c = (ip_num >> 8) & 0xFF;
    let d = ip_num & 0xFF;
    Ok(format!("{}.{}.{}.{}", a, b, c, d))
}

async fn dcc_receive(
    filename: &str,
    ip: &str,
    port: &str,
    size: &str,
    download_path: &str,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    // Convert IP if it's in DCC format (32-bit integer)
    let ip_addr = if ip.parse::<u32>().is_ok() {
        dcc_ip_to_string(ip)?
    } else {
        ip.to_string()
    };

    let file_size: u64 = size
        .trim()
        .parse()
        .map_err(|e| format!("Invalid file size '{}': {}", size, e))?;
    let port_num: u16 = port
        .trim()
        .parse()
        .map_err(|e| format!("Invalid port '{}': {}", port, e))?;

    print_line(
        &format!("Connecting to {}:{}...\n", ip_addr, port_num),
        true,
    );

    let mut stream = TcpStream::connect(format!("{}:{}", ip_addr, port_num))
        .await
        .map_err(|e| format!("Failed to connect to {}:{}: {}", ip_addr, port_num, e))?;

    let fpath = format!("{}{}", download_path, filename);
    let mut file = tokio::fs::File::create(&fpath)
        .await
        .map_err(|e| format!("Failed to create file '{}': {}", fpath, e))?;

    // Stream the file instead of loading into memory
    let mut total_bytes = 0u64;
    let mut buffer = vec![0u8; 65536]; // 64KB chunks for better performance

    while total_bytes < file_size {
        let to_read = std::cmp::min(buffer.len() as u64, file_size - total_bytes) as usize;
        let bytes_read = stream.read(&mut buffer[..to_read]).await?;

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
    Ok(fpath)
}

async fn read_lines_to_vec(path: &str) -> Result<Vec<String>, Box<dyn Error + Send + Sync>> {
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
async fn handle_dcc_file(client: Arc<IrcClient>, fpath: String) -> Result<(), Box<dyn Error + Send + Sync>> {
    let path = PathBuf::from(&fpath);

    if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
        if filename.starts_with("SearchBot_results") && filename.ends_with(".zip") {
            let txt_file = unzip_file(&fpath).await?;
            let lines_txt_file = read_lines_to_vec(&txt_file).await?;

            // Move instead of clone
            let line_count = lines_txt_file.len();
            *client.booklist.lock().await = lines_txt_file;

            // Re-lock to display (avoid holding lock during iteration)
            let booklist = client.booklist.lock().await;
            for (i, book_line) in booklist.iter().enumerate() {
                print_line(&format!("{}: {}\n", i, book_line), true);
            }
            print_line(&format!("Loaded {} books into booklist\n", line_count), true);
        } else {
            // Download other files like ebooks
            print_line(&format!("Downloaded file: {}\n", filename), true);
        }
    }

    Ok(())
}

// Handle PING message properly
fn handle_ping(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("PING ") {
        Some(format!("PONG {}", rest))
    } else {
        None
    }
}

async fn read(client: Arc<IrcClient>) -> Result<(), Box<dyn Error + Send + Sync>> {
    loop {
        let mut line = String::new();
        let bytes_read = {
            let mut reader = client.reader.lock().await;
            reader.read_line(&mut line).await?
        };

        if bytes_read == 0 {
            break;
        }

        print_received_line(&line);

        // Handle PING properly
        if let Some(pong) = handle_ping(&line) {
            client.sender.send(pong).await?;
        }

        // Handle DCC SEND with error recovery (case-insensitive)
        if line.to_uppercase().contains("DCC SEND") {
            // Parse the DCC SEND parameters immediately without awaiting
            if let Some(caps) = DCC_SEND_RE.captures(&line) {
                // Extract captures safely - regex guarantees these exist if it matched
                let filename = caps.name("filename").map(|m| m.as_str().to_string());
                let ip = caps.name("ip").map(|m| m.as_str().to_string());
                let port = caps.name("port").map(|m| m.as_str().to_string());
                let size = caps.name("size").map(|m| m.as_str().to_string());

                if let (Some(filename), Some(ip), Some(port), Some(size)) = (filename, ip, port, size) {
                    print_line(
                        &format!("Received DCC SEND request for file: {}\n", filename),
                        true,
                    );
                    print_line(
                        &format!("IP: {}, Port: {}, Size: {} bytes\n", ip, port, size),
                        true,
                    );

                    // Spawn the ENTIRE download+processing in a separate task
                    // This prevents blocking the read loop during file transfer
                    let client_clone = client.clone();
                    let download_path = client.download_path.clone();

                    // Track the spawned task for graceful shutdown
                    client.dcc_tasks.lock().await.spawn(async move {
                        match dcc_receive(&filename, &ip, &port, &size, &download_path).await {
                            Ok(fpath) => {
                                if let Err(e) = handle_dcc_file(client_clone, fpath).await {
                                    print_line(&format!("Error handling DCC file: {}\n", e), true);
                                }
                            }
                            Err(e) => {
                                print_line(&format!("Error receiving DCC file: {}\n", e), true);
                            }
                        }
                    });
                }
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let args = Args::parse();

    // Use provided values or defaults
    let server = args.server.unwrap_or_else(|| "irc.undernet.org".to_string());
    let channel = args.channel.unwrap_or_else(|| "#bookz".to_string());
    let download_path = args.download_path.unwrap_or_else(|| DEFAULT_DOWNLOAD_PATH.to_string());

    let (username, nickname, realname) = if let Some(user) = args.username {
        (user.clone(), user.clone(), user)
    } else {
        let mut rng = rand::rng();
        let random_nickname = format!("bworm{}", rng.random_range(0..=99999));
        (random_nickname.clone(), random_nickname.clone(), "Book Worm".to_string())
    };

    let (client, receiver) =
        IrcClient::new(&server, &channel, &username, &nickname, &realname, &download_path).await?;

    let init_task = tokio::spawn(init(client.clone()));
    let write_task = tokio::spawn(write(client.clone(), receiver));
    let cli_task = tokio::spawn(cli(client.clone()));
    let read_task = tokio::spawn(read(client.clone()));

    let (init_result, write_result, cli_result, read_result) =
        tokio::join!(init_task, write_task, cli_task, read_task);

    // Propagate any errors from the tasks
    init_result??;
    write_result??;
    cli_result??;
    read_result??;

    // Wait for all DCC tasks to complete before exiting
    print_line("Waiting for DCC transfers to complete...\n", true);
    let mut tasks = client.dcc_tasks.lock().await;
    while tasks.join_next().await.is_some() {
        // All tasks joined
    }
    print_line("All DCC transfers completed.\n", true);

    Ok(())
}
