use axum::body::Body;
use axum::http::header::ToStrError;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::Response;
use mime_guess::MimeGuess;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tokio::fs as async_fs;
use tokio_util::io::ReaderStream;
use tracing::{debug, error};

/// MIME type detector
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
            // Inside impl
            // Check If-None-Match (ETag)
            if let Some(if_none_match) = headers.get(header::IF_NONE_MATCH) {
                let client_etag_res: Result<&str, ToStrError> = if_none_match.to_str();
                if let Ok(client_etag) = client_etag_res
                    && (client_etag == etag || client_etag == "*")
                {
                    debug!("returning 304 Not Modified for ETag match");
                    return Ok(Response::builder()
                        .status(StatusCode::NOT_MODIFIED)
                        .header(header::ETAG, etag)
                        .body(Body::empty())
                        .unwrap());
                }
            }

            // Check If-Modified-Since
            if let Some(if_modified_since) = headers.get(header::IF_MODIFIED_SINCE) {
                let client_time_res: Result<&str, ToStrError> = if_modified_since.to_str();
                if let Ok(client_time_str) = client_time_res
                    && let Some(file_time) = modified_time
                {
                    // Simple time comparison (not parsing HTTP date for simplicity)
                    let file_timestamp: u64 = file_time
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
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(SystemTime::UNIX_EPOCH)
        {
            let timestamp = duration.as_secs();
            let http_date = format_http_date(timestamp);
            response_builder = response_builder.header(header::LAST_MODIFIED, http_date);
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
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(SystemTime::UNIX_EPOCH)
        {
            let timestamp = duration.as_secs();
            let http_date = format_http_date(timestamp);
            response_builder = response_builder.header(header::LAST_MODIFIED, http_date);
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

        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(SystemTime::UNIX_EPOCH)
        {
            duration.as_secs().hash(&mut hasher);
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

/// Handle requests to the root path "/"
pub async fn serve_root_handler(serve_dir: PathBuf, serve_index: bool) -> Response<Body> {
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
pub async fn serve_file_handler(
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
            if let Some(parent) = dir_path.parent()
                && parent != dir_path
            {
                html.push_str("<li><a href=\"../\" class=\"dir\">📁 ../</a></li>");
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
pub fn create_error_response(status: StatusCode, message: &str) -> Response<Body> {
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
pub fn add_cors_headers(headers: &mut HeaderMap) {
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
