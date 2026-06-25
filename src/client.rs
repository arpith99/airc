use crate::commands::{handle_search_results, process_command};
use crate::config::Config;
use crate::dcc::{
    DCC_SEND_RE, SEARCHBOT_RESULTS_PREFIX, ZIP_EXTENSION, dcc_receive, read_lines_to_vec,
    unzip_file,
};
use crate::error::{AircError, Result};
use crate::net::retry_with_backoff;
use crate::ui::{print_line, print_received_line, print_sent_line};
use std::path::PathBuf;
use std::process::exit;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::{debug, info, warn};

const CHANNEL_BUFFER_SIZE: usize = 100;
const QUIT_DELAY_MS: u64 = 100;

// The control connection may be plaintext TCP or a TLS stream, so the read and
// write halves are stored as trait objects rather than concrete socket types.
type BoxedReader = Box<dyn AsyncRead + Unpin + Send>;
type BoxedWriter = Box<dyn AsyncWrite + Unpin + Send>;

#[derive(Clone)]
pub(crate) struct IrcClient {
    pub(crate) config: Config,
    username: String,
    nickname: String,
    realname: String,
    reader: Arc<tokio::sync::Mutex<BufReader<BoxedReader>>>,
    writer: Arc<tokio::sync::Mutex<BoxedWriter>>,
    sender: Sender<String>,
    pub(crate) search_results: Arc<tokio::sync::Mutex<Vec<String>>>,
    pub(crate) dcc_tasks: Arc<tokio::sync::Mutex<tokio::task::JoinSet<()>>>,
}

// Perform a TLS handshake over an established TCP connection, verifying the
// server certificate against the bundled Mozilla root store.
async fn tls_connect(
    server: &str,
    tcp: TcpStream,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    use tokio_rustls::TlsConnector;
    use tokio_rustls::rustls::pki_types::ServerName;
    use tokio_rustls::rustls::{ClientConfig, RootCertStore};

    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let tls_config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let connector = TlsConnector::from(Arc::new(tls_config));
    let domain = ServerName::try_from(server.to_string())
        .map_err(|e| AircError::Connection(format!("Invalid server name '{}': {}", server, e)))?;

    connector
        .connect(domain, tcp)
        .await
        .map_err(|e| AircError::Connection(format!("TLS handshake with {} failed: {}", server, e)))
}

impl IrcClient {
    pub(crate) async fn new(
        config: Config,
        username: &str,
        nickname: &str,
        realname: &str,
    ) -> Result<(Arc<IrcClient>, Receiver<String>)> {
        info!("Connecting to {} ({})", config.server, config.channel);

        // Connect with timeout and retry logic
        let server_addr = format!("{}:{}", config.server, config.port());
        let timeout_secs = config.connection_timeout_secs;
        let server_name = config.server.clone();

        let stream = retry_with_backoff(
            || async {
                tokio::time::timeout(
                    tokio::time::Duration::from_secs(timeout_secs),
                    TcpStream::connect(&server_addr),
                )
                .await
                .map_err(|_| {
                    AircError::Timeout(format!(
                        "Connection to {} timed out after {} seconds",
                        server_name, timeout_secs
                    ))
                })?
                .map_err(|e| {
                    AircError::Connection(format!("Failed to connect to {}: {}", server_name, e))
                })
            },
            &format!("IRC connection to {}", config.server),
        )
        .await?;

        let (sender, receiver) = mpsc::channel(CHANNEL_BUFFER_SIZE);

        // Optionally upgrade to TLS, then erase the concrete stream type so the
        // rest of the client treats plaintext and TLS connections identically.
        let (reader, writer): (BoxedReader, BoxedWriter) = if config.tls {
            info!("Establishing TLS connection to {}", config.server);
            let tls = tls_connect(&config.server, stream).await?;
            let (r, w) = tokio::io::split(tls);
            (Box::new(r), Box::new(w))
        } else {
            let (r, w) = tokio::io::split(stream);
            (Box::new(r), Box::new(w))
        };
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

pub(crate) async fn init(client: Arc<IrcClient>) -> Result<()> {
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

pub(crate) async fn write(client: Arc<IrcClient>, mut receiver: Receiver<String>) -> Result<()> {
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
            // Give the server a moment to process the QUIT message.
            tokio::time::sleep(tokio::time::Duration::from_millis(QUIT_DELAY_MS)).await;
            // Wait for any in-flight DCC downloads to finish before exiting so a
            // /quit doesn't truncate files still being written to disk.
            print_line("Waiting for DCC transfers to complete...\n", true);
            drain_dcc_tasks(&client.dcc_tasks).await;
            print_line("Goodbye!\n", true);
            exit(0);
        }
    }

    Ok(())
}

pub(crate) async fn cli(client: Arc<IrcClient>) -> Result<()> {
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
            print_line(
                &format!("Loaded {} books into search results\n", num_results),
                true,
            );
        } else {
            // Download other files like ebooks
            print_line(&format!("Downloaded file: {}\n", filename), true);
        }
    }

    Ok(())
}

// Generate PONG response for PING messages
fn create_pong_response(line: &str) -> Option<String> {
    line.strip_prefix("PING ")
        .map(|rest| format!("PONG {}", rest))
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
                match dcc_receive(
                    &filename,
                    &ip,
                    &port,
                    &size,
                    &download_path,
                    connection_timeout,
                    transfer_timeout,
                )
                .await
                {
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

pub(crate) async fn receive_loop(client: Arc<IrcClient>) -> Result<()> {
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

// Wait for all in-flight DCC download tasks to finish.
pub(crate) async fn drain_dcc_tasks(tasks: &Arc<tokio::sync::Mutex<tokio::task::JoinSet<()>>>) {
    let mut tasks = tasks.lock().await;
    while tasks.join_next().await.is_some() {
        // Each task joined.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn test_drain_dcc_tasks_waits_for_all() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let counter = Arc::new(AtomicU32::new(0));
        let tasks = Arc::new(tokio::sync::Mutex::new(tokio::task::JoinSet::new()));

        {
            let mut set = tasks.lock().await;
            for _ in 0..5 {
                let c = counter.clone();
                set.spawn(async move {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    c.fetch_add(1, Ordering::SeqCst);
                });
            }
        }

        drain_dcc_tasks(&tasks).await;

        // Every spawned task must have run to completion before drain returned.
        assert_eq!(counter.load(Ordering::SeqCst), 5);
    }
}
