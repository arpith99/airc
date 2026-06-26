use crate::config::Config;
use crate::dcc::{
    DCC_SEND_RE, SEARCHBOT_RESULTS_PREFIX, ZIP_EXTENSION, dcc_receive, read_lines_to_vec,
    unzip_file,
};
use crate::error::{AircError, Result};
use crate::net::retry_with_backoff;
use crate::tui::UiEvent;
use std::path::PathBuf;
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
    pub(crate) ui_tx: Sender<UiEvent>,
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
        ui_tx: Sender<UiEvent>,
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
                ui_tx,
                dcc_tasks: Arc::new(tokio::sync::Mutex::new(tokio::task::JoinSet::new())),
            }),
            receiver,
        ))
    }

    // Queue an outgoing IRC line for the write task.
    pub(crate) async fn send(&self, message: String) -> Result<()> {
        self.sender.send(message).await?;
        Ok(())
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
        {
            let mut writer = client.writer.lock().await;
            writer.write_all(message.as_bytes()).await?;
            writer.flush().await?;
        }

        // On QUIT, stop the write loop so shutdown can proceed cleanly.
        if message.len() >= 4 && message[..4].eq_ignore_ascii_case("QUIT") {
            tokio::time::sleep(tokio::time::Duration::from_millis(QUIT_DELAY_MS)).await;
            break;
        }
    }
    Ok(())
}

// Handle received DCC file
async fn handle_dcc_file(client: Arc<IrcClient>, fpath: String) -> Result<()> {
    let path = PathBuf::from(&fpath);

    if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
        if filename.starts_with(SEARCHBOT_RESULTS_PREFIX) && filename.ends_with(ZIP_EXTENSION) {
            let txt_file = unzip_file(&fpath, client.ui_tx.clone()).await?;
            let book_entries = read_lines_to_vec(&txt_file).await?;
            let _ = client.ui_tx.send(UiEvent::BookList(book_entries)).await;
        } else {
            let _ = client
                .ui_tx
                .send(UiEvent::System(format!("Downloaded file: {}", filename)))
                .await;
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

            let client_clone = client.clone();
            let ui_tx = client.ui_tx.clone();
            let download_path = client.config.download_path.clone();
            let connection_timeout = client.config.connection_timeout_secs;
            let transfer_timeout = client.config.dcc_timeout_secs;

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
                    ui_tx.clone(),
                )
                .await
                {
                    Ok(fpath) => {
                        if let Err(e) = handle_dcc_file(client_clone, fpath).await {
                            warn!("Error handling DCC file: {}", e);
                        }
                    }
                    Err(e) => {
                        warn!("Error receiving DCC file: {}", e);
                        let _ = ui_tx
                            .send(UiEvent::DownloadFailed {
                                filename: filename.clone(),
                                error: e.to_string(),
                            })
                            .await;
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

// IRC membership parsing for the user-list pane.
pub(crate) enum Membership {
    Joined(String),
    Left(String),
}

// Parse an RPL_NAMREPLY (353): the trailing " :" segment is the space-separated
// nick list; strip channel-status prefixes.
pub(crate) fn parse_names_reply(line: &str) -> Option<Vec<String>> {
    if !line.contains(" 353 ") {
        return None;
    }
    let names = line.rsplit_once(" :")?.1;
    let users: Vec<String> = names
        .split_whitespace()
        .map(|n| n.trim_start_matches(['@', '+', '%', '&', '~']).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if users.is_empty() {
        None
    } else {
        Some(users)
    }
}

// Parse a JOIN/PART/QUIT line into the nick that joined or left.
pub(crate) fn parse_membership(line: &str) -> Option<Membership> {
    let rest = line.strip_prefix(':')?;
    let mut parts = rest.split_whitespace();
    let source = parts.next()?;
    let command = parts.next()?;
    let nick = source.split('!').next()?.to_string();
    if nick.is_empty() {
        return None;
    }
    match command {
        "JOIN" => Some(Membership::Joined(nick)),
        "PART" | "QUIT" => Some(Membership::Left(nick)),
        _ => None,
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
            let _ = client.ui_tx.send(UiEvent::Disconnected).await;
            break;
        }

        message_count += 1;
        let _ = client.ui_tx.send(UiEvent::Received(line.clone())).await;

        if let Some(users) = parse_names_reply(&line) {
            let _ = client.ui_tx.send(UiEvent::UserList(users)).await;
        } else if let Some(membership) = parse_membership(&line) {
            let event = match membership {
                Membership::Joined(nick) => UiEvent::UserJoined(nick),
                Membership::Left(nick) => UiEvent::UserLeft(nick),
            };
            let _ = client.ui_tx.send(event).await;
        }

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

    #[test]
    fn test_parse_names_reply() {
        let line = ":irc.example.com 353 mynick = #bookz :alice @bob +carol\r\n";
        assert_eq!(
            parse_names_reply(line),
            Some(vec![
                "alice".to_string(),
                "bob".to_string(),
                "carol".to_string()
            ])
        );
        assert_eq!(parse_names_reply(":x PRIVMSG #c :hi"), None);
    }

    #[test]
    fn test_parse_membership_join() {
        let line = ":alice!~a@host JOIN :#bookz\r\n";
        assert!(matches!(parse_membership(line), Some(Membership::Joined(n)) if n == "alice"));
    }

    #[test]
    fn test_parse_membership_part_and_quit() {
        assert!(matches!(
            parse_membership(":bob!~b@host PART #bookz :bye\r\n"),
            Some(Membership::Left(n)) if n == "bob"
        ));
        assert!(matches!(
            parse_membership(":carol!~c@host QUIT :Ping timeout\r\n"),
            Some(Membership::Left(n)) if n == "carol"
        ));
    }

    #[test]
    fn test_parse_membership_ignores_other() {
        assert!(parse_membership(":irc 353 mynick = #c :a b").is_none());
        assert!(parse_membership("PING :server").is_none());
    }
}
