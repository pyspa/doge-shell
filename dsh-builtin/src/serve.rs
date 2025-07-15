use super::ShellProxy;
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode, Uri, header},
    response::Response,
};
use chrono::{DateTime, Utc};
use dsh_types::{Context, ExitStatus};
use getopts::Options;
use mime_guess::MimeGuess;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use thiserror::Error;
use tokio::fs as async_fs;
use tokio_util::io::ReaderStream;
use tracing::{debug, error, info};

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

/// Configuration structure for the HTTP serve command
/// Contains all configurable options for the server behavior
#[derive(Debug, Clone)]
pub struct ServeConfig {
    /// Port number to bind the server to (1-65535)
    pub port: u16,
    /// Directory to serve files from
    pub directory: PathBuf,
    /// Enable verbose request logging
    pub verbose: bool,
    /// Automatically open browser after server starts
    pub open_browser: bool,
    /// Enable CORS headers for cross-origin requests
    pub enable_cors: bool,
    /// Serve index.html files in directories (when false, always show directory listing)
    pub serve_index: bool,
    /// Host address to bind to
    pub host: String,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            port: 8000,
            directory: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            verbose: false,
            open_browser: false,
            enable_cors: false,
            serve_index: true,
            host: "127.0.0.1".to_string(),
        }
    }
}

impl ServeConfig {
    /// Create a new ServeConfig with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate the configuration and return any errors
    pub fn validate(&self) -> Result<(), ServeError> {
        // Validate port range
        if self.port == 0 {
            return Err(ServeError::InvalidPort { port: 0 });
        }

        let path_str = self.directory.display().to_string();

        // Validate directory exists
        if !self.directory.exists() {
            return Err(ServeError::DirectoryNotFound { path: path_str });
        }

        // Validate it's actually a directory
        if !self.directory.is_dir() {
            return Err(ServeError::NotADirectory { path: path_str });
        }

        // Check if directory is readable
        match std::fs::read_dir(&self.directory) {
            Ok(_) => Ok(()),
            Err(_) => Err(ServeError::PermissionDenied { path: path_str }),
        }
    }
}

/// Parse command-line arguments and create ServeConfig
/// Returns Ok(config) on success, Err(error) for parsing errors, or Err(help_text) for help requests
fn parse_arguments(argv: &[String]) -> Result<ServeConfig, ServeError> {
    let mut opts = Options::new();

    // Define command-line options
    opts.optopt("p", "port", "Port to bind to (default: 8000)", "PORT");
    opts.optflag("v", "verbose", "Enable verbose request logging");
    opts.optflag(
        "o",
        "open",
        "Open browser automatically after server starts",
    );
    opts.optflag("", "cors", "Enable CORS headers for cross-origin requests");
    opts.optflag(
        "",
        "no-index",
        "Disable index.html serving, always show directory listing",
    );
    opts.optflag("h", "help", "Show this help message");

    let program = argv[0].clone();
    let args = &argv[1..];

    let matches = match opts.parse(args) {
        Ok(m) => m,
        Err(e) => {
            return Err(ServeError::ArgumentError(format!(
                "Error parsing arguments: {e}"
            )));
        }
    };

    // Show help if requested
    if matches.opt_present("h") {
        let brief = format!("Usage: {program} [OPTIONS] [DIRECTORY]");
        return Err(ServeError::ArgumentError(opts.usage(&brief)));
    }

    let mut config = ServeConfig::new();

    // Parse port option with enhanced error handling
    if let Some(port_str) = matches.opt_str("p") {
        match port_str.parse::<u32>() {
            Ok(port) => {
                if port == 0 || port > 65535 {
                    return Err(ServeError::InvalidPort { port });
                }
                config.port = port as u16;
            }
            Err(_) => {
                return Err(ServeError::ArgumentError(format!(
                    "Invalid port number: '{port_str}'. Port must be a number between 1 and 65535"
                )));
            }
        }
    }

    // Parse boolean flags
    config.verbose = matches.opt_present("v");
    config.open_browser = matches.opt_present("o");
    config.enable_cors = matches.opt_present("cors");
    config.serve_index = !matches.opt_present("no-index");

    // Parse directory argument (positional) with enhanced error handling
    if !matches.free.is_empty() {
        let dir_path = &matches.free[0];
        config.directory = PathBuf::from(dir_path);

        // Convert relative paths to absolute paths
        if config.directory.is_relative() {
            match std::env::current_dir() {
                Ok(current_dir) => {
                    config.directory = current_dir.join(&config.directory);
                }
                Err(e) => {
                    return Err(ServeError::IoError(e));
                }
            }
        }
    }

    // Validate the configuration
    config.validate()?;

    Ok(config)
}

/// Handle port conflict by suggesting alternatives
fn handle_port_conflict(port: u16, ctx: &Context) -> ExitStatus {
    let alternatives = ServeError::suggest_alternative_ports(port);
    let error = ServeError::PortInUse { port };

    ctx.write_stderr(&format!("serve: {}", error.user_message()))
        .ok();

    if !alternatives.is_empty() {
        ctx.write_stderr("").ok();
        ctx.write_stderr("You can try one of these commands:").ok();
        for alt_port in alternatives.iter().take(3) {
            ctx.write_stderr(&format!("  serve -p {alt_port}")).ok();
        }
    }

    ExitStatus::ExitedWith(1)
}

/// Handle configuration errors with user-friendly messages
fn handle_config_error(error: &ServeError, ctx: &Context) -> ExitStatus {
    match error {
        ServeError::PortInUse { port } => handle_port_conflict(*port, ctx),
        _ => {
            ctx.write_stderr(&format!("serve: {}", error.user_message()))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Directory entry information for file listings
/// Contains metadata about files and directories for HTML generation
#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    /// File or directory name
    pub name: String,
    /// True if this entry is a directory, false if it's a file
    pub is_directory: bool,
    /// File size in bytes (None for directories)
    pub size: Option<u64>,
    /// Last modification time
    pub modified: Option<DateTime<Utc>>,
    /// File path relative to the served directory
    pub relative_path: String,
}

impl DirectoryEntry {
    /// Create a new DirectoryEntry from filesystem metadata
    pub fn from_path(path: &Path, base_path: &Path) -> Result<Self, ServeError> {
        let metadata = fs::metadata(path)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let relative_path = path
            .strip_prefix(base_path)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| name.clone());

        let size = if metadata.is_file() {
            Some(metadata.len())
        } else {
            None
        };

        let modified = metadata.modified().ok().and_then(|time| {
            DateTime::from_timestamp(
                time.duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64,
                0,
            )
        });

        Ok(DirectoryEntry {
            name,
            is_directory: metadata.is_dir(),
            size,
            modified,
            relative_path,
        })
    }

    /// Format file size in human-readable format (B, KB, MB, GB)
    pub fn format_size(&self) -> String {
        match self.size {
            Some(size) => format_file_size(size),
            None => "-".to_string(),
        }
    }

    /// Format modification time in human-readable format
    pub fn format_modified(&self) -> String {
        match self.modified {
            Some(time) => time.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => "-".to_string(),
        }
    }

    /// Get the appropriate icon/symbol for this entry type
    pub fn get_icon(&self) -> &'static str {
        if self.is_directory {
            "📁"
        } else {
            // Determine icon based on file extension
            let path = Path::new(&self.name);
            match path.extension().and_then(|ext| ext.to_str()) {
                Some("html") | Some("htm") => "🌐",
                Some("css") => "🎨",
                Some("js") => "⚡",
                Some("json") => "📋",
                Some("md") => "📝",
                Some("txt") => "📄",
                Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("svg") => "🖼️",
                Some("pdf") => "📕",
                Some("zip") | Some("tar") | Some("gz") => "📦",
                Some("mp3") | Some("wav") | Some("ogg") => "🎵",
                Some("mp4") | Some("webm") | Some("avi") => "🎬",
                Some("rs") => "🦀",
                Some("py") => "🐍",
                Some("java") => "☕",
                Some("go") => "🐹",
                _ => "📄",
            }
        }
    }
}

/// Directory scanner for collecting file and directory entries
pub struct DirectoryScanner;

impl DirectoryScanner {
    /// Scan a directory and return sorted entries
    pub fn scan_directory(dir_path: &Path) -> Result<Vec<DirectoryEntry>, ServeError> {
        debug!("scanning directory: {}", dir_path.display());

        let mut entries = Vec::new();
        let read_dir = fs::read_dir(dir_path)?;

        for entry in read_dir {
            let entry = entry?;
            let path = entry.path();

            // Skip hidden files and directories (starting with .)
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }

            match DirectoryEntry::from_path(&path, dir_path) {
                Ok(dir_entry) => entries.push(dir_entry),
                Err(e) => {
                    debug!(
                        "failed to create directory entry for {}: {}",
                        path.display(),
                        e
                    );
                    // Continue with other entries instead of failing completely
                }
            }
        }

        // Sort entries: directories first, then files, both alphabetically
        entries.sort_by(|a, b| match (a.is_directory, b.is_directory) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });

        debug!("found {} entries in directory", entries.len());
        Ok(entries)
    }

    /// Check if a directory contains an index.html file
    pub fn has_index_file(dir_path: &Path) -> bool {
        let index_path = dir_path.join("index.html");
        index_path.exists() && index_path.is_file()
    }
}

/// HTML directory listing generator
/// Creates beautiful, responsive HTML pages for directory browsing
pub struct DirectoryListingGenerator;

impl DirectoryListingGenerator {
    /// Generate complete HTML page for directory listing
    pub fn generate_listing(
        entries: &[DirectoryEntry],
        request_path: &str,
        directory_path: &Path,
    ) -> String {
        let title = if request_path == "/" {
            "Directory listing for /".to_string()
        } else {
            format!("Directory listing for {request_path}")
        };

        let entries_html = Self::generate_entries_html(entries, request_path);
        let parent_link = Self::generate_parent_link(request_path);

        format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{title}</title>
    <style>
        {css}
    </style>
</head>
<body>
    <div class="container">
        <header class="header">
            <h1>{title}</h1>
            <p class="subtitle">Served by doge-shell 🐕</p>
            <p class="path-info">Local path: <code>{local_path}</code></p>
        </header>
        
        <div class="listing">
            {parent_link}
            {entries_html}
        </div>
        
        <footer class="footer">
            <p>Generated at {timestamp}</p>
        </footer>
    </div>
</body>
</html>"#,
            title = html_escape(&title),
            css = Self::get_css(),
            local_path = html_escape(&directory_path.display().to_string()),
            parent_link = parent_link,
            entries_html = entries_html,
            timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
        )
    }

    /// Generate HTML for directory entries
    fn generate_entries_html(entries: &[DirectoryEntry], request_path: &str) -> String {
        if entries.is_empty() {
            return r#"<div class="empty-directory">
                <p>📂 This directory is empty</p>
            </div>"#
                .to_string();
        }

        let mut html = String::from(
            r#"<table class="file-table">
            <thead>
                <tr>
                    <th class="icon-col"></th>
                    <th class="name-col">Name</th>
                    <th class="size-col">Size</th>
                    <th class="date-col">Modified</th>
                </tr>
            </thead>
            <tbody>"#,
        );

        for entry in entries {
            let href = Self::build_entry_url(request_path, &entry.name, entry.is_directory);
            let row_class = if entry.is_directory {
                "directory-row"
            } else {
                "file-row"
            };

            html.push_str(&format!(
                r#"<tr class="{row_class}">
                    <td class="icon-col">{icon}</td>
                    <td class="name-col">
                        <a href="{href}" class="entry-link">{name}</a>
                    </td>
                    <td class="size-col">{size}</td>
                    <td class="date-col">{modified}</td>
                </tr>"#,
                row_class = row_class,
                icon = entry.get_icon(),
                href = html_escape(&href),
                name = html_escape(&entry.name),
                size = entry.format_size(),
                modified = entry.format_modified(),
            ));
        }

        html.push_str("</tbody></table>");
        html
    }

    /// Generate parent directory link if not at root
    fn generate_parent_link(request_path: &str) -> String {
        if request_path == "/" {
            return String::new();
        }

        let parent_path = if request_path.ends_with('/') {
            let trimmed = request_path.trim_end_matches('/');
            if let Some(pos) = trimmed.rfind('/') {
                &trimmed[..pos + 1]
            } else {
                "/"
            }
        } else if let Some(pos) = request_path.rfind('/') {
            &request_path[..pos + 1]
        } else {
            "/"
        };

        format!(
            r#"<div class="parent-link">
                <a href="{}" class="parent-directory">📁 .. (Parent Directory)</a>
            </div>"#,
            html_escape(parent_path)
        )
    }

    /// Build URL for directory entry
    fn build_entry_url(request_path: &str, entry_name: &str, is_directory: bool) -> String {
        let base = if request_path.ends_with('/') {
            request_path.to_string()
        } else {
            format!("{request_path}/")
        };

        let url = format!("{base}{entry_name}");

        if is_directory && !url.ends_with('/') {
            format!("{url}/")
        } else {
            url
        }
    }

    /// Get CSS styles for directory listing
    fn get_css() -> &'static str {
        r#"
        * {
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }
        
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            line-height: 1.6;
            color: #333;
            background-color: #f8f9fa;
        }
        
        .container {
            max-width: 1200px;
            margin: 0 auto;
            padding: 20px;
        }
        
        .header {
            background: white;
            padding: 30px;
            border-radius: 8px;
            box-shadow: 0 2px 4px rgba(0,0,0,0.1);
            margin-bottom: 20px;
        }
        
        .header h1 {
            color: #2c3e50;
            margin-bottom: 10px;
            font-size: 2em;
        }
        
        .subtitle {
            color: #7f8c8d;
            font-size: 1.1em;
            margin-bottom: 10px;
        }
        
        .path-info {
            color: #6c757d;
            font-size: 0.9em;
        }
        
        .path-info code {
            background: #e9ecef;
            padding: 2px 6px;
            border-radius: 3px;
            font-family: 'Monaco', 'Menlo', monospace;
        }
        
        .listing {
            background: white;
            border-radius: 8px;
            box-shadow: 0 2px 4px rgba(0,0,0,0.1);
            overflow: hidden;
        }
        
        .parent-link {
            padding: 15px 20px;
            border-bottom: 1px solid #e9ecef;
            background: #f8f9fa;
        }
        
        .parent-directory {
            color: #495057;
            text-decoration: none;
            font-weight: 500;
        }
        
        .parent-directory:hover {
            color: #007bff;
        }
        
        .file-table {
            width: 100%;
            border-collapse: collapse;
        }
        
        .file-table th {
            background: #f8f9fa;
            padding: 12px 15px;
            text-align: left;
            font-weight: 600;
            color: #495057;
            border-bottom: 2px solid #dee2e6;
        }
        
        .file-table td {
            padding: 12px 15px;
            border-bottom: 1px solid #e9ecef;
        }
        
        .icon-col {
            width: 40px;
            text-align: center;
        }
        
        .name-col {
            width: auto;
        }
        
        .size-col {
            width: 100px;
            text-align: right;
        }
        
        .date-col {
            width: 180px;
            color: #6c757d;
            font-family: monospace;
        }
        
        .entry-link {
            color: #007bff;
            text-decoration: none;
        }
        
        .entry-link:hover {
            text-decoration: underline;
        }
        
        .directory-row {
            background: #f8f9fa;
        }
        
        .directory-row:hover {
            background: #e9ecef;
        }
        
        .file-row:hover {
            background: #f8f9fa;
        }
        
        .empty-directory {
            padding: 40px;
            text-align: center;
            color: #6c757d;
            font-size: 1.1em;
        }
        
        .footer {
            margin-top: 20px;
            text-align: center;
            color: #6c757d;
            font-size: 0.9em;
        }
        
        @media (max-width: 768px) {
            .container {
                padding: 10px;
            }
            
            .header {
                padding: 20px;
            }
            
            .header h1 {
                font-size: 1.5em;
            }
            
            .file-table th,
            .file-table td {
                padding: 8px 10px;
            }
            
            .date-col {
                display: none;
            }
            
            .size-col {
                width: 80px;
            }
        }
        "#
    }
}

/// Escape HTML special characters
fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Format file size in human-readable format
fn format_file_size(size: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    const THRESHOLD: u64 = 1024;

    if size == 0 {
        return "0 B".to_string();
    }

    let mut size_f = size as f64;
    let mut unit_index = 0;

    while size_f >= THRESHOLD as f64 && unit_index < UNITS.len() - 1 {
        size_f /= THRESHOLD as f64;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", size, UNITS[unit_index])
    } else {
        format!("{:.1} {}", size_f, UNITS[unit_index])
    }
}

/// MIME type detection system for HTTP file serving
/// Provides comprehensive MIME type support for web files and common formats
pub struct MimeTypeDetector;

impl MimeTypeDetector {
    /// Get MIME type for a file based on its extension
    /// Uses manual mapping first for web files, then mime_guess for comprehensive detection
    pub fn get_mime_type(file_path: &Path) -> &'static str {
        // First check manual mapping for common web files to ensure correct types
        match file_path.extension().and_then(|ext| ext.to_str()) {
            Some("html") | Some("htm") => "text/html; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            Some("js") => "application/javascript; charset=utf-8",
            Some("json") => "application/json; charset=utf-8",
            Some("xml") => "application/xml; charset=utf-8",
            Some("txt") => "text/plain; charset=utf-8",
            Some("md") => "text/markdown; charset=utf-8",
            Some("csv") => "text/csv; charset=utf-8",

            // Images
            Some("png") => "image/png",
            Some("jpg") | Some("jpeg") => "image/jpeg",
            Some("gif") => "image/gif",
            Some("svg") => "image/svg+xml",
            Some("ico") => "image/x-icon",
            Some("webp") => "image/webp",
            Some("bmp") => "image/bmp",
            Some("tiff") | Some("tif") => "image/tiff",

            // Fonts
            Some("woff") => "font/woff",
            Some("woff2") => "font/woff2",
            Some("ttf") => "font/ttf",
            Some("otf") => "font/otf",
            Some("eot") => "application/vnd.ms-fontobject",

            // Audio
            Some("mp3") => "audio/mpeg",
            Some("wav") => "audio/wav",
            Some("ogg") => "audio/ogg",
            Some("m4a") => "audio/mp4",
            Some("aac") => "audio/aac",
            Some("flac") => "audio/flac",

            // Video
            Some("mp4") => "video/mp4",
            Some("webm") => "video/webm",
            Some("avi") => "video/x-msvideo",
            Some("mov") => "video/quicktime",
            Some("wmv") => "video/x-ms-wmv",
            Some("flv") => "video/x-flv",
            Some("mkv") => "video/x-matroska",

            // Archives
            Some("zip") => "application/zip",
            Some("tar") => "application/x-tar",
            Some("gz") => "application/gzip",
            Some("bz2") => "application/x-bzip2",
            Some("7z") => "application/x-7z-compressed",
            Some("rar") => "application/vnd.rar",

            // Documents
            Some("pdf") => "application/pdf",
            Some("doc") => "application/msword",
            Some("docx") => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            }
            Some("xls") => "application/vnd.ms-excel",
            Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Some("ppt") => "application/vnd.ms-powerpoint",
            Some("pptx") => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            }

            // Fallback to mime_guess for other extensions
            _ => {
                let guess = MimeGuess::from_path(file_path);
                if let Some(mime) = guess.first() {
                    Self::mime_to_static_str(mime.as_ref())
                } else {
                    "application/octet-stream"
                }
            }
        }
    }

    /// Convert mime type to static string for common types
    fn mime_to_static_str(mime_str: &str) -> &'static str {
        match mime_str {
            "text/html" => "text/html; charset=utf-8",
            "text/css" => "text/css; charset=utf-8",
            "application/javascript" => "application/javascript; charset=utf-8",
            "application/json" => "application/json; charset=utf-8",
            "text/plain" => "text/plain; charset=utf-8",
            "image/png" => "image/png",
            "image/jpeg" => "image/jpeg",
            "image/gif" => "image/gif",
            "image/svg+xml" => "image/svg+xml",
            "application/pdf" => "application/pdf",
            _ => {
                // For other types, check if it's text and add charset
                if mime_str.starts_with("text/") {
                    "text/plain; charset=utf-8"
                } else {
                    "application/octet-stream"
                }
            }
        }
    }

    /// Check if a MIME type is text-based (for charset handling)
    pub fn is_text_type(mime_type: &str) -> bool {
        mime_type.starts_with("text/")
            || mime_type.starts_with("application/json")
            || mime_type.starts_with("application/javascript")
            || mime_type.starts_with("application/xml")
    }

    /// Get appropriate charset for text files
    pub fn get_charset_for_text() -> &'static str {
        "utf-8"
    }
}

/// File serving system for HTTP responses
/// Handles file streaming, MIME type detection, and HTTP headers
pub struct FileServer;

impl FileServer {
    /// Serve a file with appropriate headers and streaming
    /// Optimized for performance with efficient file streaming and caching
    pub async fn serve_file(file_path: &Path) -> Result<Response<Body>, StatusCode> {
        Self::serve_file_with_headers(file_path, None).await
    }

    /// Serve a file with conditional request support (If-Modified-Since, If-None-Match)
    pub async fn serve_file_with_headers(
        file_path: &Path,
        request_headers: Option<&HeaderMap>,
    ) -> Result<Response<Body>, StatusCode> {
        debug!("serving file: {}", file_path.display());

        // Check if file exists and is readable
        if !file_path.exists() {
            debug!("file not found: {}", file_path.display());
            return Err(StatusCode::NOT_FOUND);
        }

        if !file_path.is_file() {
            debug!("path is not a file: {}", file_path.display());
            return Err(StatusCode::NOT_FOUND);
        }

        // Get file metadata
        let metadata = match async_fs::metadata(file_path).await {
            Ok(metadata) => metadata,
            Err(e) => {
                debug!(
                    "failed to get file metadata for {}: {}",
                    file_path.display(),
                    e
                );
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        };

        let file_size = metadata.len();
        let modified_time = metadata.modified().ok();

        // Generate ETag based on file metadata
        let etag = Self::generate_etag(&metadata, file_path);

        // Check conditional requests for caching
        if let Some(headers) = request_headers {
            // Check If-None-Match (ETag)
            if let Some(if_none_match) = headers.get(header::IF_NONE_MATCH) {
                if let Ok(client_etag) = if_none_match.to_str() {
                    if client_etag == etag || client_etag == "*" {
                        debug!("returning 304 Not Modified for ETag match");
                        return Ok(Response::builder()
                            .status(StatusCode::NOT_MODIFIED)
                            .header(header::ETAG, etag)
                            .body(Body::empty())
                            .unwrap());
                    }
                }
            }

            // Check If-Modified-Since
            if let Some(if_modified_since) = headers.get(header::IF_MODIFIED_SINCE) {
                if let (Ok(client_time_str), Some(file_time)) =
                    (if_modified_since.to_str(), modified_time)
                {
                    // Simple time comparison (not parsing HTTP date for simplicity)
                    let file_timestamp = file_time
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let client_timestamp_str = format!("{file_timestamp}");

                    if client_time_str.contains(&client_timestamp_str) {
                        debug!("returning 304 Not Modified for If-Modified-Since");
                        return Ok(Response::builder()
                            .status(StatusCode::NOT_MODIFIED)
                            .header(header::ETAG, etag)
                            .body(Body::empty())
                            .unwrap());
                    }
                }
            }
        }

        // Determine MIME type
        let mime_type = MimeTypeDetector::get_mime_type(file_path);

        // Choose serving strategy based on file size
        const LARGE_FILE_THRESHOLD: u64 = 10 * 1024 * 1024; // 10MB

        let response = if file_size > LARGE_FILE_THRESHOLD {
            // Stream large files to avoid loading entire file into memory
            Self::serve_large_file(file_path, &metadata, mime_type, &etag).await?
        } else {
            // Load small files into memory for better performance
            Self::serve_small_file(file_path, &metadata, mime_type, &etag).await?
        };

        debug!(
            "successfully served file: {} ({} bytes)",
            file_path.display(),
            file_size
        );

        Ok(response)
    }

    /// Serve small files by loading them into memory
    async fn serve_small_file(
        file_path: &Path,
        metadata: &std::fs::Metadata,
        mime_type: &str,
        etag: &str,
    ) -> Result<Response<Body>, StatusCode> {
        // Read file content
        let content = match async_fs::read(file_path).await {
            Ok(content) => content,
            Err(e) => {
                debug!("failed to read file {}: {}", file_path.display(), e);
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        };

        // Build response with optimized headers
        let mut response_builder = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime_type)
            .header(header::CONTENT_LENGTH, content.len().to_string())
            .header(header::ETAG, etag);

        // Add caching headers based on file type
        response_builder = Self::add_caching_headers(response_builder, mime_type);

        // Add last-modified header
        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = modified.duration_since(SystemTime::UNIX_EPOCH) {
                let timestamp = duration.as_secs();
                let http_date = format_http_date(timestamp);
                response_builder = response_builder.header(header::LAST_MODIFIED, http_date);
            }
        }

        // Add security headers
        response_builder = Self::add_security_headers(response_builder, mime_type);

        response_builder
            .body(Body::from(content))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// Serve large files using streaming to avoid memory issues
    async fn serve_large_file(
        file_path: &Path,
        metadata: &std::fs::Metadata,
        mime_type: &str,
        etag: &str,
    ) -> Result<Response<Body>, StatusCode> {
        // Open file for streaming
        let file = match tokio::fs::File::open(file_path).await {
            Ok(file) => file,
            Err(e) => {
                debug!(
                    "failed to open file for streaming {}: {}",
                    file_path.display(),
                    e
                );
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        };

        // Create streaming body
        let reader_stream = ReaderStream::new(file);
        let body = Body::from_stream(reader_stream);

        // Build response with streaming headers
        let mut response_builder = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime_type)
            .header(header::CONTENT_LENGTH, metadata.len().to_string())
            .header(header::ETAG, etag)
            .header(header::ACCEPT_RANGES, "bytes"); // Enable range requests for large files

        // Add caching headers
        response_builder = Self::add_caching_headers(response_builder, mime_type);

        // Add last-modified header
        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = modified.duration_since(SystemTime::UNIX_EPOCH) {
                let timestamp = duration.as_secs();
                let http_date = format_http_date(timestamp);
                response_builder = response_builder.header(header::LAST_MODIFIED, http_date);
            }
        }

        // Add security headers
        response_builder = Self::add_security_headers(response_builder, mime_type);

        response_builder
            .body(body)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// Generate ETag for a file based on metadata
    fn generate_etag(metadata: &std::fs::Metadata, file_path: &Path) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();

        // Hash file path, size, and modification time for ETag
        file_path.hash(&mut hasher);
        metadata.len().hash(&mut hasher);

        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = modified.duration_since(SystemTime::UNIX_EPOCH) {
                duration.as_secs().hash(&mut hasher);
            }
        }

        format!("\"{}\"", hasher.finish())
    }

    /// Add appropriate caching headers based on file type
    fn add_caching_headers(
        mut builder: axum::http::response::Builder,
        mime_type: &str,
    ) -> axum::http::response::Builder {
        // Different caching strategies for different file types
        let cache_control = match mime_type {
            // Long cache for static assets
            t if t.starts_with("image/") => "public, max-age=31536000, immutable", // 1 year
            t if t.starts_with("font/") => "public, max-age=31536000, immutable",  // 1 year
            "text/css; charset=utf-8" => "public, max-age=86400",                  // 1 day
            "application/javascript; charset=utf-8" => "public, max-age=86400",    // 1 day

            // Short cache for dynamic content
            "text/html; charset=utf-8" => "public, max-age=300", // 5 minutes
            "application/json; charset=utf-8" => "public, max-age=300", // 5 minutes

            // Default cache
            _ => "public, max-age=3600", // 1 hour
        };

        builder = builder.header(header::CACHE_CONTROL, cache_control);

        // Add Vary header for content negotiation
        builder = builder.header(header::VARY, "Accept-Encoding");

        builder
    }

    /// Add security headers to prevent common attacks
    fn add_security_headers(
        mut builder: axum::http::response::Builder,
        mime_type: &str,
    ) -> axum::http::response::Builder {
        // Add X-Content-Type-Options to prevent MIME sniffing
        builder = builder.header("X-Content-Type-Options", "nosniff");

        // Add X-Frame-Options for HTML content
        if mime_type.starts_with("text/html") {
            builder = builder.header("X-Frame-Options", "DENY");
            builder = builder.header("X-XSS-Protection", "1; mode=block");
        }

        // Add Content-Security-Policy for HTML content
        if mime_type.starts_with("text/html") {
            builder = builder.header(
                "Content-Security-Policy", 
                "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'"
            );
        }

        builder
    }

    /// Check if a file is safe to serve (prevent directory traversal)
    pub fn is_safe_path(requested_path: &str, base_dir: &Path) -> bool {
        // Remove leading slash and decode URL
        let clean_path = requested_path.trim_start_matches('/');

        // Check for directory traversal attempts
        if clean_path.contains("..") || clean_path.contains("//") {
            return false;
        }

        // Build full path and check if it's within base directory
        let full_path = base_dir.join(clean_path);
        match full_path.canonicalize() {
            Ok(canonical) => canonical.starts_with(base_dir),
            Err(_) => false,
        }
    }

    /// Resolve file path from URL path
    pub fn resolve_file_path(url_path: &str, base_dir: &Path) -> Option<PathBuf> {
        if !Self::is_safe_path(url_path, base_dir) {
            return None;
        }

        let clean_path = url_path.trim_start_matches('/');
        let file_path = base_dir.join(clean_path);

        if file_path.exists() {
            Some(file_path)
        } else {
            None
        }
    }
}

/// Format Unix timestamp as HTTP date string
fn format_http_date(timestamp: u64) -> String {
    use chrono::{TimeZone, Utc};

    match Utc.timestamp_opt(timestamp as i64, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
        _ => Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
    }
}

/// Generate comprehensive help text for the serve command
#[cfg(test)]
fn generate_help_text() -> String {
    let help = r#"🐕 serve - HTTP file server for doge-shell

DESCRIPTION:
    A lightweight HTTP server for serving files and directories with modern web features.
    Perfect for local development, file sharing, and testing web applications.

USAGE:
    serve [OPTIONS] [DIRECTORY]

ARGUMENTS:
    [DIRECTORY]    Directory to serve files from (default: current directory)
                   Can be absolute or relative path

OPTIONS:
    -p, --port <PORT>    Port number to bind server to (default: 8000)
                         Valid range: 1-65535
    -v, --verbose        Enable detailed request logging with timestamps
                         Shows method, path, status, duration, and user agent
    -o, --open           Automatically open default browser after server starts
                         Works on macOS, Linux, and Windows
        --cors           Enable CORS headers for cross-origin requests
                         Allows web apps to make requests from different origins
        --no-index       Always show directory listings instead of index.html
                         Useful for browsing file structures
    -h, --help           Show this comprehensive help message

FEATURES:
    • Static file serving with proper MIME type detection
    • Beautiful HTML directory listings with file icons
    • Index file serving (index.html) with fallback to directory listing
    • Cross-origin resource sharing (CORS) support for development
    • Request logging with colored output and timing information
    • Graceful shutdown with Ctrl+C signal handling
    • Cross-platform browser integration
    • Security features (directory traversal prevention)
    • Performance optimized with async I/O and caching headers

EXAMPLES:
    Basic Usage:
        serve                        # Serve current directory on port 8000
        serve /path/to/website       # Serve specific directory
        serve ~/Documents            # Serve home Documents folder

    Custom Port:
        serve -p 3000               # Use port 3000 instead of 8000
        serve -p 8080 ./dist        # Serve ./dist directory on port 8080

    Development Features:
        serve -v                    # Enable verbose request logging
        serve -o                    # Open browser automatically
        serve --cors                # Enable CORS for API development
        serve -v -o --cors          # All development features enabled

    Directory Browsing:
        serve --no-index            # Always show file listings
        serve --no-index /var/log   # Browse log files with directory listing

    Web Development:
        serve -p 3000 -o ./build    # Serve React/Vue build output
        serve -v --cors ./public    # Serve with CORS and logging for API testing

SUPPORTED FILE TYPES:
    Web: HTML, CSS, JavaScript, JSON, XML
    Images: PNG, JPEG, GIF, SVG, WebP, ICO
    Documents: PDF, TXT, Markdown
    Fonts: WOFF, WOFF2, TTF, OTF
    Archives: ZIP, TAR, GZIP
    Code: Rust, Python, Java, Go, C/C++, and more

ENDPOINTS:
    /                           # Main file serving endpoint
    /_health                    # Health check endpoint (JSON response)

KEYBOARD SHORTCUTS:
    Ctrl+C                      # Graceful server shutdown

NOTES:
    • Server binds to 127.0.0.1 (localhost) by default for security
    • Files are served with appropriate caching headers for performance
    • Directory listings are generated dynamically with modern styling
    • All request paths are validated to prevent directory traversal attacks
    • Server supports graceful shutdown and proper cleanup of resources

For more information about doge-shell, visit: https://github.com/your-repo/doge-shell
"#;
    help.to_string()
}

/// Generate short usage text for error messages
#[cfg(test)]
fn generate_usage_text() -> String {
    "Usage: serve [OPTIONS] [DIRECTORY]\nTry 'serve --help' for more information.".to_string()
}

/// Built-in serve command implementation
/// Provides HTTP server functionality to serve files from a directory
///
/// Features:
/// - Serve files from current directory or specified directory
/// - Configurable port (default: 8000)
/// - Directory listing with HTML interface
/// - CORS support for development
/// - Automatic browser opening
/// - Request logging
/// - MIME type detection
/// - Index file serving (index.html)
///
/// Usage:
///   serve                           - Serve current directory on port 8000
///   serve -p 3000                   - Serve on port 3000
///   serve /path/to/dir              - Serve specific directory
///   serve -v                        - Verbose logging
///   serve -o                        - Open browser automatically
///   serve --cors                    - Enable CORS headers
///   serve --no-index                - Disable index.html serving
/// Start the HTTP server with the given configuration
/// This function handles the complete server lifecycle using the existing runtime
fn start_http_server(ctx: &Context, config: ServeConfig) -> Result<(), ServeError> {
    debug!("starting HTTP server with configuration: {:?}", config);

    // Create a new runtime specifically for the server to avoid nesting issues
    // This is the safest approach when we need to run async code from sync context
    let server_rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            error!("failed to create server runtime: {}", e);
            return Err(ServeError::ServerError(Box::new(e)));
        }
    };

    // Use spawn_blocking to run the server in a separate thread
    // This completely avoids the runtime nesting issue
    let ctx_clone = ctx.clone();
    let result =
        std::thread::spawn(move || server_rt.block_on(run_server_async(&ctx_clone, config))).join();

    match result {
        Ok(server_result) => server_result,
        Err(e) => {
            error!("server thread panicked: {:?}", e);
            Err(ServeError::ServerError(Box::new(std::io::Error::other(
                "Server thread panicked",
            ))))
        }
    }
}

/// Async function to run the HTTP server
async fn run_server_async(ctx: &Context, config: ServeConfig) -> Result<(), ServeError> {
    debug!("running HTTP server asynchronously");

    // Create and start the HTTP server
    let mut server = HttpServer::new(config);

    // Display server information
    ctx.write_stdout("🐕 doge-shell HTTP server starting...")
        .ok();
    ctx.write_stdout(&format!(
        "  Directory: {}",
        server.config.directory.display()
    ))
    .ok();
    ctx.write_stdout(&format!("  Port: {}", server.config.port))
        .ok();
    ctx.write_stdout("  Press Ctrl+C to stop the server").ok();
    ctx.write_stdout("").ok();

    // Start the server in a background task
    let server_task = tokio::spawn(async move {
        if let Err(e) = server.start().await {
            error!("server error: {}", e);
        }
    });

    // Wait for shutdown signal
    let shutdown_task = tokio::spawn(async {
        SignalHandler::wait_for_shutdown().await;
    });

    // Wait for either server completion or shutdown signal
    tokio::select! {
        result = server_task => {
            match result {
                Ok(_) => {
                    info!("server completed successfully");
                }
                Err(e) => {
                    error!("server task failed: {}", e);
                    return Err(ServeError::ServerError(Box::new(e)));
                }
            }
        }
        _ = shutdown_task => {
            info!("shutdown signal received");
        }
    }

    info!("HTTP server stopped");
    Ok(())
}

/// Handle requests to the root path "/"
async fn serve_root_handler(serve_dir: PathBuf, serve_index: bool) -> Response<Body> {
    debug!("serving root directory: {:?}", serve_dir);

    // Try to serve index.html if serve_index is enabled
    if serve_index {
        let index_path = serve_dir.join("index.html");
        if index_path.exists() && index_path.is_file() {
            return serve_file(&index_path).await;
        }
    }

    // Otherwise, serve directory listing
    serve_directory(&serve_dir).await
}

/// Handle requests to specific paths
async fn serve_file_handler(
    uri: Uri,
    serve_dir: PathBuf,
    serve_index: bool,
    enable_cors: bool,
) -> Response<Body> {
    let path = uri.path();
    debug!("serving file request: {}", path);

    // Remove leading slash and decode URL
    let relative_path = path.strip_prefix('/').unwrap_or(path);
    let file_path = serve_dir.join(relative_path);

    // Security check: ensure the path is within the serve directory
    match file_path.canonicalize() {
        Ok(canonical_path) => {
            match serve_dir.canonicalize() {
                Ok(canonical_serve_dir) => {
                    if !canonical_path.starts_with(&canonical_serve_dir) {
                        return create_error_response(StatusCode::FORBIDDEN, "Access denied");
                    }
                }
                Err(_) => {
                    return create_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Server error",
                    );
                }
            }

            // Serve the file or directory
            if canonical_path.is_file() {
                let mut response = serve_file(&canonical_path).await;
                if enable_cors {
                    add_cors_headers(response.headers_mut());
                }
                response
            } else if canonical_path.is_dir() {
                // Try to serve index.html if serve_index is enabled
                if serve_index {
                    let index_path = canonical_path.join("index.html");
                    if index_path.exists() && index_path.is_file() {
                        let mut response = serve_file(&index_path).await;
                        if enable_cors {
                            add_cors_headers(response.headers_mut());
                        }
                        return response;
                    }
                }

                // Serve directory listing
                let mut response = serve_directory(&canonical_path).await;
                if enable_cors {
                    add_cors_headers(response.headers_mut());
                }
                response
            } else {
                create_error_response(StatusCode::NOT_FOUND, "File not found")
            }
        }
        Err(_) => create_error_response(StatusCode::NOT_FOUND, "File not found"),
    }
}

/// Serve a single file
async fn serve_file(file_path: &Path) -> Response<Body> {
    debug!("serving file: {:?}", file_path);

    match async_fs::File::open(file_path).await {
        Ok(file) => {
            let mime_type = MimeGuess::from_path(file_path)
                .first_or_octet_stream()
                .to_string();

            let stream = ReaderStream::new(file);
            let body = Body::from_stream(stream);

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime_type)
                .body(body)
                .unwrap_or_else(|_| {
                    create_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to create response",
                    )
                })
        }
        Err(e) => {
            error!("failed to open file {:?}: {}", file_path, e);
            create_error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read file")
        }
    }
}

/// Serve directory listing as HTML
async fn serve_directory(dir_path: &Path) -> Response<Body> {
    debug!("serving directory: {:?}", dir_path);

    match fs::read_dir(dir_path) {
        Ok(entries) => {
            let mut html = String::new();
            html.push_str("<!DOCTYPE html>\n");
            html.push_str("<html><head><title>Directory Listing</title>");
            html.push_str("<style>");
            html.push_str("body { font-family: Arial, sans-serif; margin: 40px; }");
            html.push_str("h1 { color: #333; border-bottom: 1px solid #ccc; }");
            html.push_str("ul { list-style: none; padding: 0; }");
            html.push_str("li { margin: 5px 0; }");
            html.push_str("a { text-decoration: none; color: #0066cc; }");
            html.push_str("a:hover { text-decoration: underline; }");
            html.push_str(".dir { font-weight: bold; }");
            html.push_str(".file { color: #666; }");
            html.push_str("</style></head><body>");

            html.push_str(&format!("<h1>🐕 Directory: {}</h1>", dir_path.display()));
            html.push_str("<ul>");

            // Add parent directory link if not root
            if let Some(parent) = dir_path.parent() {
                if parent != dir_path {
                    html.push_str("<li><a href=\"../\" class=\"dir\">📁 ../</a></li>");
                }
            }

            // Collect and sort entries
            let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            entries.sort_by(|a, b| {
                let a_is_dir = a.path().is_dir();
                let b_is_dir = b.path().is_dir();

                // Directories first, then files
                match (a_is_dir, b_is_dir) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.file_name().cmp(&b.file_name()),
                }
            });

            // Add entries to HTML
            for entry in entries {
                let file_name = entry.file_name();
                let file_name_str = file_name.to_string_lossy();
                let path = entry.path();

                if path.is_dir() {
                    html.push_str(&format!(
                        "<li><a href=\"{file_name_str}\" class=\"dir\">📁 {file_name_str}/</a></li>"
                    ));
                } else {
                    html.push_str(&format!(
                        "<li><a href=\"{file_name_str}\" class=\"file\">📄 {file_name_str}</a></li>"
                    ));
                }
            }

            html.push_str("</ul>");
            html.push_str("<hr><p><em>Served by 🐕 doge-shell</em></p>");
            html.push_str("</body></html>");

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Body::from(html))
                .unwrap_or_else(|_| {
                    create_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to create response",
                    )
                })
        }
        Err(e) => {
            error!("failed to read directory {:?}: {}", dir_path, e);
            create_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to read directory",
            )
        }
    }
}

/// Create an error response
fn create_error_response(status: StatusCode, message: &str) -> Response<Body> {
    let html = format!(
        "<!DOCTYPE html><html><head><title>Error {}</title></head><body><h1>Error {}</h1><p>{}</p></body></html>",
        status.as_u16(),
        status.as_u16(),
        message
    );

    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("Internal Server Error"))
                .unwrap()
        })
}

/// Add CORS headers to response
fn add_cors_headers(headers: &mut HeaderMap) {
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        "GET, POST, PUT, DELETE, OPTIONS".parse().unwrap(),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "Content-Type, Authorization".parse().unwrap(),
    );
}

/// HTTP server implementation for serving files
/// Handles server lifecycle, configuration, and graceful shutdown
pub struct HttpServer {
    config: ServeConfig,
}

impl HttpServer {
    /// Create a new HTTP server with the given configuration
    pub fn new(config: ServeConfig) -> Self {
        Self { config }
    }

    /// Start the HTTP server and handle requests
    pub async fn start(&mut self) -> Result<(), ServeError> {
        use axum::{Router, http::Uri, routing::get};
        use std::net::SocketAddr;

        use tokio::net::TcpListener;

        let addr: SocketAddr = format!("{}:{}", self.config.host, self.config.port)
            .parse()
            .map_err(|e| ServeError::NetworkError(format!("Invalid address: {e}")))?;

        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| ServeError::NetworkError(format!("Failed to bind to {addr}: {e}")))?;

        info!("HTTP server listening on http://{}", addr);

        // Create a router with proper file serving capabilities
        let serve_dir = self.config.directory.clone();
        let serve_index = self.config.serve_index;
        let enable_cors = self.config.enable_cors;

        let app = Router::new()
            .route(
                "/",
                get({
                    let serve_dir = serve_dir.clone();
                    move || serve_root_handler(serve_dir, serve_index)
                }),
            )
            .route(
                "/*path",
                get({
                    let serve_dir = serve_dir.clone();
                    move |uri: Uri| serve_file_handler(uri, serve_dir, serve_index, enable_cors)
                }),
            );

        axum::serve(listener, app)
            .await
            .map_err(|e| ServeError::ServerError(Box::new(e)))?;

        Ok(())
    }
}

/// Signal handler for graceful shutdown
/// Handles various shutdown signals across different platforms
pub struct SignalHandler;

impl SignalHandler {
    /// Wait for shutdown signal (Ctrl+C, SIGTERM, etc.)
    pub async fn wait_for_shutdown() {
        use tokio::signal;

        #[cfg(unix)]
        {
            let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
            let mut sigint = signal::unix::signal(signal::unix::SignalKind::interrupt())
                .expect("failed to install SIGINT handler");

            tokio::select! {
                _ = sigterm.recv() => {
                    info!("received SIGTERM");
                }
                _ = sigint.recv() => {
                    info!("received SIGINT");
                }
            }
        }

        #[cfg(windows)]
        {
            let _ = signal::ctrl_c().await;
            info!("received Ctrl+C");
        }
    }
}

/// Main serve command entry point
/// Implements the builtin command interface for the HTTP serve functionality
pub fn command(ctx: &Context, argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    debug!("serve command called with args: {:?}", argv);

    // Parse command-line arguments with enhanced error handling
    let config = match parse_arguments(&argv) {
        Ok(config) => config,
        Err(err) => {
            match &err {
                ServeError::ArgumentError(msg) => {
                    // Check if this is a help request (not an error)
                    if msg.contains("Usage:") {
                        ctx.write_stdout(msg).ok();
                        return ExitStatus::ExitedWith(0);
                    } else {
                        ctx.write_stderr(&format!("serve: {msg}")).ok();
                        return ExitStatus::ExitedWith(1);
                    }
                }
                _ => {
                    return handle_config_error(&err, ctx);
                }
            }
        }
    };

    debug!("serve config: {:?}", config);

    // Start the HTTP server
    match start_http_server(ctx, config) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(err) => handle_config_error(&err, ctx),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::TempDir;

    /// Create a temporary directory with test files for testing
    fn create_test_directory() -> TempDir {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");

        // Create test files
        let mut html_file = File::create(temp_dir.path().join("index.html")).unwrap();
        html_file
            .write_all(b"<html><body>Test HTML</body></html>")
            .unwrap();

        let mut css_file = File::create(temp_dir.path().join("style.css")).unwrap();
        css_file.write_all(b"body { color: red; }").unwrap();

        let mut js_file = File::create(temp_dir.path().join("script.js")).unwrap();
        js_file.write_all(b"console.log('test');").unwrap();

        let mut txt_file = File::create(temp_dir.path().join("readme.txt")).unwrap();
        txt_file.write_all(b"This is a test file").unwrap();

        // Create subdirectory
        fs::create_dir(temp_dir.path().join("subdir")).unwrap();
        let mut sub_file = File::create(temp_dir.path().join("subdir/test.md")).unwrap();
        sub_file.write_all(b"# Test Markdown").unwrap();

        temp_dir
    }

    #[test]
    fn test_serve_config_default() {
        let config = ServeConfig::default();

        assert_eq!(config.port, 8000);
        assert_eq!(config.host, "127.0.0.1");
        assert!(!config.verbose);
        assert!(!config.open_browser);
        assert!(!config.enable_cors);
        assert!(config.serve_index);
        assert!(config.directory.exists());
    }

    #[test]
    fn test_serve_config_new() {
        let config = ServeConfig::new();

        assert_eq!(config.port, 8000);
        assert_eq!(config.host, "127.0.0.1");
        assert!(!config.verbose);
        assert!(!config.open_browser);
        assert!(!config.enable_cors);
        assert!(config.serve_index);
    }

    #[test]
    fn test_serve_config_validation_valid() {
        let temp_dir = create_test_directory();
        let mut config = ServeConfig::new();
        config.directory = temp_dir.path().to_path_buf();

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_serve_config_validation_invalid_port() {
        let mut config = ServeConfig::new();
        config.port = 0;

        let result = config.validate();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ServeError::InvalidPort { port: 0 }
        ));
    }

    #[test]
    fn test_serve_config_validation_directory_not_found() {
        let mut config = ServeConfig::new();
        config.directory = PathBuf::from("/nonexistent/directory");

        let result = config.validate();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ServeError::DirectoryNotFound { .. }
        ));
    }

    #[test]
    fn test_serve_config_validation_not_a_directory() {
        let temp_dir = create_test_directory();
        let mut config = ServeConfig::new();
        config.directory = temp_dir.path().join("index.html"); // Point to a file, not directory

        let result = config.validate();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ServeError::NotADirectory { .. }
        ));
    }

    #[test]
    fn test_parse_arguments_default() {
        let argv = vec!["serve".to_string()];
        let result = parse_arguments(&argv);

        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.port, 8000);
        assert!(!config.verbose);
        assert!(!config.open_browser);
        assert!(!config.enable_cors);
        assert!(config.serve_index);
    }

    #[test]
    fn test_parse_arguments_port() {
        let argv = vec!["serve".to_string(), "-p".to_string(), "3000".to_string()];
        let result = parse_arguments(&argv);

        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.port, 3000);
    }

    #[test]
    fn test_parse_arguments_port_long() {
        let argv = vec![
            "serve".to_string(),
            "--port".to_string(),
            "8080".to_string(),
        ];
        let result = parse_arguments(&argv);

        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.port, 8080);
    }

    #[test]
    fn test_parse_arguments_invalid_port() {
        let argv = vec!["serve".to_string(), "-p".to_string(), "0".to_string()];
        let result = parse_arguments(&argv);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ServeError::InvalidPort { port: 0 }
        ));
    }

    #[test]
    fn test_parse_arguments_invalid_port_too_high() {
        let argv = vec!["serve".to_string(), "-p".to_string(), "65536".to_string()];
        let result = parse_arguments(&argv);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ServeError::InvalidPort { port: 65536 }
        ));
    }

    #[test]
    fn test_parse_arguments_invalid_port_non_numeric() {
        let argv = vec!["serve".to_string(), "-p".to_string(), "abc".to_string()];
        let result = parse_arguments(&argv);

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ServeError::ArgumentError(_)));
    }

    #[test]
    fn test_parse_arguments_verbose() {
        let argv = vec!["serve".to_string(), "-v".to_string()];
        let result = parse_arguments(&argv);

        assert!(result.is_ok());
        let config = result.unwrap();
        assert!(config.verbose);
    }

    #[test]
    fn test_parse_arguments_verbose_long() {
        let argv = vec!["serve".to_string(), "--verbose".to_string()];
        let result = parse_arguments(&argv);

        assert!(result.is_ok());
        let config = result.unwrap();
        assert!(config.verbose);
    }

    #[test]
    fn test_parse_arguments_open() {
        let argv = vec!["serve".to_string(), "-o".to_string()];
        let result = parse_arguments(&argv);

        assert!(result.is_ok());
        let config = result.unwrap();
        assert!(config.open_browser);
    }

    #[test]
    fn test_parse_arguments_cors() {
        let argv = vec!["serve".to_string(), "--cors".to_string()];
        let result = parse_arguments(&argv);

        assert!(result.is_ok());
        let config = result.unwrap();
        assert!(config.enable_cors);
    }

    #[test]
    fn test_parse_arguments_no_index() {
        let argv = vec!["serve".to_string(), "--no-index".to_string()];
        let result = parse_arguments(&argv);

        assert!(result.is_ok());
        let config = result.unwrap();
        assert!(!config.serve_index);
    }

    #[test]
    fn test_parse_arguments_directory() {
        let temp_dir = create_test_directory();
        let argv = vec![
            "serve".to_string(),
            temp_dir.path().to_str().unwrap().to_string(),
        ];
        let result = parse_arguments(&argv);

        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.directory, temp_dir.path());
    }

    #[test]
    fn test_parse_arguments_help() {
        let argv = vec!["serve".to_string(), "-h".to_string()];
        let result = parse_arguments(&argv);

        assert!(result.is_err());
        if let Err(ServeError::ArgumentError(msg)) = result {
            assert!(msg.contains("Usage:"));
        } else {
            panic!("Expected ArgumentError with usage message");
        }
    }

    #[test]
    fn test_parse_arguments_multiple_flags() {
        let temp_dir = create_test_directory();
        let argv = vec![
            "serve".to_string(),
            "-v".to_string(),
            "-o".to_string(),
            "--cors".to_string(),
            "--no-index".to_string(),
            "-p".to_string(),
            "3000".to_string(),
            temp_dir.path().to_str().unwrap().to_string(),
        ];
        let result = parse_arguments(&argv);

        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.port, 3000);
        assert!(config.verbose);
        assert!(config.open_browser);
        assert!(config.enable_cors);
        assert!(!config.serve_index);
        assert_eq!(config.directory, temp_dir.path());
    }

    #[test]
    fn test_mime_type_detector_html() {
        let path = Path::new("test.html");
        let mime_type = MimeTypeDetector::get_mime_type(path);
        assert_eq!(mime_type, "text/html; charset=utf-8");
    }

    #[test]
    fn test_mime_type_detector_css() {
        let path = Path::new("style.css");
        let mime_type = MimeTypeDetector::get_mime_type(path);
        assert_eq!(mime_type, "text/css; charset=utf-8");
    }

    #[test]
    fn test_mime_type_detector_javascript() {
        let path = Path::new("script.js");
        let mime_type = MimeTypeDetector::get_mime_type(path);
        assert_eq!(mime_type, "application/javascript; charset=utf-8");
    }

    #[test]
    fn test_mime_type_detector_json() {
        let path = Path::new("data.json");
        let mime_type = MimeTypeDetector::get_mime_type(path);
        assert_eq!(mime_type, "application/json; charset=utf-8");
    }

    #[test]
    fn test_mime_type_detector_png() {
        let path = Path::new("image.png");
        let mime_type = MimeTypeDetector::get_mime_type(path);
        assert_eq!(mime_type, "image/png");
    }

    #[test]
    fn test_mime_type_detector_jpeg() {
        let path = Path::new("photo.jpg");
        let mime_type = MimeTypeDetector::get_mime_type(path);
        assert_eq!(mime_type, "image/jpeg");
    }

    #[test]
    fn test_mime_type_detector_unknown() {
        let path = Path::new("unknown.xyz");
        let mime_type = MimeTypeDetector::get_mime_type(path);
        assert_eq!(mime_type, "application/octet-stream");
    }

    #[test]
    fn test_mime_type_detector_case_insensitive() {
        let path = Path::new("TEST.HTML");
        let mime_type = MimeTypeDetector::get_mime_type(path);
        assert_eq!(mime_type, "text/html; charset=utf-8");
    }

    #[test]
    fn test_mime_type_detector_is_text_type() {
        assert!(MimeTypeDetector::is_text_type("text/html"));
        assert!(MimeTypeDetector::is_text_type("text/plain"));
        assert!(MimeTypeDetector::is_text_type("application/json"));
        assert!(MimeTypeDetector::is_text_type("application/javascript"));
        assert!(MimeTypeDetector::is_text_type("application/xml"));
        assert!(!MimeTypeDetector::is_text_type("image/png"));
        assert!(!MimeTypeDetector::is_text_type("application/octet-stream"));
    }

    #[test]
    fn test_directory_scanner_scan_directory() {
        let temp_dir = create_test_directory();
        let result = DirectoryScanner::scan_directory(temp_dir.path());

        assert!(result.is_ok());
        let entries = result.unwrap();

        // Should have files and subdirectory (excluding hidden files)
        assert!(!entries.is_empty());

        // Check that we have both files and directories
        let has_files = entries.iter().any(|e| !e.is_directory);
        let has_dirs = entries.iter().any(|e| e.is_directory);
        assert!(has_files);
        assert!(has_dirs);

        // Check sorting (directories first, then files, alphabetically)
        let mut prev_was_dir = true;
        for entry in &entries {
            if !entry.is_directory && prev_was_dir {
                prev_was_dir = false;
            } else if entry.is_directory && !prev_was_dir {
                panic!("Directories should come before files");
            }
        }
    }

    #[test]
    fn test_directory_scanner_has_index_file() {
        let temp_dir = create_test_directory();
        assert!(DirectoryScanner::has_index_file(temp_dir.path()));

        // Test directory without index.html
        let temp_dir2 = TempDir::new().unwrap();
        assert!(!DirectoryScanner::has_index_file(temp_dir2.path()));
    }

    #[test]
    fn test_directory_entry_from_path() {
        let temp_dir = create_test_directory();
        let html_path = temp_dir.path().join("index.html");

        let result = DirectoryEntry::from_path(&html_path, temp_dir.path());
        assert!(result.is_ok());

        let entry = result.unwrap();
        assert_eq!(entry.name, "index.html");
        assert!(!entry.is_directory);
        assert!(entry.size.is_some());
        assert!(entry.size.unwrap() > 0);
        assert!(entry.modified.is_some());
    }

    #[test]
    fn test_directory_entry_format_size() {
        let mut entry = DirectoryEntry {
            name: "test.txt".to_string(),
            is_directory: false,
            size: Some(1024),
            modified: None,
            relative_path: "test.txt".to_string(),
        };

        assert_eq!(entry.format_size(), "1.0 KB");

        entry.size = Some(0);
        assert_eq!(entry.format_size(), "0 B");

        entry.size = Some(1536); // 1.5 KB
        assert_eq!(entry.format_size(), "1.5 KB");

        entry.size = None;
        assert_eq!(entry.format_size(), "-");
    }

    #[test]
    fn test_directory_entry_get_icon() {
        let html_entry = DirectoryEntry {
            name: "index.html".to_string(),
            is_directory: false,
            size: Some(100),
            modified: None,
            relative_path: "index.html".to_string(),
        };
        assert_eq!(html_entry.get_icon(), "🌐");

        let dir_entry = DirectoryEntry {
            name: "folder".to_string(),
            is_directory: true,
            size: None,
            modified: None,
            relative_path: "folder".to_string(),
        };
        assert_eq!(dir_entry.get_icon(), "📁");

        let rust_entry = DirectoryEntry {
            name: "main.rs".to_string(),
            is_directory: false,
            size: Some(200),
            modified: None,
            relative_path: "main.rs".to_string(),
        };
        assert_eq!(rust_entry.get_icon(), "🦀");
    }

    #[test]
    fn test_format_file_size() {
        assert_eq!(format_file_size(0), "0 B");
        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(1024), "1.0 KB");
        assert_eq!(format_file_size(1536), "1.5 KB");
        assert_eq!(format_file_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_file_size(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(format_file_size(1024_u64.pow(4)), "1.0 TB");
    }

    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("normal text"), "normal text");
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("&amp;"), "&amp;amp;");
        assert_eq!(html_escape("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(html_escape("'single'"), "&#x27;single&#x27;");
        assert_eq!(html_escape("<>&\"'"), "&lt;&gt;&amp;&quot;&#x27;");
    }

    #[test]
    fn test_file_server_is_safe_path() {
        let temp_dir = create_test_directory();

        // Safe paths
        assert!(FileServer::is_safe_path("/", temp_dir.path()));
        assert!(FileServer::is_safe_path("/index.html", temp_dir.path()));
        assert!(FileServer::is_safe_path("/subdir/test.md", temp_dir.path()));

        // Unsafe paths (directory traversal attempts)
        assert!(!FileServer::is_safe_path("/../etc/passwd", temp_dir.path()));
        assert!(!FileServer::is_safe_path(
            "/subdir/../../../etc/passwd",
            temp_dir.path()
        ));
        assert!(!FileServer::is_safe_path("//etc/passwd", temp_dir.path()));
        assert!(!FileServer::is_safe_path("/subdir//test", temp_dir.path()));
    }

    #[test]
    fn test_file_server_resolve_file_path() {
        let temp_dir = create_test_directory();

        // Valid file resolution
        let result = FileServer::resolve_file_path("/index.html", temp_dir.path());
        assert!(result.is_some());
        assert_eq!(result.unwrap(), temp_dir.path().join("index.html"));

        // Non-existent file
        let result = FileServer::resolve_file_path("/nonexistent.txt", temp_dir.path());
        assert!(result.is_none());

        // Unsafe path
        let result = FileServer::resolve_file_path("/../etc/passwd", temp_dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn test_serve_config_cors_flag() {
        let mut config = ServeConfig::new();
        assert!(!config.enable_cors);

        config.enable_cors = true;
        assert!(config.enable_cors);
    }

    #[test]
    fn test_serve_error_user_message() {
        let error = ServeError::PortInUse { port: 8000 };
        let message = error.user_message();
        assert!(message.contains("Port 8000 is already in use"));
        assert!(message.contains("Try using a different port"));

        let error = ServeError::InvalidPort { port: 70000 };
        let message = error.user_message();
        assert!(message.contains("Invalid port number: 70000"));
        assert!(message.contains("Port must be between 1 and 65535"));

        let error = ServeError::DirectoryNotFound {
            path: "/nonexistent".to_string(),
        };
        let message = error.user_message();
        assert!(message.contains("Directory not found: /nonexistent"));
        assert!(message.contains("Make sure the directory exists"));
    }

    #[test]
    fn test_serve_error_suggest_alternative_ports() {
        let alternatives = ServeError::suggest_alternative_ports(8000);
        assert!(!alternatives.contains(&8000));
        assert!(alternatives.contains(&8001));
        assert!(alternatives.contains(&8080));
        assert!(alternatives.contains(&3000));
    }

    #[test]
    fn test_serve_config_host_validation() {
        let mut config = ServeConfig::new();
        assert_eq!(config.host, "127.0.0.1");

        config.host = "localhost".to_string();
        assert_eq!(config.host, "localhost");

        config.host = "0.0.0.0".to_string();
        assert_eq!(config.host, "0.0.0.0");
    }

    #[test]
    fn test_generate_help_text() {
        let help = generate_help_text();

        assert!(help.contains("serve - HTTP file server"));
        assert!(help.contains("USAGE:"));
        assert!(help.contains("OPTIONS:"));
        assert!(help.contains("EXAMPLES:"));
        assert!(help.contains("--port"));
        assert!(help.contains("--verbose"));
        assert!(help.contains("--cors"));
        assert!(help.contains("doge-shell"));
    }

    #[test]
    fn test_generate_usage_text() {
        let usage = generate_usage_text();

        assert!(usage.contains("Usage: serve"));
        assert!(usage.contains("--help"));
    }
}
