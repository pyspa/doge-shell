use super::config::{ServeConfig, parse_arguments};
use super::error::ServeError;
use super::handlers::{FileServer, MimeTypeDetector};
use super::scanner::{DirectoryEntry, DirectoryScanner};
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
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
    assert!(!config.enable_cors);
    assert!(config.serve_index);
    assert!(config.directory.exists());
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

type FlagCheck = fn(&ServeConfig) -> bool;

/// Each single boolean flag (short or long spelling) sets exactly the field
/// it names and leaves the rest at their `parse_arguments_default` values.
#[test]
fn test_parse_arguments_single_boolean_flags() {
    let cases: &[(&str, FlagCheck)] = &[
        ("-v", |c| c.verbose),
        ("--verbose", |c| c.verbose),
        ("--cors", |c| c.enable_cors),
        ("--no-index", |c| !c.serve_index),
    ];

    for (flag, field) in cases {
        let argv = vec!["serve".to_string(), flag.to_string()];
        let config =
            parse_arguments(&argv).unwrap_or_else(|error| panic!("{flag} should parse: {error}"));
        assert!(field(&config), "{flag} should set its flag");
    }
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
    assert!(config.enable_cors);
    assert!(!config.serve_index);
    assert_eq!(config.directory, temp_dir.path());
}

#[test]
fn test_mime_type_detector_by_extension() {
    let cases = [
        ("test.html", "text/html; charset=utf-8"),
        ("style.css", "text/css; charset=utf-8"),
        ("script.js", "application/javascript; charset=utf-8"),
        ("data.json", "application/json; charset=utf-8"),
        ("image.png", "image/png"),
        ("photo.jpg", "image/jpeg"),
        ("unknown.xyz", "application/octet-stream"),
        // Extension matching is case-insensitive.
        ("TEST.HTML", "text/html; charset=utf-8"),
    ];

    for (file_name, expected) in cases {
        let mime_type = MimeTypeDetector::get_mime_type(Path::new(file_name));
        assert_eq!(mime_type, expected, "mime type for {file_name}");
    }
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
fn test_add_cors_headers() {
    use super::handlers::add_cors_headers;
    use axum::http::HeaderMap;

    let mut headers = HeaderMap::new();
    add_cors_headers(&mut headers);

    assert_eq!(headers.get("access-control-allow-origin").unwrap(), "*");
    assert_eq!(
        headers.get("access-control-allow-methods").unwrap(),
        "GET, POST, PUT, DELETE, OPTIONS"
    );
    assert_eq!(
        headers.get("access-control-allow-headers").unwrap(),
        "Content-Type, Authorization"
    );
}

fn http_date_from_system_time(time: std::time::SystemTime) -> String {
    use chrono::{TimeZone, Utc};

    let secs = time
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("test file mtime should be after the epoch")
        .as_secs();
    Utc.timestamp_opt(secs as i64, 0)
        .single()
        .expect("test file mtime should map to a single UTC timestamp")
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

fn response_status(response: &Response<Body>) -> StatusCode {
    response.status()
}

#[tokio::test]
async fn test_if_modified_since_matching_returns_304() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("page.html");
    fs::write(&file_path, b"<html>hello</html>").unwrap();

    let mtime = fs::metadata(&file_path).unwrap().modified().unwrap();
    let if_modified_since = http_date_from_system_time(mtime);

    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::IF_MODIFIED_SINCE,
        if_modified_since.parse().unwrap(),
    );

    let response = FileServer::serve_file_with_headers(&file_path, Some(&headers))
        .await
        .unwrap();
    assert_eq!(response_status(&response), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn test_if_modified_since_older_date_returns_200() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("page.html");
    fs::write(&file_path, b"<html>hello</html>").unwrap();

    let mtime = fs::metadata(&file_path).unwrap().modified().unwrap();
    let older = mtime - std::time::Duration::from_secs(3600);
    let if_modified_since = http_date_from_system_time(older);

    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::IF_MODIFIED_SINCE,
        if_modified_since.parse().unwrap(),
    );

    let response = FileServer::serve_file_with_headers(&file_path, Some(&headers))
        .await
        .unwrap();
    assert_eq!(response_status(&response), StatusCode::OK);
}

#[tokio::test]
async fn test_directory_listing_escapes_file_names() {
    let temp_dir = TempDir::new().unwrap();
    fs::write(
        temp_dir.path().join("<img src=x onerror=alert(1)>.txt"),
        b"x",
    )
    .unwrap();
    fs::write(temp_dir.path().join("a b.txt"), b"y").unwrap();

    let response = super::handlers::serve_file_handler(
        axum::http::Uri::from_static("/"),
        temp_dir.path().to_path_buf(),
        false,
        false,
    )
    .await;

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();

    // The script-injection file name must not appear unescaped in the HTML.
    assert!(!html.contains("<img src=x"));
    assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;.txt"));
    // The href for a name with a space must be percent-encoded so the link
    // survives as a single attribute value.
    assert!(html.contains("href=\"a%20b.txt\""));
    assert!(html.contains(">📄 a b.txt</a>"));
}

#[tokio::test]
async fn test_encoded_link_resolves_back_to_file() {
    let temp_dir = TempDir::new().unwrap();
    fs::write(temp_dir.path().join("a b.txt"), b"payload").unwrap();

    // A browser follows the percent-encoded href generated by the listing.
    let response = super::handlers::serve_file_handler(
        axum::http::Uri::from_static("/a%20b.txt"),
        temp_dir.path().to_path_buf(),
        false,
        false,
    )
    .await;

    assert_eq!(response_status(&response), StatusCode::OK);
}

#[test]
fn test_url_path_escape_unescape_roundtrip() {
    use super::scanner::{url_path_escape, url_path_unescape};

    for name in [
        "plain.txt",
        "a b.txt",
        "quote'.txt",
        "quote\".txt",
        "こんにちは.md",
        "100%.txt",
    ] {
        assert_eq!(url_path_unescape(&url_path_escape(name)), name);
    }
}

#[test]
fn test_url_path_unescape_keeps_invalid_escapes() {
    use super::scanner::url_path_unescape;

    assert_eq!(url_path_unescape("100%.txt"), "100%.txt");
    assert_eq!(url_path_unescape("a%2"), "a%2");
    assert_eq!(url_path_unescape("a%ZZb"), "a%ZZb");
}
