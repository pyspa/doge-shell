use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use getopts::Options;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Built-in dmv command description
pub fn description() -> &'static str {
    "Rename files with your editor"
}

/// Built-in dmv command implementation
/// Provides batch file renaming functionality similar to zsh's zmv
///
/// Usage: dmv [options] pattern replacement
/// Options:
///   -n, --dry-run    Show what would be renamed without actually doing it
///   -v, --verbose    Show detailed output
///   -f, --force      Overwrite existing files
///   -h, --help       Show help message
pub fn command(ctx: &Context, argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    let mut opts = Options::new();
    opts.optflag(
        "n",
        "dry-run",
        "Show what would be renamed without actually doing it",
    );
    opts.optflag("v", "verbose", "Show detailed output");
    opts.optflag("f", "force", "Overwrite existing files");
    opts.optflag("h", "help", "Show help message");

    let matches = match opts.parse(&argv[1..]) {
        Ok(m) => m,
        Err(e) => {
            ctx.write_stderr(&format!("dmv: {e}")).ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    if matches.opt_present("h") {
        show_help(ctx);
        return ExitStatus::ExitedWith(0);
    }

    if matches.free.len() != 2 {
        ctx.write_stderr("dmv: exactly two arguments required: pattern and replacement")
            .ok();
        ctx.write_stderr("Usage: dmv [options] pattern replacement")
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    let pattern = &matches.free[0];
    let replacement = &matches.free[1];
    let dry_run = matches.opt_present("n");
    let verbose = matches.opt_present("v") || dry_run;
    let force = matches.opt_present("f");

    match execute_dmv(ctx, pattern, replacement, dry_run, verbose, force) {
        Ok(count) => {
            if verbose {
                if dry_run {
                    ctx.write_stdout(&format!("Would rename {count} files"))
                        .ok();
                } else {
                    ctx.write_stdout(&format!("Renamed {count} files")).ok();
                }
            }
            ExitStatus::ExitedWith(0)
        }
        Err(e) => {
            ctx.write_stderr(&format!("dmv: {e}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

fn show_help(ctx: &Context) {
    let help_text = r#"dmv - batch file renaming utility

Usage: dmv [options] pattern replacement

Options:
  -n, --dry-run    Show what would be renamed without actually doing it
  -v, --verbose    Show detailed output
  -f, --force      Overwrite existing files
  -h, --help       Show this help message

Pattern Syntax:
  Use glob patterns with capture groups in parentheses
  * matches any characters except /
  ** matches any characters including /
  ? matches any single character
  [abc] matches any character in the set
  
Replacement Syntax:
  Use $1, $2, etc. to reference capture groups from the pattern
  Use $0 to reference the entire matched filename

Examples:
  dmv '*.txt' '*.bak'           # Rename all .txt files to .bak
  dmv '(*).txt' '$1.backup'     # Add .backup extension to .txt files
  dmv 'IMG_(*).jpg' 'photo_$1.jpg'  # Rename IMG_*.jpg to photo_*.jpg
  dmv -n '*.log' 'old_*.log'    # Dry run: show what would be renamed
"#;
    ctx.write_stdout(help_text).ok();
}

fn execute_dmv(
    ctx: &Context,
    pattern: &str,
    replacement: &str,
    dry_run: bool,
    verbose: bool,
    force: bool,
) -> Result<usize, String> {
    // Parse the pattern to extract glob pattern and capture groups
    let (glob_pattern, capture_regex) = parse_pattern(pattern)?;

    // Find all matching files
    let matches = find_matching_files(&glob_pattern)?;

    if matches.is_empty() {
        if verbose {
            ctx.write_stdout("No files match the pattern").ok();
        }
        return Ok(0);
    }

    // Generate rename operations
    let mut rename_ops = Vec::new();
    for file_path in matches {
        if let Some(new_name) = generate_replacement(&file_path, &capture_regex, replacement)? {
            rename_ops.push((file_path, new_name));
        }
    }

    // Check for conflicts
    check_rename_conflicts(&rename_ops, force)?;

    // Execute or preview renames
    let mut count = 0;
    for (old_path, new_path) in rename_ops {
        if verbose {
            if dry_run {
                ctx.write_stdout(&format!(
                    "Would rename: {} -> {}",
                    old_path.display(),
                    new_path.display()
                ))
                .ok();
            } else {
                ctx.write_stdout(&format!(
                    "Renaming: {} -> {}",
                    old_path.display(),
                    new_path.display()
                ))
                .ok();
            }
        }

        if !dry_run {
            // Create parent directory if it doesn't exist
            if let Some(parent) = new_path.parent()
                && !parent.exists()
            {
                fs::create_dir_all(parent).map_err(|e| {
                    format!("Failed to create directory {}: {}", parent.display(), e)
                })?;
            }

            // Perform the rename
            fs::rename(&old_path, &new_path).map_err(|e| {
                format!(
                    "Failed to rename {} to {}: {}",
                    old_path.display(),
                    new_path.display(),
                    e
                )
            })?;
        }
        count += 1;
    }

    Ok(count)
}

fn parse_pattern(pattern: &str) -> Result<(String, Regex), String> {
    let mut glob_pattern = String::new();
    let mut regex_pattern = String::from("^");
    let mut chars = pattern.chars().peekable();
    let mut _capture_count = 0;

    while let Some(ch) = chars.next() {
        match ch {
            '(' => {
                // Start of capture group
                _capture_count += 1;
                glob_pattern.push('*');
                regex_pattern.push('(');

                // Find the matching closing parenthesis
                let mut paren_count = 1;
                let mut capture_content = String::new();

                for inner_ch in chars.by_ref() {
                    if inner_ch == '(' {
                        paren_count += 1;
                    } else if inner_ch == ')' {
                        paren_count -= 1;
                        if paren_count == 0 {
                            break;
                        }
                    }
                    capture_content.push(inner_ch);
                }

                if paren_count != 0 {
                    return Err("Unmatched parentheses in pattern".to_string());
                }

                // Convert glob pattern inside parentheses to regex
                let inner_regex = glob_to_regex(&capture_content)?;
                regex_pattern.push_str(&inner_regex);
                regex_pattern.push(')');
            }
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next(); // consume second *
                    glob_pattern.push_str("**");
                    regex_pattern.push_str(".*");
                } else {
                    glob_pattern.push('*');
                    regex_pattern.push_str("[^/]*");
                }
            }
            '?' => {
                glob_pattern.push('?');
                regex_pattern.push_str("[^/]");
            }
            '[' => {
                glob_pattern.push('[');
                regex_pattern.push('[');

                // Copy character class
                for class_ch in chars.by_ref() {
                    glob_pattern.push(class_ch);
                    regex_pattern.push(class_ch);
                    if class_ch == ']' {
                        break;
                    }
                }
            }
            '.' | '^' | '$' | '+' | '{' | '}' | '|' | '\\' => {
                // Escape regex special characters
                glob_pattern.push(ch);
                regex_pattern.push('\\');
                regex_pattern.push(ch);
            }
            _ => {
                glob_pattern.push(ch);
                regex_pattern.push(ch);
            }
        }
    }

    regex_pattern.push('$');

    let regex = Regex::new(&regex_pattern).map_err(|e| format!("Invalid pattern: {e}"))?;

    Ok((glob_pattern, regex))
}

fn glob_to_regex(glob: &str) -> Result<String, String> {
    let mut regex = String::new();
    let mut chars = glob.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    regex.push_str(".*");
                } else {
                    regex.push_str("[^/]*");
                }
            }
            '?' => regex.push_str("[^/]"),
            '[' => {
                regex.push('[');
                for class_ch in chars.by_ref() {
                    regex.push(class_ch);
                    if class_ch == ']' {
                        break;
                    }
                }
            }
            '.' | '^' | '$' | '+' | '{' | '}' | '|' | '\\' | '(' | ')' => {
                regex.push('\\');
                regex.push(ch);
            }
            _ => regex.push(ch),
        }
    }

    Ok(regex)
}

fn find_matching_files(pattern: &str) -> Result<Vec<PathBuf>, String> {
    // Simple glob implementation
    let current_dir =
        std::env::current_dir().map_err(|e| format!("Failed to get current directory: {e}"))?;

    let mut matches = Vec::new();
    find_matches_recursive(&current_dir, pattern, &mut matches)?;

    Ok(matches)
}

fn find_matches_recursive(
    dir: &Path,
    pattern: &str,
    matches: &mut Vec<PathBuf>,
) -> Result<(), String> {
    if let Some(sub_pattern) = pattern.strip_prefix("**/") {
        // Recursive pattern
        find_matches_in_dir(dir, sub_pattern, matches)?;

        // Recursively search subdirectories
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if let Ok(file_type) = entry.file_type()
                    && file_type.is_dir()
                {
                    find_matches_recursive(&entry.path(), pattern, matches)?;
                }
            }
        }
    } else {
        find_matches_in_dir(dir, pattern, matches)?;
    }

    Ok(())
}

fn find_matches_in_dir(
    dir: &Path,
    pattern: &str,
    matches: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = fs::read_dir(dir)
        .map_err(|e| format!("Failed to read directory {}: {}", dir.display(), e))?;

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let file_name_str = file_name.to_string_lossy();

        if matches_pattern(&file_name_str, pattern) {
            matches.push(entry.path());
        }
    }

    Ok(())
}

fn matches_pattern(filename: &str, pattern: &str) -> bool {
    // Simple pattern matching implementation
    pattern_match(filename, pattern)
}

fn pattern_match(text: &str, pattern: &str) -> bool {
    pattern_match_impl(text, pattern)
}

fn pattern_match_impl(text: &str, pattern: &str) -> bool {
    let mut text_chars = text.chars();
    let mut pattern_chars = pattern.chars().peekable();
    let mut text_pos = 0;

    while let Some(&p) = pattern_chars.peek() {
        match p {
            '*' => {
                pattern_chars.next();
                if pattern_chars.peek().is_none() {
                    return true; // * at end matches everything
                }

                // Try matching at each remaining position in text
                let remaining_text = &text[text_pos..];
                let remaining_pattern: String = pattern_chars.collect();

                for i in 0..=remaining_text.len() {
                    if pattern_match_impl(&remaining_text[i..], &remaining_pattern) {
                        return true;
                    }
                }
                return false;
            }
            '?' => {
                pattern_chars.next();
                if let Some(ch) = text_chars.next() {
                    text_pos += ch.len_utf8();
                } else {
                    return false;
                }
            }
            c => {
                pattern_chars.next();
                if let Some(text_ch) = text_chars.next() {
                    if text_ch != c {
                        return false;
                    }
                    text_pos += text_ch.len_utf8();
                } else {
                    return false;
                }
            }
        }
    }

    text_chars.next().is_none()
}

fn generate_replacement(
    file_path: &Path,
    capture_regex: &Regex,
    replacement: &str,
) -> Result<Option<PathBuf>, String> {
    let file_name = file_path
        .file_name()
        .ok_or("Invalid file path")?
        .to_string_lossy();

    let captures = match capture_regex.captures(&file_name) {
        Some(caps) => caps,
        None => return Ok(None), // No match
    };

    let mut result = replacement.to_string();

    // Replace $0 with full match
    result = result.replace("$0", &captures[0]);

    // Replace $1, $2, etc. with capture groups
    for i in 1..captures.len() {
        let placeholder = format!("${i}");
        if let Some(capture) = captures.get(i) {
            result = result.replace(&placeholder, capture.as_str());
        }
    }

    // Construct new path
    let new_path = if let Some(parent) = file_path.parent() {
        parent.join(result)
    } else {
        PathBuf::from(result)
    };

    Ok(Some(new_path))
}

fn check_rename_conflicts(rename_ops: &[(PathBuf, PathBuf)], force: bool) -> Result<(), String> {
    let mut target_files: HashMap<PathBuf, PathBuf> = HashMap::new();

    for (source, target) in rename_ops {
        // Check if target already exists
        if target.exists() && !force {
            return Err(format!(
                "Target file already exists: {} (use -f to force)",
                target.display()
            ));
        }

        // Check for duplicate targets
        if let Some(existing_source) = target_files.get(target) {
            return Err(format!(
                "Multiple files would be renamed to {}: {} and {}",
                target.display(),
                existing_source.display(),
                source.display()
            ));
        }

        target_files.insert(target.clone(), source.clone());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pattern() {
        let (glob, regex) = parse_pattern("(*).txt").unwrap();
        assert_eq!(glob, "*.txt");
        assert!(regex.is_match("test.txt"));
        assert!(!regex.is_match("test.doc"));

        let captures = regex.captures("hello.txt").unwrap();
        assert_eq!(&captures[1], "hello");
    }

    #[test]
    fn test_pattern_match() {
        assert!(pattern_match("test.txt", "*.txt"));
        assert!(pattern_match("hello", "*"));
        assert!(pattern_match("a", "?"));
        assert!(!pattern_match("ab", "?"));
        assert!(pattern_match("test", "t*t"));
    }

    #[test]
    fn test_generate_replacement() {
        let regex = Regex::new(r"^(.*)\.txt$").unwrap();
        let path = PathBuf::from("test.txt");

        let result = generate_replacement(&path, &regex, "$1.bak").unwrap();
        assert_eq!(result, Some(PathBuf::from("test.bak")));
    }

    #[test]
    fn test_parse_pattern_complex() {
        // Test recursive pattern
        let (glob, regex) = parse_pattern("(**/)(*).jpg").unwrap();
        assert_eq!(glob, "**.jpg");
        assert!(regex.is_match("dir/subdir/photo.jpg"));

        let captures = regex.captures("path/to/image.jpg").unwrap();
        assert_eq!(&captures[1], "path/to/");
        assert_eq!(&captures[2], "image");
    }

    #[test]
    fn test_parse_pattern_errors() {
        // Test unmatched parentheses
        assert!(parse_pattern("(*.txt").is_err());
        assert!(parse_pattern("*.txt)").is_err());
    }

    #[test]
    fn test_pattern_match_advanced() {
        // Test simple patterns (character classes are not fully implemented)
        assert!(pattern_match("test1.txt", "test?.txt"));
        assert!(!pattern_match("test.txt", "test?.txt"));

        // Test multiple wildcards
        assert!(pattern_match(
            "prefix_middle_suffix",
            "prefix*middle*suffix"
        ));
        assert!(!pattern_match("prefix_suffix", "prefix*middle*suffix"));
    }

    #[test]
    fn test_generate_replacement_multiple_captures() {
        let regex = Regex::new(r"^(.*)_(.*)\.txt$").unwrap();
        let path = PathBuf::from("prefix_suffix.txt");

        let result = generate_replacement(&path, &regex, "$2_$1.bak").unwrap();
        assert_eq!(result, Some(PathBuf::from("suffix_prefix.bak")));
    }

    #[test]
    fn test_generate_replacement_no_match() {
        let regex = Regex::new(r"^(.*)\.txt$").unwrap();
        let path = PathBuf::from("test.doc");

        let result = generate_replacement(&path, &regex, "$1.bak").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_check_rename_conflicts() {
        let operations = vec![
            (PathBuf::from("file1.txt"), PathBuf::from("file1.bak")),
            (PathBuf::from("file2.txt"), PathBuf::from("file2.bak")),
        ];

        // Should pass without conflicts
        assert!(check_rename_conflicts(&operations, false).is_ok());
    }

    #[test]
    fn test_check_rename_conflicts_duplicate_targets() {
        let operations = vec![
            (PathBuf::from("file1.txt"), PathBuf::from("same.bak")),
            (PathBuf::from("file2.txt"), PathBuf::from("same.bak")),
        ];

        // Should fail due to duplicate targets
        assert!(check_rename_conflicts(&operations, false).is_err());
    }

    #[test]
    fn test_glob_to_regex() {
        assert_eq!(glob_to_regex("*.txt").unwrap(), "[^/]*\\.txt");
        assert_eq!(glob_to_regex("test?.log").unwrap(), "test[^/]\\.log");
        assert_eq!(glob_to_regex("**/*.jpg").unwrap(), ".*/[^/]*\\.jpg");
        assert_eq!(glob_to_regex("[abc].txt").unwrap(), "[abc]\\.txt");
    }
}
