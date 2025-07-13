use std::fs;
use std::io::copy;
use std::path::PathBuf;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::Sender;
use zip::ZipArchive;
use log::{debug, info, error};

use crate::error::{AircError, Result};
use crate::config::Config;

#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub filename: String,
    pub progress: u16,
    pub total_size: u32,
    pub current_size: u32,
    pub status: DownloadStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadStatus {
    Starting,
    InProgress,
    Completed,
    Failed(String),
    Extracting,
}

#[derive(Debug, Clone)]
pub enum DownloadMessage {
    Started(String, u32),
    Progress(String, u32),
    Completed(String),
    Failed(String, String),
    Extracting(String),
    Extracted(String),
}

impl DownloadProgress {
    pub fn new(filename: String, total_size: u32) -> Self {
        Self {
            filename,
            progress: 0,
            total_size,
            current_size: 0,
            status: DownloadStatus::Starting,
        }
    }
    
    pub fn update_progress(&mut self, current_size: u32) {
        self.current_size = current_size;
        if self.total_size > 0 {
            self.progress = ((current_size as f64 / self.total_size as f64) * 100.0) as u16;
        }
        self.status = DownloadStatus::InProgress;
    }
    
    pub fn mark_completed(&mut self) {
        self.status = DownloadStatus::Completed;
        self.progress = 100;
    }
    
    pub fn mark_failed(&mut self, error: String) {
        self.status = DownloadStatus::Failed(error);
    }
    
    pub fn mark_extracting(&mut self) {
        self.status = DownloadStatus::Extracting;
    }
}

#[derive(Clone)]
pub struct Downloader {
    config: Config,
    sender: Sender<DownloadMessage>,
}

impl Downloader {
    pub fn new(config: Config, sender: Sender<DownloadMessage>) -> Self {
        Self { config, sender }
    }
    
    pub async fn download_dcc(&self, filename: &str, ip: &str, port: &str, size: u32) -> Result<()> {
        info!("Starting DCC download: {} from {}:{} (size: {})", filename, ip, port, size);
        
        // Notify start
        self.sender.send(DownloadMessage::Started(filename.to_string(), size)).await
            .map_err(|e| AircError::ChannelError(format!("Failed to send download start: {}", e)))?;
        
        // Create connection with timeout
        let stream = tokio::time::timeout(
            Duration::from_secs(self.config.download_timeout_secs),
            TcpStream::connect(format!("{}:{}", ip, port))
        )
        .await
        .map_err(|_| AircError::DownloadError("Connection timeout".to_string()))?
        .map_err(|e| AircError::DownloadError(format!("Failed to connect: {}", e)))?;
        
        // Ensure download directory exists
        if let Err(e) = fs::create_dir_all(&self.config.download_path) {
            error!("Failed to create download directory: {}", e);
            return Err(AircError::FileOperationFailed(format!("Cannot create download directory: {}", e)));
        }
        
        let filepath = self.config.download_path.join(filename);
        let mut file = File::create(&filepath).await
            .map_err(|e| AircError::FileOperationFailed(format!("Cannot create file: {}", e)))?;
        
        // Download with progress tracking
        let result = self.download_with_progress(stream, &mut file, filename, size).await;
        
        match result {
            Ok(_) => {
                info!("Download completed: {}", filename);
                self.sender.send(DownloadMessage::Completed(filename.to_string())).await
                    .map_err(|e| AircError::ChannelError(format!("Failed to send completion: {}", e)))?;
                
                // Auto-extract if it's a zip file
                if filename.ends_with(".zip") {
                    self.extract_zip(&filepath, filename).await?;
                }
                
                Ok(())
            }
            Err(e) => {
                error!("Download failed for {}: {}", filename, e);
                self.sender.send(DownloadMessage::Failed(filename.to_string(), e.to_string())).await
                    .map_err(|e| AircError::ChannelError(format!("Failed to send failure: {}", e)))?;
                Err(e)
            }
        }
    }
    
    async fn download_with_progress(
        &self,
        mut stream: TcpStream,
        file: &mut File,
        filename: &str,
        expected_size: u32
    ) -> Result<()> {
        let mut buffer = vec![0u8; 8192]; // Increased buffer size
        let mut received = 0u32;
        let mut last_progress_update = std::time::Instant::now();
        
        loop {
            let bytes_read = stream.read(&mut buffer).await
                .map_err(|e| AircError::DownloadError(format!("Read error: {}", e)))?;
            
            if bytes_read == 0 {
                break;
            }
            
            received += bytes_read as u32;
            
            file.write_all(&buffer[0..bytes_read]).await
                .map_err(|e| AircError::FileOperationFailed(format!("Write error: {}", e)))?;
            
            // Send progress updates every 500ms to avoid spam
            if last_progress_update.elapsed() > Duration::from_millis(500) {
                self.sender.send(DownloadMessage::Progress(filename.to_string(), received)).await
                    .map_err(|e| AircError::ChannelError(format!("Failed to send progress: {}", e)))?;
                last_progress_update = std::time::Instant::now();
            }
            
            if received >= expected_size {
                break;
            }
        }
        
        file.flush().await
            .map_err(|e| AircError::FileOperationFailed(format!("Flush error: {}", e)))?;
        
        debug!("Download completed: {} bytes received", received);
        Ok(())
    }
    
    async fn extract_zip(&self, filepath: &PathBuf, filename: &str) -> Result<()> {
        info!("Extracting ZIP file: {}", filename);
        
        self.sender.send(DownloadMessage::Extracting(filename.to_string())).await
            .map_err(|e| AircError::ChannelError(format!("Failed to send extracting: {}", e)))?;
        
        let filepath_clone = filepath.clone();
        let filename_clone = filename.to_string();
        let sender_clone = self.sender.clone();
        
        // Run extraction in blocking thread to avoid blocking async runtime
        tokio::task::spawn_blocking(move || {
            let result = extract_zip_file(&filepath_clone);
            
            let message = match result {
                Ok(_) => {
                    info!("ZIP extraction completed: {}", filename_clone);
                    DownloadMessage::Extracted(filename_clone)
                }
                Err(e) => {
                    error!("ZIP extraction failed for {}: {}", filename_clone, e);
                    DownloadMessage::Failed(filename_clone, format!("Extraction failed: {}", e))
                }
            };
            
            // Send result back to main thread
            if let Err(e) = sender_clone.blocking_send(message) {
                error!("Failed to send extraction result: {}", e);
            }
        });
        
        Ok(())
    }
}

fn extract_zip_file(filepath: &PathBuf) -> Result<()> {
    let file = fs::File::open(filepath)
        .map_err(|e| AircError::FileOperationFailed(format!("Cannot open ZIP file: {}", e)))?;
    
    let mut archive = ZipArchive::new(file)
        .map_err(|e| AircError::FileOperationFailed(format!("Invalid ZIP file: {}", e)))?;
    
    // Extract first file for now (could be enhanced to extract all files)
    if archive.len() > 0 {
        let mut file_in_zip = archive.by_index(0)
            .map_err(|e| AircError::FileOperationFailed(format!("Cannot access ZIP content: {}", e)))?;
        
        let output_path = filepath.with_extension("");
        let mut output_file = fs::File::create(&output_path)
            .map_err(|e| AircError::FileOperationFailed(format!("Cannot create output file: {}", e)))?;
        
        copy(&mut file_in_zip, &mut output_file)
            .map_err(|e| AircError::FileOperationFailed(format!("Cannot extract file: {}", e)))?;
        
        debug!("Extracted ZIP to: {:?}", output_path);
    }
    
    Ok(())
}