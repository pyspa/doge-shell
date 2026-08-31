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

/// What `expand_alias_tilde` needs to resolve a token.
///
/// The environment is here because a span (`--file=`, `$HOME`, `/x`) is a
/// single argv entry, so its variables must be resolved *before* the parts are
/// joined -- the later whole-token pass cannot see inside a span.
pub struct ExpandCtx<'a> {
    pub env: &'a Environment,
    pub current_dir: &'a Path,
}

impl ExpandCtx<'_> {
    fn alias(&self) -> &HashMap<String, String> {
        &self.env.variable_state.alias
    }
}

/// Characters that survive a re-parse as themselves: no whitespace, no quote,
/// no `$`, no glob metacharacter, no `~`, and no `=` -- at the start of a
/// command that would now be read as an environment prefix.
fn is_reparse_safe(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '.' | '/' | ':' | '@' | '%' | '+' | ',' | '-')
        })
}

/// Quote `value` so re-parsing yields exactly these bytes.
///
/// Single quotes, not double: the expanded line is re-parsed, and inside double
/// quotes `$` and `\` are live, so a value containing either would be
/// interpolated a second time. Values that cannot mean anything but themselves
/// are left bare so the intermediate line stays readable.
fn shell_escape_single(value: &str) -> String {
    if is_reparse_safe(value) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Expand a pattern that may contain brace and glob metacharacters.
///
/// Returns raw, unquoted results; the caller decides how to escape them. A
/// pattern that matches nothing comes back as itself, which is what the shell
/// has always done here.
fn expand_glob_pattern(pattern: &str, current_dir: &Path) -> Vec<String> {
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

/// Look up the value a `variable` pair stands for.
///
/// Normalizes `$FOO` and `${FOO}` to the same key. An unresolved variable keeps
/// its literal text, which is what the shell did before spans existed.
fn resolve_variable(pair: &Pair<Rule>, env: &Environment) -> String {
    let text = pair.as_str();
    env.get_var(text).unwrap_or_else(|| text.to_string())
}

/// Expand the command name, resolving an alias if the name is one.
///
/// Shared by both dispatch paths into `expand_alias_tilde`: the nested match in
/// the catch-all is the one real commands actually take, so a fix applied only
/// to the top-level arm would never run.
fn expand_argv0(pair: Pair<Rule>, cx: &ExpandCtx<'_>) -> Result<Vec<String>> {
    let mut argv = Vec::new();
    for span in pair.into_inner() {
        // The alias table is keyed by the bare command name, so look it up
        // against the raw span value rather than the escaped form.
        // When the span declines to flatten it hands back markers the re-parse
        // has to read as syntax -- `$(cmd)`, `(`, `)`. Escaping those would turn
        // a command substitution in command position into a literal string, so
        // only a real value is escaped.
        let (values, escape) = match expand_span(&span, cx) {
            Some(values) => (values, true),
            None => (expand_alias_tilde(span, cx)?, false),
        };
        for (index, arg) in values.iter().enumerate() {
            let arg = arg.trim();
            if index == 0
                && let Some(alias) = cx.alias().get(arg)
            {
                debug!("alias '{arg}' => '{alias}'");
                argv.push(alias.trim().to_string());
                continue;
            }
            if escape {
                argv.push(shell_escape_single(arg));
            } else {
                argv.push(arg.to_string());
            }
        }
    }
    Ok(argv)
}

/// Escape the characters that would otherwise start matching files.
///
/// Applied to text that came from a quote or a variable value: those are
/// literals, even when another part of the same word is a real pattern.
fn escape_glob_metacharacters(value: &str) -> String {
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
fn unescape_glob_metacharacters(value: &str) -> String {
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

/// Whether this part begins with a tilde the shell should expand.
fn starts_with_bare_tilde(part: &Pair<Rule>) -> bool {
    matches!(
        part.as_rule(),
        Rule::word | Rule::glob_word | Rule::brace_word
    ) && part.as_str().starts_with('~')
}

/// Whether this part -- or anything nested in a double-quoted run inside it --
/// is a substitution that `shell/parse.rs` handles as a marker rather than text.
fn contains_substitution(part: &Pair<Rule>) -> bool {
    match part.as_rule() {
        Rule::command_subst | Rule::subshell | Rule::proc_subst => true,
        Rule::d_quoted => part.clone().into_inner().any(|inner| {
            matches!(
                inner.as_rule(),
                Rule::command_subst | Rule::subshell | Rule::proc_subst
            )
        }),
        _ => false,
    }
}

/// Expand one `span` into the argv entries it stands for, or `None` when this
/// span must take the older per-part path.
///
/// Command substitutions, subshells and process substitutions are handed to
/// `shell/parse.rs` as markers rather than as text, so a span containing one
/// cannot be flattened here.
fn expand_span(pair: &Pair<Rule>, cx: &ExpandCtx<'_>) -> Option<Vec<String>> {
    let parts: Vec<_> = pair.clone().into_inner().collect();
    if parts.iter().any(contains_substitution) {
        return None;
    }

    // Two views of the same word. `text` is the literal result; `pattern` is
    // what to match files against, with metacharacters that came from a quote
    // or a variable value escaped. The flag is per span, so without this a
    // quoted `*` next to a real glob became a live pattern.
    let mut text = String::new();
    let mut pattern = String::new();
    let mut globbable = false;

    let push_literal = |text: &mut String, pattern: &mut String, value: &str| {
        text.push_str(value);
        pattern.push_str(&escape_glob_metacharacters(value));
    };

    for part in &parts {
        match part.as_rule() {
            Rule::variable => {
                push_literal(&mut text, &mut pattern, &resolve_variable(part, cx.env));
            }
            Rule::s_quoted => {
                push_literal(
                    &mut text,
                    &mut pattern,
                    &get_string(part.clone()).unwrap_or_default(),
                );
            }
            // Double quotes interpolate, so walk the parts instead of taking
            // the literal text.
            Rule::d_quoted => {
                for inner in part.clone().into_inner() {
                    let value = match inner.as_rule() {
                        Rule::variable => resolve_variable(&inner, cx.env),
                        _ => get_string(inner).unwrap_or_default(),
                    };
                    push_literal(&mut text, &mut pattern, &value);
                }
            }
            // Glob and brace patterns keep their backslashes: `get_string`
            // would collapse `\*` into a literal `*` and it would start
            // matching files.
            Rule::glob_word | Rule::brace_word => {
                globbable = true;
                text.push_str(part.as_str());
                pattern.push_str(part.as_str());
            }
            // Everything else goes through `get_string` so `a\ b` loses its
            // backslash here rather than carrying it into argv.
            _ => {
                push_literal(
                    &mut text,
                    &mut pattern,
                    &get_string(part.clone()).unwrap_or_default(),
                );
            }
        }
    }

    // A tilde is only special at the start of a word, and only when the user
    // typed it unquoted -- `"x"~/y` is a literal, and so is `\~`.
    if parts.first().is_some_and(starts_with_bare_tilde) {
        text = shellexpand::tilde(&text).into_owned();
        pattern = shellexpand::tilde(&pattern).into_owned();
    }

    if !globbable {
        return Some(vec![text]);
    }

    let matches = expand_glob_pattern(&pattern, cx.current_dir);
    // A pattern that matched nothing comes back as itself, escapes and all, so
    // hand back the literal view instead -- the backslashes we added to keep a
    // quoted `*` inert must not reach argv.
    if matches.len() == 1 && matches[0] == pattern {
        return Some(vec![text]);
    }
    Some(
        matches
            .into_iter()
            .map(|value| unescape_glob_metacharacters(&value))
            .collect(),
    )
}

pub fn expand_alias_tilde(pair: Pair<Rule>, cx: &ExpandCtx<'_>) -> Result<Vec<String>> {
    let mut argv: Vec<String> = vec![];

    match pair.as_rule() {
        // A span is one argv entry, so resolve and join its parts here. The
        // result is single-quoted because the expanded line is re-parsed and
        // must not be split, globbed or interpolated a second time.
        Rule::span => match expand_span(&pair, cx) {
            Some(values) => argv.extend(values.iter().map(|value| shell_escape_single(value))),
            None => {
                // Contains a substitution. The markers `shell/parse.rs`
                // recognises have to survive, so the parentheses are re-emitted
                // around a body that is itself expanded -- dropping them would
                // turn a subshell into a plain command list, and skipping the
                // body left everything inside it unexpanded.
                for inner_pair in pair.into_inner() {
                    match inner_pair.as_rule() {
                        Rule::subshell => {
                            argv.push("(".to_string());
                            for body in inner_pair.into_inner() {
                                argv.append(&mut expand_alias_tilde(body, cx)?);
                            }
                            argv.push(")".to_string());
                        }
                        Rule::proc_subst => {
                            argv.push("<(".to_string());
                            for body in inner_pair.into_inner() {
                                argv.append(&mut expand_alias_tilde(body, cx)?);
                            }
                            argv.push(")".to_string());
                        }
                        _ => argv.append(&mut expand_alias_tilde(inner_pair, cx)?),
                    }
                }
            }
        },
        // Without an explicit arm these fall into the catch-all, whose inner
        // match does not list them -- and the whole prefix disappears from the
        // re-serialized line, so `FOO=$HOME cmd` silently loses `FOO`.
        Rule::assignment_list => {
            for assignment in pair.into_inner() {
                argv.append(&mut expand_alias_tilde(assignment, cx)?);
            }
        }
        Rule::assignment => {
            let mut name = String::new();
            let mut value = None;
            for part in pair.into_inner() {
                match part.as_rule() {
                    Rule::assign_name => name = part.as_str().to_string(),
                    Rule::span => {
                        // Assignment is not a glob context, so join whatever
                        // the span expands to rather than letting it split.
                        value = Some(match expand_span(&part, cx) {
                            Some(values) => values.join(" "),
                            None => part.as_str().to_string(),
                        });
                    }
                    _ => {}
                }
            }
            argv.push(format!(
                "{name}={}",
                shell_escape_single(&value.unwrap_or_default())
            ));
        }
        Rule::glob_word | Rule::brace_word => {
            let pattern = shellexpand::tilde(pair.as_str()).to_string();
            argv.extend(
                expand_glob_pattern(&pattern, cx.current_dir)
                    .iter()
                    .map(|value| shell_escape_single(value)),
            );
        }
        // Reached only when the span declined to flatten, i.e. this string
        // contains a substitution. `"$(cmd)"` on its own is unwrapped so the
        // re-parse still substitutes it as a single argument.
        //
        // KNOWN LIMITATION: mixed content such as `"a $(cmd) b"` stays literal.
        // Joining it correctly needs the substitution's *result*, which the
        // parser cannot produce -- it hands substitutions to `shell/parse.rs`
        // as markers. Emitting the parts separately would silently turn one
        // argument into three, so a visibly literal `$(cmd)` is the honest
        // failure until expansion moves after parsing.
        Rule::d_quoted => {
            let mut inner = pair.clone().into_inner();
            match (inner.next(), inner.next()) {
                (Some(only), None) if only.as_rule() == Rule::command_subst => {
                    argv.append(&mut expand_alias_tilde(only, cx)?);
                }
                _ => argv.push(shellexpand::tilde(pair.as_str()).to_string()),
            }
        }
        // A duplication is one indivisible operator. Without an arm it fell to
        // the catch-all, whose inner match does not list it either, so any line
        // that also triggered expansion lost the `2>&1` entirely.
        Rule::fd_dup => argv.push(pair.as_str().to_string()),
        Rule::word
        | Rule::variable
        | Rule::s_quoted
        | Rule::literal_s_quoted
        | Rule::literal_d_quoted
        | Rule::stdout_redirect_direction
        | Rule::stderr_redirect_direction
        | Rule::stdouterr_redirect_direction
        | Rule::stdin_redirect_direction
        | Rule::stdin_redirect_direction_in => {
            argv.push(shellexpand::tilde(pair.as_str()).to_string());
        }
        // The body of a substitution is a command line like any other, so it
        // gets the same expansion. Passing it through verbatim meant nothing
        // inside it was ever expanded: `echo $(echo $HOME)` printed `$HOME`,
        // `$(echo ~)` printed `~`, and an alias in there was never resolved.
        // Only the markers are re-emitted; the body itself is recursed into,
        // the way `subshell` and `proc_subst` already are.
        Rule::command_subst => {
            debug!("expand command_subst {}", pair.as_str());
            argv.push("$(".to_string());
            for inner_pair in pair.into_inner() {
                let mut v = expand_alias_tilde(inner_pair, cx)?;
                argv.append(&mut v);
            }
            argv.push(")".to_string());
        }
        Rule::argv0 => argv.append(&mut expand_argv0(pair, cx)?),
        // Operators are re-serialized as they were written. Every one of these
        // used to fall through to the catch-all below, which iterates children
        // and drops anything it does not recognise, so `a | b &` came back as
        // `a | b` and ran in the foreground, and `(a; b)` came back as `(a b)`.
        Rule::background_op
        | Rule::pipeline_op
        | Rule::capture_op
        | Rule::struct_pipe_op
        | Rule::sequential_op
        | Rule::and_op
        | Rule::or_op
        | Rule::command_list_sep
        | Rule::capture_suffix
        | Rule::struct_pipe_command => {
            argv.push(pair.as_str().to_string());
        }
        Rule::pipe_command => {
            debug!("expand pipe_command {}", pair.as_str());
            for inner_pair in pair.into_inner() {
                let mut v = expand_alias_tilde(inner_pair, cx)?;
                argv.append(&mut v);
            }
        }
        Rule::redirect => {
            for inner_pair in pair.into_inner() {
                let mut v = expand_alias_tilde(inner_pair, cx)?;
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
                                let mut v = expand_alias_tilde(inner_pair, cx)?;
                                argv.append(&mut v);
                            }
                        }
                    }
                    Rule::proc_subst => {
                        debug!("expand proc_subst {}", inner_pair.as_str());
                        argv.push("<(".to_string());
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = expand_alias_tilde(inner_pair, cx)?;
                            argv.append(&mut v);
                        }
                        argv.push(")".to_string());
                    }
                    Rule::subshell => {
                        debug!("expand subshell {}", inner_pair.as_str());
                        argv.push("(".to_string());
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = expand_alias_tilde(inner_pair, cx)?;
                            argv.append(&mut v);
                        }
                        argv.push(")".to_string());
                    }
                    Rule::argv0 => argv.append(&mut expand_argv0(inner_pair, cx)?),
                    Rule::pipe_command => {
                        for inner_pair in inner_pair.into_inner() {
                            if inner_pair.as_rule() == Rule::pipeline_op {
                                argv.push(inner_pair.as_str().to_string());
                            } else {
                                let mut v = expand_alias_tilde(inner_pair, cx)?;
                                argv.append(&mut v);
                            }
                        }
                    }
                    Rule::commands | Rule::command | Rule::simple_command | Rule::args => {
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = expand_alias_tilde(inner_pair, cx)?;
                            argv.append(&mut v);
                        }
                    }
                    // Dispatched whole, not iterated. A span's parts form one
                    // argv entry, and an assignment prefix is not listed by the
                    // inner match at all -- stepping into either here dropped
                    // it from the re-serialized line.
                    Rule::assignment_list
                    | Rule::command_subst
                    | Rule::background_op
                    | Rule::pipeline_op
                    | Rule::capture_op
                    | Rule::struct_pipe_op
                    | Rule::sequential_op
                    | Rule::and_op
                    | Rule::or_op
                    | Rule::command_list_sep
                    | Rule::capture_suffix
                    | Rule::struct_pipe_command
                    | Rule::fd_dup
                    | Rule::span
                    | Rule::word
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
                        let mut v = expand_alias_tilde(inner_pair, cx)?;
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

    let has_meta = input.contains('~')
        || input.contains('$')
        || input.contains('{')
        || input.contains('*')
        || input.contains('?')
        || input.contains('[');

    if !has_meta {
        let env_read = environment.read();
        if env_read.variable_state.alias.is_empty() {
            return Ok((std::borrow::Cow::Borrowed(input), Some(pairs)));
        }
    }

    // Check if expansion is needed
    let mut needs_expansion = false;
    {
        let env_read = environment.read();

        // We iterate over a clone of pairs to check for expansion triggers
        // This is cheaper than re-parsing if we can avoid expansion
        for pair in pairs.clone() {
            if check_expansion_needed(pair, &env_read.variable_state.alias) {
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
            let mut it = pair.into_inner();
            if let Some(first) = it.next() {
                if let Some(cmd) = get_string(first.clone())
                    && alias.contains_key(&cmd)
                {
                    return true;
                }
                if check_expansion_needed(first, alias) {
                    return true;
                }
            }
            for inner in it {
                if check_expansion_needed(inner, alias) {
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

/// Resolve any remaining whole-token variables.
///
/// Spans are already resolved and escaped by [`expand_span`], so what reaches
/// here is operators and markers. Only a token that still *looks* like a
/// variable reference is substituted -- a bare word must never be read as a
/// variable name, or `echo $USER LANG` would print the value of `LANG`.
fn expand_var_args(args: Vec<String>, env: &Environment, buf: &mut Vec<String>) {
    for arg in args {
        if !arg.starts_with('$') {
            buf.push(arg);
            continue;
        }
        match env.get_var(&arg) {
            // No trimming: leading and trailing whitespace can be the
            // whole point of a value, and the escaping below already keeps
            // it from being re-split.
            Some(val) => buf.push(shell_escape_single(&val)),
            None => buf.push(arg),
        }
    }
}

fn expand_command_alias(
    pair: Pair<Rule>,
    environment: Arc<RwLock<Environment>>,
    _current_dir: &PathBuf,
) -> Result<Vec<String>> {
    let mut buf: Vec<String> = Vec::new();

    if let Rule::command = pair.as_rule() {
        let env_guard = environment.read();
        let cx = ExpandCtx {
            env: &env_guard,
            current_dir: _current_dir,
        };
        for inner_pair in pair.into_inner() {
            match inner_pair.as_rule() {
                Rule::simple_command => {
                    let args = expand_alias_tilde(inner_pair, &cx)?;
                    expand_var_args(args, &env_guard, &mut buf);
                }
                // `&` and `|` come back from the expander itself now, so that
                // a nested `(a | b &)` keeps them too. Adding them here as well
                // would double them.
                Rule::simple_command_bg | Rule::pipe_command => {
                    let args = expand_alias_tilde(inner_pair, &cx)?;
                    expand_var_args(args, &env_guard, &mut buf);
                }
                Rule::struct_pipe_command => {
                    // Preserve struct_pipe_command (|: lisp_expr) during alias expansion
                    buf.push(inner_pair.as_str().to_string());
                }
                Rule::capture_suffix => {
                    // Preserve capture suffix (|>)
                    buf.push(inner_pair.as_str().to_string());
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
