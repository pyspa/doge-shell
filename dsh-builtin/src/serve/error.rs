use thiserror::Error;

/// Comprehensive error types for the serve command
/// Covers all possible error conditions with user-friendly messages
#[derive(Debug, Error)]
pub enum ServeError {
    #[error("Port {port} is already in use")]
    PortInUse { port: u16 },

    #[error("Invalid port number: {port}. Port must be between 1 and 65535")]
    InvalidPort { port: u32 },

    #[error("Directory not found: {path}")]
    DirectoryNotFound { path: String },

    #[error("Permission denied: {path}")]
    PermissionDenied { path: String },

    #[error("Path is not a directory: {path}")]
    NotADirectory { path: String },

    #[error("Failed to bind to address {addr}: {source}")]
    BindError {
        addr: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Server error: {0}")]
    ServerError(#[from] Box<dyn std::error::Error + Send + Sync>),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Argument parsing error: {0}")]
    ArgumentError(String),

    #[error("Configuration validation error: {0}")]
    ConfigError(String),

    #[error("Browser launch failed: {0}")]
    BrowserError(String),

    #[error("Network error: {0}")]
    NetworkError(String),
}

impl ServeError {
    /// Get user-friendly error message with suggestions for common issues
    pub fn user_message(&self) -> String {
        match self {
            ServeError::PortInUse { port } => {
                format!(
                    "Port {port} is already in use.\nTry using a different port with -p option.\nSuggested alternatives: -p 8001, -p 8080, -p 3000"
                )
            }
            ServeError::InvalidPort { port } => {
                format!(
                    "Invalid port number: {port}.\nPort must be between 1 and 65535.\nExample: serve -p 8080"
                )
            }
            ServeError::DirectoryNotFound { path } => {
                format!(
                    "Directory not found: {path}\nMake sure the directory exists and you have permission to access it."
                )
            }
            ServeError::PermissionDenied { path } => {
                format!(
                    "Permission denied: {path}\nMake sure you have read permission for this directory.\nTry: chmod +r \"{path}\""
                )
            }
            ServeError::NotADirectory { path } => {
                format!(
                    "Path is not a directory: {path}\nPlease specify a directory path, not a file."
                )
            }
            ServeError::BindError { addr, source } => {
                format!(
                    "Failed to bind to address {addr}: {source}\nThe address might be in use or you might not have permission to bind to it."
                )
            }
            _ => self.to_string(),
        }
    }

    /// Suggest alternative ports when port is in use
    pub fn suggest_alternative_ports(port: u16) -> Vec<u16> {
        let alternatives = [8001, 8080, 3000, 4000, 5000, 9000];
        alternatives
            .iter()
            .filter(|&&p| p != port)
            .copied()
            .collect()
    }
}
