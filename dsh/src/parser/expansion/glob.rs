//! Turning a pattern into the filenames it names: brace expansion, splitting a
//! pattern into its fixed root and its wildcard tail, the glob walk itself, and
//! the escaping that keeps text from a quote or a variable value literal.
use super::*;

fn find_glob_root(path: &str) -> (String, String) {
    let mut root = Vec::new();
    let mut glob = Vec::new();
    let mut find_glob = false;
    let path = Path::new(path);
    if path.is_relative() {
        return (".".to_string(), path.to_string_lossy().to_string());
    }
    for p in path.iter() {
        let file = p.to_string_lossy();
        if !find_glob && (file.contains("*") || file.contains("?") || file.contains("[")) {
            find_glob = true;
        }
        if find_glob {
            glob.push(file.to_string());
        } else {
            root.push(file.to_string());
        }
    }

    let mut root = root.join(std::path::MAIN_SEPARATOR_STR);
    let mut glob = glob.join(std::path::MAIN_SEPARATOR_STR);
    if Path::new(&glob).is_absolute() {
        glob = glob[1..].to_string();
    }

    if root.is_empty() {
        (".".to_string(), glob.to_string())
    } else {
        if root.starts_with("//") {
            root = root[1..].to_string();
        }
        (root.to_string(), glob.to_string())
    }
}

pub(crate) fn expand_braces(pattern: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut stack = Vec::new();
    let mut starts = Vec::new();
    let mut i = 0;
    let chars: Vec<char> = pattern.chars().collect();

    while i < chars.len() {
        if chars[i] == '{' && (i == 0 || chars[i - 1] != '\\') {
            stack.push(i);
            starts.push(i);
        } else if chars[i] == '}'
            && (i == 0 || chars[i - 1] != '\\')
            && let Some(start) = stack.pop()
            && stack.is_empty()
        {
            // Found outermost brace pair
            let prefix: String = chars[0..start].iter().collect();
            let suffix: String = chars[i + 1..].iter().collect();
            // Split content by comma, respecting nested braces
            let mut parts = Vec::new();
            let mut current_part = String::new();
            let mut depth = 0;
            let content_slice = &chars[start + 1..i];
            let mut j = 0;
            while j < content_slice.len() {
                let c = content_slice[j];
                if c == '{' && (j == 0 || content_slice[j - 1] != '\\') {
                    depth += 1;
                    current_part.push(c);
                } else if c == '}' && (j == 0 || content_slice[j - 1] != '\\') {
                    depth -= 1;
                    current_part.push(c);
                } else if c == ',' && depth == 0 && (j == 0 || content_slice[j - 1] != '\\') {
                    parts.push(current_part.clone());
                    current_part.clear();
                } else {
                    current_part.push(c);
                }
                j += 1;
            }
            parts.push(current_part);

            for part in parts {
                let new_pattern = format!("{}{}{}", prefix, part, suffix);
                result.extend(expand_braces(&new_pattern));
            }
            return result;
        }
        i += 1;
    }

    // No top-level braces found to expand
    vec![pattern.to_string()]
}

/// Expand a pattern that may contain brace and glob metacharacters.
///
/// Returns raw, unquoted results; the caller decides how to escape them. A
/// pattern that matches nothing comes back as itself, which is what the shell
/// has always done here.
pub(super) fn expand_glob_pattern(pattern: &str, current_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for pat in expand_braces(pattern) {
        if !(pat.contains('*') || pat.contains('?') || pat.contains('[')) {
            out.push(pat);
            continue;
        }

        let (root, glob) = find_glob_root(&pat);
        debug!("glob pattern: root:{} {:?} ", root, glob);

        let effective_root = if Path::new(&root).is_absolute() {
            PathBuf::from(&root)
        } else {
            current_dir.join(&root)
        };

        match globmatch::Builder::new(&glob).build(&effective_root) {
            Ok(builder) => {
                let paths: Vec<_> = builder.into_iter().flatten().collect();
                if paths.is_empty() {
                    debug!("dsh: no matches for wildcard '{}'", &glob);
                    out.push(pat);
                } else {
                    for path in paths {
                        debug!("glob match {}", path.display());
                        // Relative patterns stay relative so the argv the user
                        // sees matches what they typed.
                        let display_path = if Path::new(&root).is_relative() {
                            path.strip_prefix(current_dir)
                                .unwrap_or(&path)
                                .to_path_buf()
                        } else {
                            path
                        };
                        out.push(display_path.display().to_string());
                    }
                }
            }
            Err(err) => {
                debug!("dsh: failed resolve paths. {}. treating as literal.", err);
                out.push(pat);
            }
        }
    }
    out
}

/// Escape the characters that would otherwise start matching files.
///
/// Applied to text that came from a quote or a variable value: those are
/// literals, even when another part of the same word is a real pattern.
pub(super) fn escape_glob_metacharacters(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Undo [`escape_glob_metacharacters`].
pub(super) fn unescape_glob_metacharacters(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(next) = chars.next()
        {
            if !matches!(next, '*' | '?' | '[' | ']' | '{' | '}' | '\\') {
                out.push('\\');
            }
            out.push(next);
            continue;
        }
        out.push(c);
    }
    out
}
