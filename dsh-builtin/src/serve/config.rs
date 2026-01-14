use super::error::ServeError;
use getopts::Options;
use std::path::PathBuf;

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
pub fn parse_arguments(argv: &[String]) -> Result<ServeConfig, ServeError> {
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
