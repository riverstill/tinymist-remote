//! Configuration of the service manager.
//!
//! The configuration is stored at `%APPDATA%\tinymist\manager.toml` and edited
//! by the graphical interface.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The entry of a supervised language server service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceEntry {
    /// The unique name of the service, also used as the log file stem.
    pub name: String,
    /// The transport protocol the language server listens with.
    #[serde(rename = "transport")]
    pub transport: ServiceTransport,
    /// The port the language server listens on.
    pub port: u16,
    /// The token required to authenticate remote clients. Empty means no
    /// authentication.
    #[serde(default)]
    pub token: String,
    /// Additional directories that are recursively searched for fonts.
    #[serde(default)]
    pub font_paths: Vec<String>,
    /// Whether to start the service when the manager starts.
    #[serde(default = "default_true")]
    pub auto_start: bool,
}

/// The transport protocol of a service.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceTransport {
    /// The WebSocket transport (`tinymist lsp --ws`).
    Ws,
    /// The raw TCP transport (`tinymist lsp --tcp`).
    Tcp,
}

/// The whole configuration of the service manager.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManagerConfig {
    /// The supervised services.
    #[serde(default, rename = "service")]
    pub services: Vec<ServiceEntry>,
}

fn default_true() -> bool {
    true
}

impl ServiceTransport {
    /// The command line flag selecting the transport.
    pub(crate) fn flag(self) -> &'static str {
        match self {
            ServiceTransport::Ws => "--ws",
            ServiceTransport::Tcp => "--tcp",
        }
    }

    /// The human readable name of the transport.
    pub(crate) fn display(self) -> &'static str {
        match self {
            ServiceTransport::Ws => "ws",
            ServiceTransport::Tcp => "tcp",
        }
    }
}

/// Returns the directory storing the manager configuration and logs.
pub fn manager_dir() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(PathBuf::from(appdata).join("tinymist"))
}

/// Returns the path of the manager configuration file.
pub fn config_path() -> Option<PathBuf> {
    manager_dir().map(|dir| dir.join("manager.toml"))
}

/// Returns the directory storing the per-service logs.
pub fn log_dir() -> Option<PathBuf> {
    manager_dir().map(|dir| dir.join("logs"))
}

impl ManagerConfig {
    /// Loads the configuration, creating a default one if it does not exist.
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default_with_entry();
        };
        match fs::read_to_string(&path) {
            Ok(content) => toml::from_str(&content).unwrap_or_else(|err| {
                log::error!("failed to parse {}: {err}", path.display());
                Self::default_with_entry()
            }),
            Err(_) => {
                let config = Self::default_with_entry();
                config.save();
                config
            }
        }
    }

    /// The configuration seeded with a default service entry.
    fn default_with_entry() -> Self {
        Self {
            services: vec![ServiceEntry {
                name: "main".to_string(),
                transport: ServiceTransport::Ws,
                port: 23626,
                token: "123456".to_string(),
                font_paths: Vec::new(),
                auto_start: true,
            }],
        }
    }

    /// Persists the configuration.
    pub fn save(&self) {
        let Some(path) = config_path() else {
            return;
        };
        let content = match toml::to_string_pretty(self) {
            Ok(content) => content,
            Err(err) => {
                log::error!("failed to serialize the manager configuration: {err}");
                return;
            }
        };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Err(err) = fs::write(&path, content) {
            log::error!("failed to write {}: {err}", path.display());
        }
    }
}

impl ServiceEntry {
    /// The `host:port` address the service listens on.
    pub fn addr(&self) -> String {
        format!("0.0.0.0:{}", self.port)
    }

    /// Builds the arguments passed to the `tinymist lsp` command.
    pub fn server_args(&self) -> Vec<String> {
        let mut args = vec!["lsp".to_string(), self.transport.flag().to_string()];
        args.push(self.addr());
        if !self.token.is_empty() {
            args.push("--auth-token".to_string());
            args.push(self.token.clone());
        }
        for path in &self.font_paths {
            if path.trim().is_empty() {
                continue;
            }
            args.push("--font-path".to_string());
            args.push(path.clone());
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_server_args() {
        let entry = ServiceEntry {
            name: "main".to_string(),
            transport: ServiceTransport::Ws,
            port: 23626,
            token: "123456".to_string(),
            font_paths: vec!["C:\\fonts".to_string(), "  ".to_string()],
            auto_start: true,
        };

        assert_eq!(
            entry.server_args(),
            vec![
                "lsp",
                "--ws",
                "0.0.0.0:23626",
                "--auth-token",
                "123456",
                "--font-path",
                "C:\\fonts",
            ]
        );
    }

    #[test]
    fn round_trips_the_config() {
        let config = ManagerConfig::default_with_entry();
        let content = toml::to_string_pretty(&config).unwrap();
        let parsed: ManagerConfig = toml::from_str(&content).unwrap();

        assert_eq!(parsed.services, config.services);
        assert!(parsed.services[0].auto_start);
    }
}
