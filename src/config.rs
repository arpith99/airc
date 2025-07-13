use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::error::{AircError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: String,
    pub port: u16,
    pub username: String,
    pub nickname: String,
    pub realname: String,
    pub channel: String,
    pub download_path: PathBuf,
    pub max_message_history: usize,
    pub connection_timeout_secs: u64,
    pub download_timeout_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: "irc.undernet.org".to_string(),
            port: 6667,
            username: generate_random_username(),
            nickname: generate_random_username(),
            realname: "Book Worm".to_string(),
            channel: "#bookz".to_string(),
            download_path: get_default_download_path(),
            max_message_history: 100,
            connection_timeout_secs: 30,
            download_timeout_secs: 300,
        }
    }
}

impl Config {
    pub fn load_or_default() -> Result<Self> {
        let config_path = get_config_path();
        
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .map_err(|e| AircError::ConfigError(format!("Failed to read config file: {}", e)))?;
            
            serde_json::from_str(&content)
                .map_err(|e| AircError::ConfigError(format!("Failed to parse config file: {}", e)))
        } else {
            let config = Self::default();
            config.save()?;
            Ok(config)
        }
    }
    
    pub fn save(&self) -> Result<()> {
        let config_path = get_config_path();
        
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AircError::ConfigError(format!("Failed to create config directory: {}", e)))?;
        }
        
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| AircError::ConfigError(format!("Failed to serialize config: {}", e)))?;
        
        std::fs::write(&config_path, content)
            .map_err(|e| AircError::ConfigError(format!("Failed to write config file: {}", e)))?;
        
        Ok(())
    }
    
    pub fn with_args(mut self, server: Option<String>, channel: Option<String>, username: Option<String>) -> Self {
        if let Some(server) = server {
            self.server = server;
        }
        if let Some(channel) = channel {
            self.channel = channel;
        }
        if let Some(username) = username {
            self.username = username.clone();
            self.nickname = username;
        }
        self
    }
}

fn generate_random_username() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let random_num = rng.random_range(0..=99999);
    format!("bworm{}", random_num)
}

fn get_default_download_path() -> PathBuf {
    dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("Books")
}

fn get_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("airc")
        .join("config.json")
}