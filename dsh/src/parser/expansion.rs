use super::{Rule, ShellParser, ast::get_string};
use crate::environment::Environment;
use anyhow::{Result, anyhow};
use parking_lot::RwLock;
use pest::Parser;
use pest::iterators::Pair;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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

fn expand_braces(pattern: &str) -> Vec<String> {
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
            let content: String = chars[start + 1..i].iter().collect(); // content inside {}

            // Split content by comma, respecting nested braces
            let mut parts = Vec::new();
            let mut current_part = String::new();
            let mut depth = 0;
            let content_chars: Vec<char> = content.chars().collect();
            let mut j = 0;
            while j < content_chars.len() {
                let c = content_chars[j];
                if c == '{' && (j == 0 || content_chars[j - 1] != '\\') {
                    depth += 1;
                    current_part.push(c);
                } else if c == '}' && (j == 0 || content_chars[j - 1] != '\\') {
                    depth -= 1;
                    current_part.push(c);
                } else if c == ',' && depth == 0 && (j == 0 || content_chars[j - 1] != '\\') {
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

pub fn expand_alias_tilde(
    pair: Pair<Rule>,
    alias: &HashMap<String, String>,
    _current_dir: &PathBuf,
) -> Result<Vec<String>> {
    let mut argv: Vec<String> = vec![];

    match pair.as_rule() {
        Rule::glob_word | Rule::brace_word => {
            let pattern = shellexpand::tilde(pair.as_str()).to_string();
            // First expand braces
            let expanded_patterns = expand_braces(&pattern);

            for pat in expanded_patterns {
                // Check if the expanded pattern actually contains glob characters
                // Note: expand_braces handles brace expansion, so we check for glob chars in the result
                if pat.contains('*') || pat.contains('?') || pat.contains('[') {
                    let (root, pattern) = find_glob_root(&pat);
                    debug!("glob pattern: root:{} {:?} ", root, pattern);
                    match globmatch::Builder::new(&pattern).build(root) {
                        Ok(builder) => {
                            let paths: Vec<_> = builder.into_iter().flatten().collect();
                            if paths.is_empty() {
                                debug!("dsh: no matches for wildcard '{}'", &pattern);
                                argv.push(pat);
                            } else {
                                for path in paths {
                                    debug!("glob match {}", path.display());
                                    argv.push(format!("\"{}\"", path.display()));
                                }
                            }
                        }
                        Err(err) => {
                            debug!("dsh: failed resolve paths. {}. treating as literal.", err);
                            argv.push(pat);
                        }
                    }
                } else {
                    // No glob chars, just push the brace-expanded string
                    argv.push(pat);
                }
            }
        }
        Rule::word
        | Rule::variable
        | Rule::s_quoted
        | Rule::d_quoted
        | Rule::literal_s_quoted
        | Rule::literal_d_quoted
        | Rule::stdout_redirect_direction
        | Rule::stderr_redirect_direction
        | Rule::stdouterr_redirect_direction
        | Rule::stdin_redirect_direction
        | Rule::stdin_redirect_direction_in
        | Rule::command_subst => {
            argv.push(shellexpand::tilde(pair.as_str()).to_string());
        }
        Rule::argv0 => {
            for inner_pair in pair.into_inner() {
                let v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                for (i, arg) in v.iter().enumerate() {
                    if i == 0 {
                        if let Some(val) = alias.get(arg) {
                            debug!("alias '{arg}' => '{val}'");
                            argv.push(val.trim().to_string());
                        } else {
                            argv.push(arg.trim().to_string());
                        }
                    } else {
                        argv.push(arg.trim().to_string());
                    }
                }
            }
        }
        Rule::pipe_command => {
            debug!("expand pipe_command {}", pair.as_str());
            // Pipe character is added by expand_alias function, so don't add it here
            for inner_pair in pair.into_inner() {
                let mut v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                argv.append(&mut v);
            }
        }
        Rule::redirect => {
            for inner_pair in pair.into_inner() {
                let mut v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                argv.append(&mut v);
            }
        }
        _ => {
            debug!("@expand: {:?} : {:?}", pair.as_rule(), pair.as_str());
            for inner_pair in pair.into_inner() {
                match inner_pair.as_rule() {
                    Rule::simple_command_bg => {
                        for inner_pair in inner_pair.into_inner() {
                            if inner_pair.as_rule() == Rule::background_op {
                                argv.push(inner_pair.as_str().to_string());
                            } else {
                                let mut v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                                argv.append(&mut v);
                            }
                        }
                    }
                    Rule::proc_subst => {
                        debug!("expand proc_subst {}", inner_pair.as_str());
                        argv.push("<(".to_string());
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                            argv.append(&mut v);
                        }
                        argv.push(")".to_string());
                    }
                    Rule::command_subst => {
                        debug!("expand command_subst {}", inner_pair.as_str());
                        argv.push(inner_pair.as_str().to_string());
                    }
                    Rule::subshell => {
                        debug!("expand subshell {}", inner_pair.as_str());
                        argv.push("(".to_string());
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                            argv.append(&mut v);
                        }
                        argv.push(")".to_string());
                    }
                    Rule::argv0 => {
                        for inner_pair in inner_pair.into_inner() {
                            let v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                            for (i, arg) in v.iter().enumerate() {
                                if i == 0 {
                                    if let Some(val) = alias.get(arg) {
                                        debug!("alias '{arg}' => '{val}'");
                                        argv.push(val.trim().to_string());
                                    } else {
                                        argv.push(arg.trim().to_string());
                                    }
                                } else {
                                    argv.push(arg.trim().to_string());
                                }
                            }
                        }
                    }
                    Rule::pipe_command => {
                        for inner_pair in inner_pair.into_inner() {
                            if inner_pair.as_rule() == Rule::pipeline_op {
                                argv.push(inner_pair.as_str().to_string());
                            } else {
                                let mut v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                                argv.append(&mut v);
                            }
                        }
                    }
                    Rule::commands
                    | Rule::command
                    | Rule::simple_command
                    | Rule::args
                    | Rule::span => {
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                            argv.append(&mut v);
                        }
                    }
                    Rule::word
                    | Rule::glob_word
                    | Rule::brace_word
                    | Rule::variable
                    | Rule::s_quoted
                    | Rule::d_quoted
                    | Rule::literal_s_quoted
                    | Rule::literal_d_quoted
                    | Rule::proc_subst_direction_in
                    | Rule::stdout_redirect_direction
                    | Rule::stderr_redirect_direction
                    | Rule::stdouterr_redirect_direction => {
                        let mut v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                        argv.append(&mut v);
                    }
                    _ => {
                        debug!(
                            "expand_alias_tilde missing {:?} {:?}",
                            inner_pair.as_rule(),
                            inner_pair.as_str()
                        );
                    }
                }
            }
        }
    }
    Ok(argv)
}

#[allow(dead_code)]
pub fn expand_alias(input: String, environment: Arc<RwLock<Environment>>) -> Result<String> {
    let (cow, _) = parse_with_expansion(&input, environment)?;
    Ok(cow.into_owned())
}

pub fn parse_with_expansion<'a>(
    input: &'a str,
    environment: Arc<RwLock<Environment>>,
) -> Result<(
    std::borrow::Cow<'a, str>,
    Option<pest::iterators::Pairs<'a, Rule>>,
)> {
    let pairs = ShellParser::parse(Rule::commands, input).map_err(|e| anyhow!(e))?;

    // Check if expansion is needed
    let mut needs_expansion = false;
    {
        let env_read = environment.read();

        // We iterate over a clone of pairs to check for expansion triggers
        // This is cheaper than re-parsing if we can avoid expansion
        for pair in pairs.clone() {
            if check_expansion_needed(pair, &env_read.alias) {
                needs_expansion = true;
                break;
            }
        }
    }

    if !needs_expansion {
        return Ok((std::borrow::Cow::Borrowed(input), Some(pairs)));
    }

    // If expansion is needed, we fall back to the full expansion logic
    // We can reuse the pairs we already parsed for the first step of expansion
    // but expand_alias implementation currently re-parses.
    // To avoid changing expand_alias logic too much and risking bugs, we just call it.
    // Ideally expand_alias should take pairs as input.

    // For now, let's just call expand_alias which returns a String
    let expanded = expand_alias_from_pairs(pairs, environment)?;
    Ok((std::borrow::Cow::Owned(expanded), None))
}

fn check_expansion_needed(pair: Pair<Rule>, alias: &HashMap<String, String>) -> bool {
    match pair.as_rule() {
        Rule::glob_word | Rule::brace_word => {
            let s = pair.as_str();
            s.contains('*')
                || s.contains('?')
                || s.contains('[')
                || s.contains('~')
                || s.contains('$')
                || s.contains('{')
        }
        Rule::word | Rule::variable | Rule::s_quoted | Rule::d_quoted => {
            let s = pair.as_str();
            s.contains('~') || s.contains('$')
        }
        Rule::argv0 => {
            for inner in pair.into_inner() {
                if check_expansion_needed(inner.clone(), alias) {
                    return true;
                }
                // Check alias for the first word
                // This is a simplification; a more robust check would mirror expand_alias_tilde
                // But for argv0, we primarily care if the command itself is an alias
                if let Some(cmd) = get_string(inner)
                    && alias.contains_key(&cmd)
                {
                    return true;
                }
            }
            false
        }
        Rule::commands | Rule::command | Rule::simple_command | Rule::args => {
            for inner in pair.into_inner() {
                if check_expansion_needed(inner, alias) {
                    return true;
                }
            }
            false
        }
        _ => {
            // Recurse for other rules
            for inner in pair.into_inner() {
                if check_expansion_needed(inner, alias) {
                    return true;
                }
            }
            false
        }
    }
}

pub fn expand_alias_from_pairs(
    pairs: pest::iterators::Pairs<Rule>,
    environment: Arc<RwLock<Environment>>,
) -> Result<String> {
    let mut buf: Vec<String> = Vec::new();
    let current_dir = std::env::current_dir()?;
    for pair in pairs {
        for pair in pair.into_inner() {
            let mut commands = expand_command_alias(pair, Arc::clone(&environment), &current_dir)?;
            buf.append(&mut commands);
        }
    }
    Ok(buf.join(" "))
}

fn expand_command_alias(
    pair: Pair<Rule>,
    environment: Arc<RwLock<Environment>>,
    _current_dir: &PathBuf,
) -> Result<Vec<String>> {
    let mut buf: Vec<String> = Vec::new();

    if let Rule::command = pair.as_rule() {
        for inner_pair in pair.into_inner() {
            match inner_pair.as_rule() {
                Rule::simple_command => {
                    let args =
                        expand_alias_tilde(inner_pair, &environment.read().alias, _current_dir)?;
                    for arg in args {
                        if let Some(val) = environment.read().get_var(&arg) {
                            if val.is_empty() {
                                buf.push("\"\"".to_string());
                            } else {
                                // Quote multiline or whitespace-containing values
                                let needs_quote =
                                    val.contains('\n') || val.contains(' ') || val.contains('\t');
                                if needs_quote {
                                    // Escape existing double quotes and wrap
                                    let escaped = val.replace('"', r#"\""#);
                                    buf.push(format!("\"{}\"", escaped));
                                } else {
                                    buf.push(val.trim().to_string());
                                }
                            }
                        } else {
                            buf.push(arg);
                        }
                    }
                }
                Rule::simple_command_bg => {
                    let args =
                        expand_alias_tilde(inner_pair, &environment.read().alias, _current_dir)?;
                    for arg in args {
                        if let Some(val) = environment.read().get_var(&arg) {
                            if val.is_empty() {
                                buf.push("\"\"".to_string());
                            } else {
                                let needs_quote =
                                    val.contains('\n') || val.contains(' ') || val.contains('\t');
                                if needs_quote {
                                    let escaped = val.replace('"', r#"\""#);
                                    buf.push(format!("\"{}\"", escaped));
                                } else {
                                    buf.push(val.trim().to_string());
                                }
                            }
                        } else {
                            buf.push(arg);
                        }
                    }
                    buf.push("&".to_string());
                }
                Rule::pipe_command => {
                    buf.push("|".to_string());
                    let args =
                        expand_alias_tilde(inner_pair, &environment.read().alias, _current_dir)?;
                    for arg in args {
                        if let Some(val) = environment.read().get_var(&arg) {
                            if val.is_empty() {
                                buf.push("\"\"".to_string());
                            } else {
                                let needs_quote =
                                    val.contains('\n') || val.contains(' ') || val.contains('\t');
                                if needs_quote {
                                    let escaped = val.replace('"', r#"\""#);
                                    buf.push(format!("\"{}\"", escaped));
                                } else {
                                    buf.push(val.trim().to_string());
                                }
                            }
                        } else {
                            buf.push(arg);
                        }
                    }
                }
                _ => {
                    debug!(
                        "expand_command_alias missing {:?} {:?}",
                        inner_pair.as_rule(),
                        inner_pair.as_str()
                    );
                }
            }
        }
    } else if let Rule::command_list_sep = pair.as_rule() {
        buf.push(pair.as_str().to_string());
    }

    Ok(buf)
}
