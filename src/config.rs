use crate::error::{AircError, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(default = "default_server")]
    pub server: String,

    #[serde(default = "default_channel")]
    pub channel: String,

    #[serde(default)]
    pub username: Option<String>,

    #[serde(default = "default_download_path")]
    pub download_path: String,

    #[serde(default = "default_connection_timeout")]
    pub connection_timeout_secs: u64,

    #[serde(default = "default_dcc_timeout")]
    pub dcc_timeout_secs: u64,

    #[serde(default)]
    pub tls: bool,

    #[serde(default)]
    pub port: Option<u16>,
}

const DEFAULT_PORT: u16 = 6667;
const DEFAULT_TLS_PORT: u16 = 6697;

fn default_server() -> String {
    "irc.undernet.org".to_string()
}

fn default_channel() -> String {
    "#bookz".to_string()
}

fn default_download_path() -> String {
    "./downloads/".to_string()
}

fn default_connection_timeout() -> u64 {
    30
}

fn default_dcc_timeout() -> u64 {
    300
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: default_server(),
            channel: default_channel(),
            username: None,
            download_path: default_download_path(),
            connection_timeout_secs: default_connection_timeout(),
            dcc_timeout_secs: default_dcc_timeout(),
            tls: false,
            port: None,
        }
    }
}

impl Config {
    /// Resolve the port to connect on. Falls back to the conventional IRC
    /// ports: 6667 for plaintext, 6697 for TLS.
    pub fn port(&self) -> u16 {
        self.port
            .unwrap_or(if self.tls { DEFAULT_TLS_PORT } else { DEFAULT_PORT })
    }

    /// Load configuration from file, or create default if not found
    pub async fn load() -> Result<Self> {
        let config_path = Self::config_path()?;

        if tokio::fs::try_exists(&config_path).await.unwrap_or(false) {
            let contents = tokio::fs::read_to_string(&config_path).await?;
            let config: Config = toml::from_str(&contents)?;
            tracing::info!("Loaded config from {}", config_path.display());
            Ok(config)
        } else {
            tracing::info!("No config file found, using defaults");
            Ok(Config::default())
        }
    }

    /// Get the config file path
    fn config_path() -> Result<PathBuf> {
        if let Some(proj_dirs) = ProjectDirs::from("com", "airc", "airc") {
            Ok(proj_dirs.config_dir().join("config.toml"))
        } else {
            Err(AircError::Config(
                "Could not determine config directory".to_string(),
            ))
        }
    }

    /// Merge with CLI arguments (CLI takes precedence)
    pub fn merge_with_args(
        mut self,
        server: Option<String>,
        channel: Option<String>,
        username: Option<String>,
        download_path: Option<String>,
        tls: bool,
        port: Option<u16>,
    ) -> Self {
        if let Some(s) = server {
            self.server = s;
        }
        if let Some(c) = channel {
            self.channel = c;
        }
        if let Some(u) = username {
            self.username = Some(u);
        }
        if let Some(d) = download_path {
            self.download_path = d;
        }
        // --tls can enable TLS but absence of the flag never disables a
        // config-file setting.
        if tls {
            self.tls = true;
        }
        if let Some(p) = port {
            self.port = Some(p);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server, "irc.undernet.org");
        assert_eq!(config.channel, "#bookz");
        assert_eq!(config.username, None);
        assert_eq!(config.download_path, "./downloads/");
        assert_eq!(config.connection_timeout_secs, 30);
        assert_eq!(config.dcc_timeout_secs, 300);
        assert!(!config.tls);
        assert_eq!(config.port, None);
    }

    #[test]
    fn test_port_defaults_to_plaintext() {
        let config = Config::default();
        assert_eq!(config.port(), 6667);
    }

    #[test]
    fn test_port_defaults_to_tls_when_enabled() {
        let config = Config {
            tls: true,
            ..Config::default()
        };
        assert_eq!(config.port(), 6697);
    }

    #[test]
    fn test_explicit_port_overrides_tls_default() {
        let config = Config {
            tls: true,
            port: Some(1234),
            ..Config::default()
        };
        assert_eq!(config.port(), 1234);
    }

    #[test]
    fn test_merge_with_args_server() {
        let config = Config::default();
        let merged =
            config.merge_with_args(Some("irc.example.com".to_string()), None, None, None, false, None);
        assert_eq!(merged.server, "irc.example.com");
        assert_eq!(merged.channel, "#bookz");
    }

    #[test]
    fn test_merge_with_args_all() {
        let config = Config::default();
        let merged = config.merge_with_args(
            Some("irc.test.org".to_string()),
            Some("#test".to_string()),
            Some("testuser".to_string()),
            Some("/tmp/downloads".to_string()),
            true,
            Some(6697),
        );
        assert_eq!(merged.server, "irc.test.org");
        assert_eq!(merged.channel, "#test");
        assert_eq!(merged.username, Some("testuser".to_string()));
        assert_eq!(merged.download_path, "/tmp/downloads");
        assert!(merged.tls);
        assert_eq!(merged.port, Some(6697));
    }

    #[test]
    fn test_merge_with_args_partial() {
        let config = Config::default();
        let merged =
            config.merge_with_args(None, Some("#custom".to_string()), None, None, false, None);
        assert_eq!(merged.server, "irc.undernet.org");
        assert_eq!(merged.channel, "#custom");
        assert_eq!(merged.username, None);
        assert!(!merged.tls);
    }
}
