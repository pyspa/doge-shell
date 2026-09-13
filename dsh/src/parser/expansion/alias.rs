//! Rewriting a command line with its aliases applied: the `parse_with_expansion`
//! entry point, the cheap pre-check that decides whether a reparse is needed at
//! all, and the per-command substitution that walks the parsed pairs.
use super::*;

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
