use super::config::{ServeConfig, parse_arguments};
use super::error::ServeError;
use super::handlers::{FileServer, MimeTypeDetector};
use super::scanner::{DirectoryEntry, DirectoryScanner};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Generate comprehensive help text for the serve command
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
fn generate_usage_text() -> String {
    "Usage: serve [OPTIONS] [DIRECTORY]\nTry 'serve --help' for more information.".to_string()
}

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
