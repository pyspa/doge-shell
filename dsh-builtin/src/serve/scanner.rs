use super::error::ServeError;
use chrono::{DateTime, Utc};
use std::fs;
use std::path::Path;
use std::time::SystemTime;
use tracing::debug;

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
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && name.starts_with('.')
            {
                continue;
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
