//! Turning a pattern into the filenames it names: brace expansion, splitting a
//! pattern into its fixed root and its wildcard tail, the glob walk itself, and
//! the escaping that keeps text from a quote or a variable value literal.
use std::path::{Path, PathBuf};
use tracing::debug;

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
pub(crate) fn expand_glob_pattern(pattern: &str, current_dir: &Path) -> Vec<String> {
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
                let paths = filter_and_sort_glob_hits(&glob, &effective_root, paths);
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
                        let mut display_string = display_path.display().to_string();
                        // globmatch normalizes `./` away; put back the prefix
                        // the user typed so `./*.txt` expands like bash does.
                        if pat.starts_with("./") && !display_string.starts_with("./") {
                            display_string.insert_str(0, "./");
                        }
                        out.push(display_string);
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

/// Whether a single glob path component explicitly matches a leading dot,
/// so hidden filenames at that level stay eligible (POSIX: a leading `.`
/// must be matched by a literal `.`, not by `*` / `?` / `[...]`).
fn glob_component_allows_hidden(component: &str) -> bool {
    component.starts_with('.')
}

/// Split a glob tail into path components for hidden-file checks.
///
/// Empty segments (from a trailing `/`) and `.` segments (from a `./`
/// prefix) carry no name to match, so they are dropped: without this,
/// `./*.txt` would check the hit against `"."` at depth 0 and wrongly
/// allow hidden files.
fn glob_check_components(glob: &str) -> Vec<&str> {
    glob.split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect()
}

/// Split a path relative to the glob root into comparable names.
///
/// `CurDir` (`.`), root and prefix components carry no name; `..` is kept
/// so a `..` hit never silently aligns with the wrong pattern level.
fn rel_check_parts(rel: &Path) -> Vec<String> {
    rel.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            std::path::Component::ParentDir => Some("..".to_string()),
            _ => None,
        })
        .collect()
}

/// Whether the hit at `depth` may be hidden under the pattern components.
fn hit_level_allows_hidden(components: &[&str], has_doublestar: bool, depth: usize) -> bool {
    if has_doublestar {
        components.iter().any(|c| glob_component_allows_hidden(c))
    } else if depth < components.len() {
        glob_component_allows_hidden(components[depth])
    } else {
        false
    }
}
/// Filter raw globmatch hits to POSIX pathname-expansion semantics and
/// sort them deterministically.
///
/// Drops the root itself (globset `*` matches the empty relative path),
/// drops hidden entries unless the corresponding pattern component
/// explicitly starts with a dot, and sorts so output does not depend on
/// filesystem iteration order.
fn filter_and_sort_glob_hits(
    glob: &str,
    effective_root: &Path,
    paths: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let components = glob_check_components(glob);
    let has_doublestar = components.iter().any(|c| c.contains("**"));
    let mut kept = Vec::with_capacity(paths.len());
    for path in paths {
        if path == effective_root {
            continue;
        }
        match path.strip_prefix(effective_root) {
            Ok(rel) if !rel.as_os_str().is_empty() => {
                let rel_parts = rel_check_parts(rel);
                if rel_parts.is_empty() {
                    continue;
                }
                let mut excluded = false;
                for (depth, name) in rel_parts.iter().enumerate() {
                    if !name.starts_with('.') {
                        continue;
                    }
                    if !hit_level_allows_hidden(&components, has_doublestar, depth) {
                        excluded = true;
                        break;
                    }
                }
                if excluded {
                    continue;
                }
            }
            // Outside the root (`..` patterns resolve above it): keep the
            // pre-existing expansion, but still hide dot basenames the
            // pattern does not explicitly request.
            _ => {
                let hidden_basename = path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'));
                if hidden_basename
                    && !components
                        .last()
                        .is_some_and(|c| glob_component_allows_hidden(c))
                {
                    continue;
                }
            }
        }
        kept.push(path);
    }
    kept.sort();
    kept
}

/// Escape the characters that would otherwise start matching files.
///
/// Applied to text that came from a quote or a protected expansion: those
/// are literals, even when another part of the same word is a real pattern.
/// Unquoted parameter and command-substitution results are *not* escaped
/// here; they stay eligible for pathname expansion (see
/// [`escape_brace_metacharacters`]).
pub(crate) fn escape_glob_metacharacters(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape brace metacharacters while leaving glob metacharacters active.
///
/// Applied to unquoted dynamic values (parameter expansion, command
/// substitution) after field splitting: `* ? [` stay live for pathname
/// expansion, but `{ }` and `\` are escaped so a value like `{a,b}` never
/// retroactively activates brace expansion. Quoted values use
/// [`escape_glob_metacharacters`] instead and stay fully literal.
pub(crate) fn escape_brace_metacharacters(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | '{' | '}') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Undo [`escape_glob_metacharacters`].
pub(crate) fn unescape_glob_metacharacters(value: &str) -> String {
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
