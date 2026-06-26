use crate::error::{AircError, Result};
use crate::net::retry_with_backoff;
use crate::tui::UiEvent;
use once_cell::sync::Lazy;
use regex::Regex;
use std::fs;
use std::io::copy;
use std::path::PathBuf;
use std::time::Duration;
use tokio::fs::File;
use tokio::sync::mpsc::Sender;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use zip::ZipArchive;

const DCC_CHUNK_SIZE: usize = 65536; // 64KB
const MAX_FILE_SIZE_BYTES: u64 = 10 * 1024 * 1024 * 1024; // 10GB max file size
const MAX_FILENAME_LENGTH: usize = 255;

pub(crate) const SEARCHBOT_RESULTS_PREFIX: &str = "SearchBot_results";
pub(crate) const ZIP_EXTENSION: &str = ".zip";

pub(crate) static DCC_SEND_RE: Lazy<Regex> = Lazy::new(|| {
    // Filename may be bare (\S+) or double-quoted when it contains spaces.
    Regex::new(
        r#"(?i).*DCC SEND (?:"(?P<qfilename>[^"]+)"|(?P<filename>\S+)) (?P<ip>\d+) (?P<port>\d+) (?P<size>\d+)"#,
    )
    .unwrap()
});

pub(crate) async fn unzip_file(filename: &str, ui_tx: Sender<UiEvent>) -> Result<String> {
    let base = std::path::Path::new(filename)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(filename)
        .to_string();
    let _ = ui_tx
        .send(UiEvent::DownloadExtracting(base.clone()))
        .await;

    let filename_owned = filename.to_string();

    // Run blocking zip operations in a separate thread pool
    let result = tokio::task::spawn_blocking(move || -> Result<(String, u64)> {
        let file = fs::File::open(&filename_owned)
            .map_err(|e| format!("Failed to open zip file '{}': {}", filename_owned, e))?;

        let mut archive = ZipArchive::new(file)
            .map_err(|e| format!("Failed to read zip archive '{}': {}", filename_owned, e))?;

        let mut zipped_file = archive
            .by_index(0)
            .map_err(|e| format!("Failed to access first file in archive: {}", e))?;

        let file_size = zipped_file.size();

        let outpath = PathBuf::from(
            filename_owned
                .strip_suffix(ZIP_EXTENSION)
                .ok_or(format!("Filename doesn't end with {}", ZIP_EXTENSION))?,
        );

        let mut outfile = fs::File::create(&outpath)
            .map_err(|e| format!("Failed to create output file '{}': {}", outpath.display(), e))?;

        copy(&mut zipped_file, &mut outfile)
            .map_err(|e| format!("Failed to extract file: {}", e))?;

        let outpath_str = outpath
            .to_str()
            .ok_or("Output path contains invalid UTF-8")?
            .to_string();

        Ok((outpath_str, file_size))
    })
    .await?;

    let (outpath_str, _file_size) = result?;

    let _ = ui_tx.send(UiEvent::DownloadExtracted(base)).await;
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
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

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
    let canonical_base = base_path.canonicalize().unwrap_or_else(|_| base_path.clone());

    // For new files, we can't canonicalize yet, so we check the parent
    if let Some(parent) = file_path.parent() {
        let canonical_parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());

        if !canonical_parent.starts_with(&canonical_base) {
            return Err(format!(
                "Path traversal detected: {} escapes {}",
                filename, download_path
            )
            .into());
        }
    }

    Ok(file_path)
}

pub(crate) async fn dcc_receive(
    filename: &str,
    ip: &str,
    port: &str,
    size: &str,
    download_path: &str,
    connection_timeout: u64,
    transfer_timeout: u64,
    ui_tx: Sender<UiEvent>,
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
        )
        .into());
    }

    let _ = ui_tx
        .send(UiEvent::DownloadStarted {
            filename: filename.to_string(),
            size: file_size,
        })
        .await;

    let port_num: u16 = port
        .trim()
        .parse()
        .map_err(|e| format!("Invalid port '{}': {}", port, e))?;

    // Connect with timeout and retry logic
    let dcc_addr = format!("{}:{}", ip_addr, port_num);
    let mut stream = retry_with_backoff(
        || async {
            tokio::time::timeout(
                tokio::time::Duration::from_secs(connection_timeout),
                TcpStream::connect(&dcc_addr),
            )
            .await
            .map_err(|_| {
                AircError::Timeout(format!(
                    "DCC connection to {} timed out after {} seconds",
                    dcc_addr, connection_timeout
                ))
            })?
            .map_err(|e| AircError::Connection(format!("Failed to connect to {}: {}", dcc_addr, e)))
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

    let mut last_emit = std::time::Instant::now();
    let mut last_percent = 0u16;

    while total_bytes < file_size {
        let to_read = std::cmp::min(buffer.len() as u64, file_size - total_bytes) as usize;

        // Read with timeout to detect stalled transfers
        let bytes_read = tokio::time::timeout(
            tokio::time::Duration::from_secs(transfer_timeout),
            stream.read(&mut buffer[..to_read]),
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

        let percent = if file_size > 0 {
            ((total_bytes as f64 / file_size as f64) * 100.0) as u16
        } else {
            0
        };
        if percent != last_percent || last_emit.elapsed() >= Duration::from_millis(200) {
            let _ = ui_tx
                .send(UiEvent::DownloadProgress {
                    filename: filename.to_string(),
                    received: total_bytes,
                })
                .await;
            last_percent = percent;
            last_emit = std::time::Instant::now();
        }

        // Send DCC ACK (total bytes received in network byte order)
        // DCC protocol uses u32 which wraps around for files >4GB
        let ack_bytes = (total_bytes as u32).to_be_bytes();
        stream.write_all(&ack_bytes).await?;
    }

    file.flush().await?;
    stream.flush().await?;

    // Gracefully close the connection
    stream.shutdown().await?;

    let _ = ui_tx
        .send(UiEvent::DownloadCompleted(filename.to_string()))
        .await;
    Ok(file_path.to_string_lossy().to_string())
}

pub(crate) async fn read_lines_to_vec(path: &str) -> Result<Vec<String>> {
    let file = File::open(path).await?;
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut line_stream = reader.lines();
    while let Some(line) = line_stream.next_line().await? {
        lines.push(line);
    }
    Ok(lines)
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

        // Test real-world message with underscores and commas in filename
        let msg4 = ":Bsk!~abc@177.243.138.6 PRIVMSG bworm9084 :DCC SEND Exceptional_C++_-_47_Engineering_Puzzles,_Programming_Problems,_and_Solutions.pdf 2985527814 10052 11717617";
        assert!(DCC_SEND_RE.is_match(msg4), "Real-world DCC SEND message should match");

        let caps4 = DCC_SEND_RE.captures(msg4).unwrap();
        assert_eq!(caps4.name("filename").unwrap().as_str(), "Exceptional_C++_-_47_Engineering_Puzzles,_Programming_Problems,_and_Solutions.pdf");
        assert_eq!(caps4.name("ip").unwrap().as_str(), "2985527814");
        assert_eq!(caps4.name("port").unwrap().as_str(), "10052");
        assert_eq!(caps4.name("size").unwrap().as_str(), "11717617");

        // Test invalid messages
        assert!(!DCC_SEND_RE.is_match("PRIVMSG #channel :hello"));
        assert!(!DCC_SEND_RE.is_match("DCC SEND"));

        // Test NOTICE messages (announcements) should NOT match - missing port/size
        let notice_msg = ":Bsk!~abc@177.243.138.6 NOTICE bworm9084 :DCC Send Exceptional C++ - 47 Engineering Puzzles, Programming Problems, and Solutions.pdf (177.243.138.6)";
        assert!(
            !DCC_SEND_RE.is_match(notice_msg),
            "NOTICE message should NOT match - it has filename with spaces and IP in parentheses, but no port/size"
        );

        // Verify captures returns None for NOTICE
        assert!(
            DCC_SEND_RE.captures(notice_msg).is_none(),
            "NOTICE message should have no captures"
        );
    }

    #[test]
    fn test_notice_vs_privmsg_dcc() {
        // This test specifically checks that we don't confuse announcement with actual transfer
        let notice = ":Bsk!~abc@177.243.138.6 NOTICE bworm9084 :DCC Send Exceptional C++ - 47 Engineering Puzzles, Programming Problems, and Solutions.pdf (177.243.138.6)";
        let privmsg = ":Bsk!~abc@177.243.138.6 PRIVMSG bworm9084 :DCC SEND Exceptional_C++_-_47_Engineering_Puzzles,_Programming_Problems,_and_Solutions.pdf 2985527814 10052 11717617";

        // NOTICE should not match
        assert!(!DCC_SEND_RE.is_match(notice), "NOTICE is just an announcement");

        // PRIVMSG should match
        assert!(
            DCC_SEND_RE.is_match(privmsg),
            "PRIVMSG has actual transfer details"
        );

        // Both contain "DCC SEND" text
        assert!(notice.to_uppercase().contains("DCC SEND"));
        assert!(privmsg.to_uppercase().contains("DCC SEND"));

        // But only PRIVMSG matches the pattern
        assert!(DCC_SEND_RE.captures(notice).is_none());
        assert!(DCC_SEND_RE.captures(privmsg).is_some());
    }

    #[test]
    fn test_dcc_with_ctcp_delimiters() {
        // Test DCC SEND with CTCP delimiters (\x01)
        // This is the REAL format sent by IRC servers
        let with_ctcp = ":SearchOok!ook@OokMP3.users.undernet.org PRIVMSG bworm55417 :\x01DCC SEND SearchBot_results_for__rust_program.txt.zip 1544743952 2044 807\x01\r\n";

        println!("Testing CTCP-wrapped DCC SEND: {:?}", with_ctcp);
        println!(
            "Line contains 'DCC SEND': {}",
            with_ctcp.to_uppercase().contains("DCC SEND")
        );
        println!("Regex matches: {}", DCC_SEND_RE.is_match(with_ctcp));

        if let Some(caps) = DCC_SEND_RE.captures(with_ctcp) {
            println!(
                "Captured filename: {:?}",
                caps.name("filename").map(|m| m.as_str())
            );
            println!("Captured ip: {:?}", caps.name("ip").map(|m| m.as_str()));
            println!("Captured port: {:?}", caps.name("port").map(|m| m.as_str()));
            println!("Captured size: {:?}", caps.name("size").map(|m| m.as_str()));
        } else {
            println!("NO CAPTURES!");
        }

        assert!(
            DCC_SEND_RE.is_match(with_ctcp),
            "Should match DCC SEND with CTCP delimiters"
        );

        let caps = DCC_SEND_RE.captures(with_ctcp).unwrap();
        assert_eq!(
            caps.name("filename").unwrap().as_str(),
            "SearchBot_results_for__rust_program.txt.zip"
        );
        assert_eq!(caps.name("ip").unwrap().as_str(), "1544743952");
        assert_eq!(caps.name("port").unwrap().as_str(), "2044");
        assert_eq!(caps.name("size").unwrap().as_str(), "807");
    }

    #[test]
    fn test_dcc_send_quoted_filename_with_spaces() {
        // Some bots quote filenames that contain spaces. The unquoted \S+ branch
        // stops at the first space, so a quoted alternative is required.
        let line = ":Oatmeal!Oatmeal@50.25.28.166 PRIVMSG bworm23183 :DCC SEND \"Brad Thor - (Scot Harvath 01) - The Lions of Lucerne.epub\" 840506534 3005 1272554";
        let caps = DCC_SEND_RE
            .captures(line)
            .expect("quoted DCC SEND with spaces should match");
        let filename = caps
            .name("qfilename")
            .or_else(|| caps.name("filename"))
            .unwrap()
            .as_str();
        assert_eq!(
            filename,
            "Brad Thor - (Scot Harvath 01) - The Lions of Lucerne.epub"
        );
        assert_eq!(caps.name("ip").unwrap().as_str(), "840506534");
        assert_eq!(caps.name("port").unwrap().as_str(), "3005");
        assert_eq!(caps.name("size").unwrap().as_str(), "1272554");
    }

    #[test]
    fn test_dcc_send_quoted_filename_unicode() {
        // Non-ASCII characters inside a (quoted) filename must be captured intact.
        let line = ":bot!u@h PRIVMSG me :DCC SEND \"Café Société.epub\" 2130706433 1234 5678";
        let caps = DCC_SEND_RE
            .captures(line)
            .expect("unicode quoted DCC SEND should match");
        let filename = caps
            .name("qfilename")
            .or_else(|| caps.name("filename"))
            .unwrap()
            .as_str();
        assert_eq!(filename, "Café Société.epub");
        assert_eq!(caps.name("size").unwrap().as_str(), "5678");
    }
}
