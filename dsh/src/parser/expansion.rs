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

mod alias;
mod glob;
pub use alias::{expand_alias, expand_alias_from_pairs, parse_with_expansion};
#[cfg(test)]
pub(crate) use glob::expand_braces;
use glob::{escape_glob_metacharacters, expand_glob_pattern, unescape_glob_metacharacters};

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
