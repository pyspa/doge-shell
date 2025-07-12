use crate::environment::Environment;
use crate::string_optimization::{StringOptimizationContext, get_fixed_string};
use anyhow::{Result, anyhow, bail, ensure};
use parking_lot::RwLock;
use pest::Parser;
use pest::Span;
use pest::iterators::Pair;
use pest_derive::Parser;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::debug;

#[derive(Parser)]
#[grammar = "shell.pest"]
pub struct ShellParser;

/// helpers
pub fn get_string(pair: Pair<Rule>) -> Option<String> {
    match pair.as_rule() {
        Rule::s_quoted | Rule::d_quoted => {
            let res = if let Some(next) = pair.into_inner().next() {
                next.as_str().to_string()
            } else {
                "".to_string()
            };
            Some(res)
        }
        Rule::span => get_string(pair.into_inner().next().unwrap()), // TODO fix
        _ => Some(pair.as_str().to_string()),
    }
}

pub fn get_pos_word(input: &str, pos: usize) -> Result<Option<(Rule, Span)>> {
    let pairs = ShellParser::parse(Rule::command, input).map_err(|e| anyhow!(e))?;

    for pair in pairs {
        match pair.as_rule() {
            Rule::command => {
                for pair in pair.into_inner() {
                    let res = search_pos_word(pair, pos);
                    if res.is_some() {
                        return Ok(res);
                    }
                }
            }
            _ => return Ok(None),
        }
    }
    Ok(None)
}

fn search_pos_word(pair: Pair<Rule>, pos: usize) -> Option<(Rule, Span)> {
    match pair.as_rule() {
        Rule::commands
        | Rule::command
        | Rule::simple_command
        | Rule::simple_command_bg
        | Rule::span
        | Rule::proc_subst
        | Rule::subshell => {
            for pair in pair.into_inner() {
                let res = search_pos_word(pair, pos);
                if res.is_some() {
                    return res;
                }
            }
        }
        Rule::argv0 => {
            for pair in pair.into_inner() {
                for pair in pair.into_inner() {
                    match pair.as_rule() {
                        Rule::proc_subst => {
                            let res = search_pos_word(pair, pos);
                            if res.is_some() {
                                return res;
                            }
                        }
                        Rule::subshell => {
                            let res = search_pos_word(pair, pos);
                            if res.is_some() {
                                return res;
                            }
                        }
                        _ => {
                            if let Some(res) = search_inner_word(pair, pos) {
                                return Some((Rule::argv0, res));
                            }
                        }
                    }
                }
            }
        }
        Rule::args => {
            for pair in pair.into_inner() {
                for pair in pair.into_inner() {
                    match pair.as_rule() {
                        Rule::proc_subst => {
                            let res = search_pos_word(pair, pos);
                            if res.is_some() {
                                return res;
                            }
                        }
                        Rule::subshell => {
                            let res = search_pos_word(pair, pos);
                            if res.is_some() {
                                return res;
                            }
                        }
                        _ => {
                            if let Some(res) = search_inner_word(pair, pos) {
                                return Some((Rule::argv0, res));
                            }
                        }
                    }
                }
            }
        }
        _ => {
            // TODO check search_pos_word
            println!("search_pos_word {:?} {:?}", pair.as_rule(), pair.as_str());
        }
    }
    None
}

fn search_inner_word(pair: Pair<Rule>, pos: usize) -> Option<Span> {
    match pair.as_rule() {
        Rule::s_quoted | Rule::d_quoted => {
            for pair in pair.into_inner() {
                let pair_span = pair.as_span();
                if pair_span.start() < pos && pos <= pair_span.end() {
                    return Some(pair_span);
                }
            }
        }
        Rule::word | Rule::glob_word | Rule::variable => {
            let pair_span = pair.as_span();
            if pair_span.start() < pos && pos <= pair_span.end() {
                return Some(pair_span);
            }
        }
        _ => {}
    }
    None
}

fn expand_alias_tilde_optimized(
    pair: Pair<Rule>,
    alias: &HashMap<String, String>,
    _current_dir: &PathBuf,
    ctx: &mut StringOptimizationContext,
) -> Result<Vec<String>> {
    let mut argv = Vec::with_capacity(4); // Pre-allocate with reasonable capacity

    match pair.as_rule() {
        Rule::glob_word => {
            let pattern = ctx.fast_tilde_expand(pair.as_str());
            let (root, pattern) = find_glob_root_optimized(&pattern, ctx);
            debug!("glob pattern: root:{} {:?} ", root, pattern);
            match globmatch::Builder::new(&pattern).build(&*root) {
                Ok(builder) => {
                    let paths: Vec<_> = builder.into_iter().flatten().collect();
                    ensure!(
                        !paths.is_empty(),
                        "dsh: no matches for wildcard '{}'",
                        &pattern
                    );

                    for path in paths {
                        debug!("glob match {}", path.display());
                        // Use string pool for formatted strings
                        let mut quoted = ctx.get_pooled_string(path.as_os_str().len() + 2);
                        quoted.push('"');
                        quoted.push_str(&path.to_string_lossy());
                        quoted.push('"');
                        argv.push(quoted);
                    }
                }
                Err(err) => {
                    bail!("dsh: failed resolve paths. {}", err);
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
        | Rule::stdouterr_redirect_direction => {
            argv.push(ctx.fast_tilde_expand(pair.as_str()).into_owned());
        }
        Rule::argv0 => {
            for inner_pair in pair.into_inner() {
                let v = expand_alias_tilde_optimized(inner_pair, alias, _current_dir, ctx)?;
                for (i, arg) in v.iter().enumerate() {
                    if i == 0 {
                        if let Some(val) = alias.get(arg) {
                            debug!("alias '{}' => '{}'", arg, val);
                            argv.push(StringOptimizationContext::trim_cow(val).into_owned());
                        } else {
                            argv.push(StringOptimizationContext::trim_cow(arg).into_owned());
                        }
                    } else {
                        argv.push(StringOptimizationContext::trim_cow(arg).into_owned());
                    }
                }
            }
        }
        Rule::pipe_command => {
            debug!("expand pipe_command {}", pair.as_str());
            // Pipe character is added by expand_alias function, so don't add it here
            for inner_pair in pair.into_inner() {
                let v = expand_alias_tilde_optimized(inner_pair, alias, _current_dir, ctx)?;
                argv.extend(v);
            }
        }
        _ => {
            debug!("@expand: {:?} : {:?}", pair.as_rule(), pair.as_str());
            for inner_pair in pair.into_inner() {
                match inner_pair.as_rule() {
                    Rule::simple_command_bg => {
                        for inner_pair in inner_pair.into_inner() {
                            let v =
                                expand_alias_tilde_optimized(inner_pair, alias, _current_dir, ctx)?;
                            argv.extend(v);
                        }
                        // Use fixed string for background operator
                        if let Some(bg_op) = get_fixed_string("&") {
                            argv.push(bg_op.to_string());
                        } else {
                            argv.push("&".to_string());
                        }
                    }
                    Rule::proc_subst => {
                        debug!("expand proc_subst {}", inner_pair.as_str());
                        if let Some(proc_start) = get_fixed_string("<(") {
                            argv.push(proc_start.to_string());
                        } else {
                            argv.push("<(".to_string());
                        }
                        for inner_pair in inner_pair.into_inner() {
                            let v =
                                expand_alias_tilde_optimized(inner_pair, alias, _current_dir, ctx)?;
                            argv.extend(v);
                        }
                        if let Some(paren) = get_fixed_string(")") {
                            argv.push(paren.to_string());
                        } else {
                            argv.push(")".to_string());
                        }
                    }
                    Rule::subshell => {
                        debug!("expand subshell {}", inner_pair.as_str());
                        if let Some(paren_start) = get_fixed_string("(") {
                            argv.push(paren_start.to_string());
                        } else {
                            argv.push("(".to_string());
                        }
                        for inner_pair in inner_pair.into_inner() {
                            let v =
                                expand_alias_tilde_optimized(inner_pair, alias, _current_dir, ctx)?;
                            argv.extend(v);
                        }
                        if let Some(paren_end) = get_fixed_string(")") {
                            argv.push(paren_end.to_string());
                        } else {
                            argv.push(")".to_string());
                        }
                    }
                    Rule::argv0 => {
                        for inner_pair in inner_pair.into_inner() {
                            let v =
                                expand_alias_tilde_optimized(inner_pair, alias, _current_dir, ctx)?;
                            for (i, arg) in v.iter().enumerate() {
                                if i == 0 {
                                    if let Some(val) = alias.get(arg) {
                                        debug!("alias '{}' => '{}'", arg, val);
                                        argv.push(
                                            StringOptimizationContext::trim_cow(val).into_owned(),
                                        );
                                    } else {
                                        argv.push(
                                            StringOptimizationContext::trim_cow(arg).into_owned(),
                                        );
                                    }
                                } else {
                                    argv.push(
                                        StringOptimizationContext::trim_cow(arg).into_owned(),
                                    );
                                }
                            }
                        }
                    }
                    Rule::pipe_command => {
                        if let Some(pipe_op) = get_fixed_string("|") {
                            argv.push(pipe_op.to_string());
                        } else {
                            argv.push("|".to_string());
                        }
                        for inner_pair in inner_pair.into_inner() {
                            let v =
                                expand_alias_tilde_optimized(inner_pair, alias, _current_dir, ctx)?;
                            argv.extend(v);
                        }
                    }
                    Rule::commands
                    | Rule::command
                    | Rule::simple_command
                    | Rule::args
                    | Rule::redirect
                    | Rule::span => {
                        for inner_pair in inner_pair.into_inner() {
                            let v =
                                expand_alias_tilde_optimized(inner_pair, alias, _current_dir, ctx)?;
                            argv.extend(v);
                        }
                    }
                    Rule::word
                    | Rule::glob_word
                    | Rule::variable
                    | Rule::s_quoted
                    | Rule::d_quoted
                    | Rule::literal_s_quoted
                    | Rule::literal_d_quoted
                    | Rule::proc_subst_direction_in
                    | Rule::stdout_redirect_direction
                    | Rule::stderr_redirect_direction
                    | Rule::stdouterr_redirect_direction => {
                        let v = expand_alias_tilde_optimized(inner_pair, alias, _current_dir, ctx)?;
                        argv.extend(v);
                    }
                    _ => {
                        debug!(
                            "expand_alias_tilde_optimized missing {:?} {:?}",
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

fn expand_alias_tilde(
    pair: Pair<Rule>,
    alias: &HashMap<String, String>,
    _current_dir: &PathBuf,
) -> Result<Vec<String>> {
    let mut argv: Vec<String> = vec![];

    match pair.as_rule() {
        Rule::glob_word => {
            let pattern = shellexpand::tilde(pair.as_str()).to_string();
            let (root, pattern) = find_glob_root(pattern.as_str());
            debug!("glob pattern: root:{} {:?} ", root, pattern);
            match globmatch::Builder::new(&pattern).build(root) {
                Ok(builder) => {
                    let paths: Vec<_> = builder.into_iter().flatten().collect();
                    ensure!(
                        !paths.is_empty(),
                        "dsh: no matches for wildcard '{}'",
                        &pattern
                    );

                    for path in paths {
                        debug!("glob match {}", path.display());
                        argv.push(format!("\"{}\"", path.display()));
                    }
                }
                Err(err) => {
                    bail!("dsh: failed resolve paths. {}", err);
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
        | Rule::stdouterr_redirect_direction => {
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
        _ => {
            debug!("@expand: {:?} : {:?}", pair.as_rule(), pair.as_str());
            for inner_pair in pair.into_inner() {
                match inner_pair.as_rule() {
                    Rule::simple_command_bg => {
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                            argv.append(&mut v);
                        }
                        argv.push("&".to_string());
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
                        argv.push("|".to_string());
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                            argv.append(&mut v);
                        }
                    }
                    Rule::commands
                    | Rule::command
                    | Rule::simple_command
                    | Rule::args
                    | Rule::redirect
                    | Rule::span => {
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = expand_alias_tilde(inner_pair, alias, _current_dir)?;
                            argv.append(&mut v);
                        }
                    }
                    Rule::word
                    | Rule::glob_word
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

pub fn expand_alias_optimized(
    input: String,
    environment: Arc<RwLock<Environment>>,
    ctx: &mut StringOptimizationContext,
) -> Result<String> {
    let mut buf = Vec::with_capacity(16); // Pre-allocate with reasonable capacity
    let pairs = ShellParser::parse(Rule::commands, &input).map_err(|e| anyhow!(e))?;
    let current_dir = std::env::current_dir()?;

    for pair in pairs {
        for pair in pair.into_inner() {
            let args = expand_command_optimized(pair, &environment, &current_dir, ctx)?;
            buf.extend(args);
        }
    }
    Ok(buf.join(" "))
}

fn expand_command_optimized(
    pair: Pair<Rule>,
    environment: &Arc<RwLock<Environment>>,
    current_dir: &PathBuf,
    ctx: &mut StringOptimizationContext,
) -> Result<Vec<String>> {
    let mut buf = Vec::with_capacity(8); // Pre-allocate

    if let Rule::command = pair.as_rule() {
        for pair in pair.into_inner() {
            match pair.as_rule() {
                Rule::simple_command => {
                    let args = expand_alias_tilde_optimized(
                        pair,
                        &environment.read().alias,
                        current_dir,
                        ctx,
                    )?;

                    for (i, val) in args.iter().enumerate() {
                        if i == 0 {
                            // First argument - check for variable expansion
                            if val.starts_with('$') {
                                if let Some(expanded) = environment.read().variables.get(val) {
                                    if expanded.is_empty() {
                                        buf.push("\"\"".to_string());
                                    } else {
                                        buf.push(
                                            StringOptimizationContext::trim_cow(expanded)
                                                .into_owned(),
                                        );
                                    }
                                } else {
                                    buf.push(StringOptimizationContext::trim_cow(val).into_owned());
                                }
                            } else {
                                buf.push(StringOptimizationContext::trim_cow(val).into_owned());
                            }
                        } else {
                            // Other arguments - check for variable expansion
                            if val.starts_with('$') {
                                if let Some(expanded) = environment.read().variables.get(val) {
                                    if expanded.is_empty() {
                                        buf.push("\"\"".to_string());
                                    } else {
                                        buf.push(
                                            StringOptimizationContext::trim_cow(expanded)
                                                .into_owned(),
                                        );
                                    }
                                } else {
                                    buf.push(StringOptimizationContext::trim_cow(val).into_owned());
                                }
                            } else {
                                buf.push(StringOptimizationContext::trim_cow(val).into_owned());
                            }
                        }
                    }
                }
                Rule::simple_command_bg => {
                    for inner_pair in pair.into_inner() {
                        let args = expand_alias_tilde_optimized(
                            inner_pair,
                            &environment.read().alias,
                            current_dir,
                            ctx,
                        )?;

                        for val in args {
                            if val.starts_with('$') {
                                if let Some(expanded) = environment.read().variables.get(&val) {
                                    if expanded.is_empty() {
                                        buf.push("\"\"".to_string());
                                    } else {
                                        buf.push(
                                            StringOptimizationContext::trim_cow(expanded)
                                                .into_owned(),
                                        );
                                    }
                                } else {
                                    buf.push(
                                        StringOptimizationContext::trim_cow(&val).into_owned(),
                                    );
                                }
                            } else {
                                buf.push(StringOptimizationContext::trim_cow(&val).into_owned());
                            }
                        }
                    }
                    // Use fixed string for background operator
                    if let Some(bg_op) = get_fixed_string("&") {
                        buf.push(bg_op.to_string());
                    } else {
                        buf.push("&".to_string());
                    }
                }
                Rule::pipe_command => {
                    if let Some(pipe_op) = get_fixed_string("|") {
                        buf.push(pipe_op.to_string());
                    } else {
                        buf.push("|".to_string());
                    }
                    let args = expand_alias_tilde_optimized(
                        pair,
                        &environment.read().alias,
                        current_dir,
                        ctx,
                    )?;

                    for val in args {
                        if val.starts_with('$') {
                            if let Some(expanded) = environment.read().variables.get(&val) {
                                if expanded.is_empty() {
                                    buf.push("\"\"".to_string());
                                } else {
                                    buf.push(
                                        StringOptimizationContext::trim_cow(expanded).into_owned(),
                                    );
                                }
                            } else {
                                buf.push(StringOptimizationContext::trim_cow(&val).into_owned());
                            }
                        } else {
                            buf.push(StringOptimizationContext::trim_cow(&val).into_owned());
                        }
                    }
                }
                _ => {
                    debug!(
                        "expand_command_optimized missing {:?} {:?}",
                        pair.as_rule(),
                        pair.as_str()
                    );
                }
            }
        }
    } else if let Rule::command_list_sep = pair.as_rule() {
        // Use fixed string for command separators
        if let Some(sep) = get_fixed_string(pair.as_str()) {
            buf.push(sep.to_string());
        } else {
            buf.push(pair.as_str().to_string());
        }
    }

    Ok(buf)
}

pub fn expand_alias(input: String, environment: Arc<RwLock<Environment>>) -> Result<String> {
    let mut buf: Vec<String> = Vec::new();
    let pairs = ShellParser::parse(Rule::commands, &input).map_err(|e| anyhow!(e))?;
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
                                let val = val.trim().to_string();
                                buf.push(val);
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
                                let val = val.trim().to_string();
                                buf.push(val);
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
                                let val = val.trim().to_string();
                                buf.push(val);
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

pub fn get_words(input: &str, pos: usize) -> Result<Vec<(Rule, Span, bool)>> {
    let pairs = ShellParser::parse(Rule::command, input).map_err(|e| anyhow!(e))?;
    let mut result: Vec<(Rule, Span, bool)> = Vec::new();
    for pair in pairs {
        match pair.as_rule() {
            Rule::command => {
                for pair in pair.into_inner() {
                    let mut res = to_words(pair, pos);
                    result.append(&mut res);
                }
            }
            _ => return Ok(result),
        }
    }
    Ok(result)
}

fn to_words(pair: Pair<Rule>, pos: usize) -> Vec<(Rule, Span, bool)> {
    let mut result: Vec<(Rule, Span, bool)> = vec![];
    match pair.as_rule() {
        Rule::argv0 => {
            for pair in pair.into_inner() {
                for pair in pair.into_inner() {
                    // TODO check subshell
                    if let Some((span, current)) = get_span(pair, pos) {
                        result.push((Rule::argv0, span, current));
                    };
                }
            }
        }
        Rule::args => {
            for pair in pair.into_inner() {
                for pair in pair.into_inner() {
                    if let Some((span, current)) = get_span(pair, pos) {
                        result.push((Rule::args, span, current));
                    };
                }
            }
        }

        _ => {
            for inner_pair in pair.into_inner() {
                // debug!(
                //     "inner_pair {:?} {:?}",
                //     inner_pair.as_rule(),
                //     inner_pair.as_str()
                // );

                match inner_pair.as_rule() {
                    Rule::simple_command | Rule::simple_command_bg => {
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = to_words(inner_pair, pos);
                            result.append(&mut v);
                        }
                    }
                    Rule::argv0 => {
                        for pair in inner_pair.into_inner() {
                            for pair in pair.into_inner() {
                                // TODO check subshell
                                if let Some((span, current)) = get_span(pair, pos) {
                                    result.push((Rule::argv0, span, current));
                                };
                            }
                        }
                    }
                    Rule::args => {
                        for pair in inner_pair.into_inner() {
                            for pair in pair.into_inner() {
                                if let Some((span, current)) = get_span(pair, pos) {
                                    result.push((Rule::args, span, current));
                                };
                            }
                        }
                    }

                    _ => {
                        debug!(
                            "to_words missing {:?} {:?}",
                            inner_pair.as_rule(),
                            inner_pair.as_str()
                        );
                    }
                }
            }
        }
    }
    result
}

fn get_span(pair: Pair<Rule>, pos: usize) -> Option<(Span, bool)> {
    match pair.as_rule() {
        Rule::span => {
            for pair in pair.into_inner() {
                let span = get_span(pair, pos);
                if span.is_some() {
                    return span;
                }
            }
        }
        Rule::s_quoted
        | Rule::d_quoted
        | Rule::argv0
        | Rule::args
        | Rule::proc_subst
        | Rule::subshell
        | Rule::simple_command
        | Rule::simple_command_bg
        | Rule::command
        | Rule::commands => {
            for pair in pair.into_inner() {
                let span = get_span(pair, pos);
                if span.is_some() {
                    return span;
                }
            }
        }
        Rule::word
        | Rule::glob_word
        | Rule::variable
        | Rule::literal_s_quoted
        | Rule::literal_d_quoted
        | Rule::proc_subst_direction
        | Rule::stdout_redirect_direction
        | Rule::stderr_redirect_direction
        | Rule::stdouterr_redirect_direction => {
            let pair_span = pair.as_span();
            if pair_span.start() < pos && pos <= pair_span.end() {
                return Some((pair_span, true));
            } else {
                return Some((pair_span, false));
            }
        }
        Rule::proc_subst_direction_in => {
            // skip
        }

        _ => {
            debug!("get_span missing {:?} {:?}", pair.as_rule(), pair.as_str());
        }
    }
    None
}

fn find_glob_root_optimized(path: &str, ctx: &mut StringOptimizationContext) -> (String, String) {
    let (root, glob) = ctx.find_glob_root_cached(path);
    (root.into_owned(), glob.into_owned())
}

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
        if !find_glob && file.contains("*") {
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

/// Performance testing and benchmarking functions
#[cfg(test)]
pub mod performance_tests {
    use super::*;
    use crate::string_optimization::StringOptimizationContext;
    use std::time::Instant;

    /// Benchmark expand_alias performance
    pub fn benchmark_expand_alias(iterations: usize) -> (std::time::Duration, std::time::Duration) {
        let env = crate::environment::Environment::new();
        env.write()
            .alias
            .insert("ls".to_string(), "exa -al".to_string());
        env.write().alias.insert("g".to_string(), "git".to_string());
        env.write()
            .variables
            .insert("$HOME".to_string(), "/home/user".to_string());

        let test_commands = vec![
            "ls -la ~/Documents".to_string(),
            "g status".to_string(),
            "echo $HOME/test".to_string(),
            "find . -name '*.rs' | grep main".to_string(),
            "cd ~/Projects && ls".to_string(),
        ];

        // Benchmark original implementation
        let start = Instant::now();
        for _ in 0..iterations {
            for cmd in &test_commands {
                let _ = expand_alias(cmd.clone(), Arc::clone(&env));
            }
        }
        let original_time = start.elapsed();

        // Benchmark optimized implementation
        let mut ctx = StringOptimizationContext::new();
        let start = Instant::now();
        for _ in 0..iterations {
            for cmd in &test_commands {
                let _ = expand_alias_optimized(cmd.clone(), Arc::clone(&env), &mut ctx);
            }
        }
        let optimized_time = start.elapsed();

        (original_time, optimized_time)
    }

    /// Test memory usage comparison
    pub fn test_memory_efficiency() {
        let env = crate::environment::Environment::new();
        env.write()
            .alias
            .insert("test".to_string(), "echo hello".to_string());

        let mut ctx = StringOptimizationContext::new();

        // Test with many repeated operations
        for i in 0..1000 {
            let cmd = format!("test ~/path/to/file{}.txt", i);
            let _ = expand_alias_optimized(cmd, Arc::clone(&env), &mut ctx);
        }

        let stats = ctx.stats();
        println!("Cache hit rates:");
        println!("  Tilde expansion: {:.2}%", stats.tilde_hit_rate() * 100.0);
        println!("  Path processing: {:.2}%", stats.path_hit_rate() * 100.0);
        println!(
            "  String pool reuse: {:.2}%",
            stats.pool_reuse_rate() * 100.0
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pest::Parser;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tracing::debug;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    type JobLink = Rc<RefCell<Job>>;

    #[derive(Debug)]
    #[allow(dead_code)]
    pub struct Job {
        name: String,
        next: Option<JobLink>,
    }

    impl Job {
        fn new(name: String) -> Rc<RefCell<Self>> {
            Rc::new(RefCell::new(Self { name, next: None }))
        }
    }

    #[test]
    fn parse_word() {
        init();
        let pairs = ShellParser::parse(Rule::word, "a1bc").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::word, pair.as_rule());
        }
    }

    #[test]
    fn parse_quoted() {
        init();
        let pairs =
            ShellParser::parse(Rule::quoted, "\'a1bc\'").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::s_quoted, pair.as_rule());
            assert_eq!("a1bc", get_string(pair).unwrap());
        }
        let pairs =
            ShellParser::parse(Rule::quoted, "\"a1bc\"").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::d_quoted, pair.as_rule());
            assert_eq!("a1bc", get_string(pair).unwrap());
        }
    }

    #[test]
    fn parse_argv0() {
        init();
        let pairs = ShellParser::parse(Rule::argv0, "a1bc").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::argv0, pair.as_rule());
        }
    }

    #[test]
    fn parse_args1() {
        init();
        let pairs = ShellParser::parse(Rule::args, " a1bc b2").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::args, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);
            for inner_pair in pair.into_inner() {
                assert_eq!(Rule::span, inner_pair.as_rule());
            }
        }
    }

    #[test]
    fn parse_args2() {
        init();
        let pairs =
            ShellParser::parse(Rule::args, r#"echo "test""#).unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::args, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);
            for (i, inner_pair) in pair.into_inner().enumerate() {
                if i == 0 {
                    assert_eq!(Rule::span, inner_pair.as_rule());
                    assert_eq!("echo", get_string(inner_pair).unwrap());
                } else {
                    assert_eq!(Rule::span, inner_pair.as_rule());
                    assert_eq!("test", get_string(inner_pair).unwrap());
                }
            }
        }
    }

    #[test]
    fn parse_simple_command1() {
        init();
        let pairs = ShellParser::parse(Rule::simple_command, "test --a1bc --b2=c3  ")
            .unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::simple_command, pair.as_rule());

            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);

            for inner_pair in pair.into_inner() {
                match inner_pair.as_rule() {
                    Rule::argv0 => {
                        let cmd = inner_pair.as_str();
                        assert_eq!("test", cmd);
                    }
                    Rule::args => {
                        for inner_pair in inner_pair.into_inner() {
                            assert_eq!(Rule::span, inner_pair.as_rule());
                        }
                    }

                    _ => {}
                }
            }
        }
    }

    #[test]
    fn parse_simple_command2() {
        init();
        let pairs = ShellParser::parse(Rule::simple_command, "  test   ")
            .unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::simple_command, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(1, count);

            for inner_pair in pair.into_inner() {
                if inner_pair.as_rule() == Rule::argv0 {
                    let cmd = inner_pair.as_str();
                    assert_eq!("test", cmd);
                }
            }
        }
    }

    #[test]
    fn parse_simple_command3() {
        init();
        let pairs = ShellParser::parse(Rule::simple_command, r#"echo abc " test" '-vvv' --foo "#)
            .unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::simple_command, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);

            // let argv = get_argv(pair);
            // assert_eq!(5, argv.len());
            // assert_eq!("echo", argv[0].0);
            // assert_eq!("abc", argv[1].0);
            // assert_eq!(" test", argv[2].0);
            // assert_eq!("-vvv", argv[3].0);
            // assert_eq!("--foo", argv[4].0);
        }
    }

    #[test]
    fn parse_simple_command4() {
        init();
        let pairs = ShellParser::parse(Rule::simple_command, r#"sk -q "" "#)
            .unwrap_or_else(|e| panic!("{}", e));

        let mut v = vec![];
        for pair in pairs {
            assert_eq!(Rule::simple_command, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);

            for pair in pair.into_inner() {
                if let Rule::args = pair.as_rule() {
                    for pair in pair.into_inner() {
                        debug!("arg:'{}'", pair.as_str());
                        v.push(pair.as_str().to_string());
                    }
                }
            }
            // assert_eq!(5, argv.len());
            // assert_eq!("echo", argv[0].0);
            // assert_eq!("abc", argv[1].0);
            // assert_eq!(" test", argv[2].0);
            // assert_eq!("-vvv", argv[3].0);
            // assert_eq!("--foo", argv[4].0);
        }

        debug!("{}", v.join(" "));
    }

    #[test]
    fn parse_command1() {
        init();
        let pairs = ShellParser::parse(Rule::command, "history | sk --ansi --inline-info ")
            .unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::command, pair.as_rule());

            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);

            for inner_pair in pair.into_inner() {
                match inner_pair.as_rule() {
                    Rule::simple_command => {
                        let cmd = inner_pair.as_str();
                        assert_eq!("history", cmd);
                    }
                    Rule::pipe_command => {
                        let inner_pair = inner_pair.into_inner();
                        let cmd = inner_pair.as_str();
                        assert_eq!("sk --ansi --inline-info", cmd);
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn parse_command2() {
        init();
        let pairs = ShellParser::parse(Rule::command, "history|test  --a1bc --b2=c3|dd  ")
            .unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::command, pair.as_rule());

            let count = pair.clone().into_inner().count();
            assert_eq!(3, count);

            for (i, inner_pair) in pair.into_inner().enumerate() {
                match inner_pair.as_rule() {
                    Rule::simple_command => {
                        let cmd = inner_pair.as_str();
                        if i == 0 {
                            assert_eq!("history", cmd);
                        }
                    }
                    Rule::pipe_command => {
                        let inner_pair = inner_pair.into_inner();
                        let cmd = inner_pair.as_str();
                        if i == 1 {
                            assert_eq!("test  --a1bc --b2=c3", cmd);
                        } else if i == 2 {
                            assert_eq!("dd", cmd);
                        }
                    }

                    _ => {}
                }
            }
        }
    }

    #[test]
    fn parse_command3() {
        init();
        let pairs =
            ShellParser::parse(Rule::command, "history").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::command, pair.as_rule());

            let count = pair.clone().into_inner().count();
            assert_eq!(1, count);

            for (i, inner_pair) in pair.into_inner().enumerate() {
                if inner_pair.as_rule() == Rule::simple_command {
                    let cmd = inner_pair.as_str();
                    if i == 0 {
                        assert_eq!("history", cmd);
                    }
                }
            }
        }
    }

    #[test]
    fn parse_command4() {
        init();
        let pairs = ShellParser::parse(Rule::command, "history | sk | bash -s")
            .unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::command, pair.as_rule());

            let count = pair.clone().into_inner().count();
            assert_eq!(3, count);

            for (i, inner_pair) in pair.into_inner().enumerate() {
                debug!("{:?}", inner_pair.as_rule());
                match inner_pair.as_rule() {
                    Rule::simple_command => {
                        let cmd = inner_pair.as_str();
                        if i == 0 {
                            assert_eq!("history", cmd);
                        }
                    }
                    Rule::pipe_command => {
                        let inner_pair = inner_pair.into_inner();
                        let cmd = inner_pair.as_str();
                        if i == 1 {
                            assert_eq!("sk", cmd);
                        } else if i == 2 {
                            assert_eq!("bash -s", cmd);
                        }
                    }

                    _ => {}
                }
            }
        }
    }

    #[test]
    fn parse_command_sp() {
        init();
        let pairs = ShellParser::parse(Rule::command, "   ").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::command, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(0, count);
        }
    }

    #[test]
    fn parse_simple_command_bg1() {
        init();
        let pairs = ShellParser::parse(Rule::simple_command_bg, "sleep 20 &")
            .unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::simple_command_bg, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(1, count);

            for inner_pair in pair.into_inner() {
                if inner_pair.as_rule() == Rule::simple_command {
                    let cmd = inner_pair.as_str();
                    assert_eq!("sleep 20", cmd);
                }
            }
        }
    }

    #[test]
    fn parse_command_bg() {
        init();
        let pairs = ShellParser::parse(Rule::command, "sleep 20 & sleep 30 &")
            .unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::command, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);
            for (i, inner_pair) in pair.into_inner().enumerate() {
                if inner_pair.as_rule() == Rule::simple_command_bg {
                    let inner_pair = inner_pair.into_inner();
                    let cmd = inner_pair.as_str();
                    if i == 0 {
                        assert_eq!("sleep 20", cmd);
                    } else if i == 1 {
                        assert_eq!("sleep 30", cmd);
                    }
                }
            }
        }
    }

    #[test]
    fn test_get_pos_word1() -> Result<()> {
        init();
        let input = "sudo git st aaa &";
        let res = get_pos_word(input, 1)?;
        assert_eq!("sudo", res.unwrap().1.as_str());

        let res = get_pos_word(input, 5)?;
        assert_eq!(None, res);

        let res = get_pos_word(input, 6)?;
        assert_eq!("git", res.unwrap().1.as_str());

        let input = "sudo ";
        let res = get_pos_word(input, 1)?;
        assert_eq!("sudo", res.unwrap().1.as_str());

        let input = "sudo git st ( docker ps -a -q) &";
        let res = get_pos_word(input, 15)?;
        assert_eq!("docker", res.unwrap().1.as_str());
        assert_eq!(Rule::argv0, res.unwrap().0);

        Ok(())
    }

    #[test]
    fn test_get_pos_word2() -> Result<()> {
        init();
        let input = "mv *.toml ";
        let res = get_pos_word(input, 9)?;
        println!("{:?}", res.unwrap().0);
        assert_eq!("*.toml", res.unwrap().1.as_str());

        Ok(())
    }

    #[test]
    fn test_expand_alias() -> Result<()> {
        init();
        let env = crate::environment::Environment::new();

        env.write()
            .alias
            .insert("alias".to_string(), "echo 'test' | sk ".to_string());
        env.write()
            .variables
            .insert("$FOO".to_string(), "BAR".to_string());

        let input = r#"alias abc " test" '-vvv' --foo "#.to_string();
        let replaced = expand_alias(input, Arc::clone(&env))?;
        assert_eq!(
            replaced,
            r#"echo 'test' | sk abc " test" '-vvv' --foo"#.to_string()
        );

        let input = r#"alias abc " test" '-vvv' --foo &"#.to_string();
        let replaced = expand_alias(input, Arc::clone(&env))?;
        assert_eq!(
            replaced,
            r#"echo 'test' | sk abc " test" '-vvv' --foo &"#.to_string()
        );

        let input = r#"alias | abc " test" '-vvv' --foo &"#.to_string();
        let replaced = expand_alias(input, Arc::clone(&env))?;
        assert_eq!(
            replaced,
            r#"echo 'test' | sk | abc " test" '-vvv' --foo"#.to_string()
        );

        let input = r#"sh -c | alias " test" '-vvv' --foo &"#.to_string();
        let replaced = expand_alias(input, Arc::clone(&env))?;
        assert_eq!(
            replaced,
            r#"sh -c | echo 'test' | sk " test" '-vvv' --foo"#.to_string()
        );

        let input = r#"echo (alias " test" '-vvv' --foo) "#.to_string();
        let replaced = expand_alias(input, Arc::clone(&env))?;
        assert_eq!(
            replaced,
            r#"echo ( echo 'test' | sk " test" '-vvv' --foo )"#.to_string()
        );
        let input = r#"echo $FOO"#.to_string();
        let replaced = expand_alias(input, Arc::clone(&env))?;
        assert_eq!(replaced, r#"echo BAR"#.to_string());

        let input = r#"echo 'test' > test.log"#.to_string();
        let replaced = expand_alias(input, Arc::clone(&env))?;
        assert_eq!(replaced, r#"echo 'test' > test.log"#.to_string());

        Ok(())
    }

    #[test]
    fn test_expand_alias_optimized() -> Result<()> {
        init();
        let env = crate::environment::Environment::new();

        env.write()
            .alias
            .insert("alias".to_string(), "echo 'test' | sk ".to_string());
        env.write()
            .variables
            .insert("$FOO".to_string(), "BAR".to_string());

        let mut ctx = StringOptimizationContext::new();

        let input = r#"alias abc " test" '-vvv' --foo "#.to_string();
        let replaced = expand_alias_optimized(input, Arc::clone(&env), &mut ctx)?;
        assert_eq!(
            replaced,
            r#"echo 'test' | sk abc " test" '-vvv' --foo"#.to_string()
        );

        let input = r#"echo $FOO"#.to_string();
        let replaced = expand_alias_optimized(input, Arc::clone(&env), &mut ctx)?;
        assert_eq!(replaced, r#"echo BAR"#.to_string());

        // Check cache statistics
        let stats = ctx.stats();
        println!("Optimization stats:");
        println!("  Tilde cache hits: {}", stats.tilde_cache_hits);
        println!("  Path cache hits: {}", stats.path_cache_hits);
        println!("  String pool reuses: {}", stats.string_pool_reuses);

        Ok(())
    }

    #[test]
    fn test_performance_comparison() {
        init();
        let env = crate::environment::Environment::new();

        env.write()
            .alias
            .insert("ls".to_string(), "exa -al".to_string());
        env.write().alias.insert("g".to_string(), "git".to_string());

        let test_commands = vec![
            "ls ~/Documents".to_string(),
            "g status".to_string(),
            "echo ~/test".to_string(),
        ];

        // Test original implementation
        let start = std::time::Instant::now();
        for _ in 0..100 {
            for cmd in &test_commands {
                let _ = expand_alias(cmd.clone(), Arc::clone(&env));
            }
        }
        let original_time = start.elapsed();

        // Test optimized implementation
        let mut ctx = StringOptimizationContext::new();
        let start = std::time::Instant::now();
        for _ in 0..100 {
            for cmd in &test_commands {
                let _ = expand_alias_optimized(cmd.clone(), Arc::clone(&env), &mut ctx);
            }
        }
        let optimized_time = start.elapsed();

        println!("Performance comparison:");
        println!("  Original: {:?}", original_time);
        println!("  Optimized: {:?}", optimized_time);

        let stats = ctx.stats();
        println!("  Tilde hit rate: {:.2}%", stats.tilde_hit_rate() * 100.0);
        println!("  Path hit rate: {:.2}%", stats.path_hit_rate() * 100.0);

        // The optimized version should be faster or at least not significantly slower
        let speedup_ratio = original_time.as_nanos() as f64 / optimized_time.as_nanos() as f64;
        println!("  Speedup ratio: {:.2}x", speedup_ratio);
    }

    #[test]
    fn parse_commands() {
        init();
        let pairs = ShellParser::parse(Rule::commands, "sleep 10 ; echo 'test' ")
            .unwrap_or_else(|e| panic!("{}", e));

        let mut result: Option<JobLink> = None;
        let mut root: Option<JobLink> = None;
        // let mut result: Option<JobLink> = None;

        for pair in pairs {
            for pair in pair.into_inner() {
                match pair.as_rule() {
                    Rule::command => {
                        debug!("{:?} {:?}", pair.as_rule(), pair.as_str());
                        let job = Job::new(pair.as_str().to_string());
                        match result.take() {
                            Some(prev) => {
                                prev.borrow_mut().next = Some(Rc::clone(&job));
                                result = Some(Rc::clone(&job));
                            }
                            None => {
                                result = Some(Rc::clone(&job));
                                root = Some(Rc::clone(&job));
                            }
                        }
                    }
                    Rule::command_list_sep => {}
                    _ => {}
                }
            }
        }

        debug!("{:?}", root);
    }

    #[test]
    fn parse_subshell() {
        init();
        let pairs = ShellParser::parse(Rule::commands, "sudo docker rm -v (sudo docker ps -a -q)")
            .unwrap_or_else(|e| panic!("{}", e));

        for pair in pairs {
            for pair in pair.into_inner() {
                match pair.as_rule() {
                    Rule::command => {
                        for pair in pair.into_inner() {
                            match pair.as_rule() {
                                Rule::simple_command => {
                                    for pair in pair.into_inner() {
                                        match pair.as_rule() {
                                            Rule::argv0 => {}
                                            Rule::args => {
                                                for pair in pair.into_inner() {
                                                    if pair.as_rule() == Rule::span {
                                                        for pair in pair.into_inner() {
                                                            if pair.as_rule() == Rule::subshell {
                                                                assert_eq!(
                                                                    pair.as_str(),
                                                                    "(sudo docker ps -a -q)"
                                                                )
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            _ => {}
                                        }
                                    }
                                }
                                _ => {
                                    println!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                                }
                            }
                        }
                    }
                    _ => {
                        println!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                    }
                }
            }
        }
    }

    #[test]
    fn parse_subshell2() {
        init();
        let sub = "(ls -al | wc -l)";
        let cmd = format!("echo {}", &sub);
        let pairs = ShellParser::parse(Rule::commands, &cmd).unwrap_or_else(|e| panic!("{}", e));

        for pair in pairs {
            for pair in pair.into_inner() {
                match pair.as_rule() {
                    Rule::command => {
                        for pair in pair.into_inner() {
                            match pair.as_rule() {
                                Rule::simple_command => {
                                    for pair in pair.into_inner() {
                                        match pair.as_rule() {
                                            Rule::argv0 => {}
                                            Rule::args => {
                                                for pair in pair.into_inner() {
                                                    if pair.as_rule() == Rule::span {
                                                        for pair in pair.into_inner() {
                                                            if pair.as_rule() == Rule::subshell {
                                                                assert_eq!(pair.as_str(), sub);
                                                                println!("{}", pair.as_str());
                                                                for pair in pair.into_inner() {
                                                                    println!(
                                                                        "{:?} {:?}",
                                                                        pair.as_rule(),
                                                                        pair.as_str()
                                                                    );
                                                                    for pair in pair.into_inner() {
                                                                        println!(
                                                                            "{:?} {:?}",
                                                                            pair.as_rule(),
                                                                            pair.as_str()
                                                                        );
                                                                        for pair in
                                                                            pair.into_inner()
                                                                        {
                                                                            println!(
                                                                                "{:?} {:?}",
                                                                                pair.as_rule(),
                                                                                pair.as_str()
                                                                            );
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            _ => {}
                                        }
                                    }
                                }
                                _ => {
                                    println!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                                }
                            }
                        }
                    }
                    _ => {
                        println!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                    }
                }
            }
        }
    }

    #[test]
    fn parse_proc_subst() {
        init();
        let pairs =
            ShellParser::parse(Rule::commands, "echo <(ls)").unwrap_or_else(|e| panic!("{}", e));

        for pair in pairs {
            for pair in pair.into_inner() {
                match pair.as_rule() {
                    Rule::command => {
                        for pair in pair.into_inner() {
                            match pair.as_rule() {
                                Rule::simple_command => {
                                    for pair in pair.into_inner() {
                                        match pair.as_rule() {
                                            Rule::argv0 => {}
                                            Rule::args => {
                                                for pair in pair.into_inner() {
                                                    if pair.as_rule() == Rule::span {
                                                        for pair in pair.into_inner() {
                                                            if pair.as_rule() == Rule::proc_subst {
                                                                assert_eq!(pair.as_str(), "<(ls)")
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            _ => {}
                                        }
                                    }
                                }
                                _ => {
                                    println!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                                }
                            }
                        }
                    }
                    _ => {
                        println!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                    }
                }
            }
        }
    }

    #[test]
    fn test_exec_subshell() {
        init();
        let pairs = ShellParser::parse(Rule::simple_command, r#"sleep (echo 1) "#)
            .unwrap_or_else(|e| panic!("{}", e));

        for pair in pairs {
            assert_eq!(Rule::simple_command, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);

            // let argv = get_argv(pair);
            // assert_eq!(2, argv.len());
            // assert_eq!("sleep", argv[0].0);
            // assert_eq!("(echo 1)", argv[1].0);
        }
    }

    #[test]
    fn test_variable() {
        init();
        let mut find = false;
        let pairs = ShellParser::parse(Rule::simple_command, r#"sleep $foo "#)
            .unwrap_or_else(|e| panic!("{}", e));

        for pair in pairs {
            assert_eq!(Rule::simple_command, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);
            for pair in pair.into_inner() {
                if pair.as_rule() == Rule::args {
                    for pair in pair.into_inner() {
                        for pair in pair.into_inner() {
                            assert_eq!(Rule::variable, pair.as_rule());
                            assert_eq!("$foo", pair.as_str());

                            find = true;
                        }
                    }
                }
            }
        }

        assert!(find);
    }

    #[test]
    fn test_redirect() {
        init();
        let pairs = ShellParser::parse(Rule::simple_command, r#"echo "f" > test.log "#)
            .unwrap_or_else(|e| panic!("{}", e));
        let mut found = false;
        for pair in pairs {
            assert_eq!(Rule::simple_command, pair.as_rule());
            // println!("* {:?} {:?}", pair.as_rule(), pair.as_str());
            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);
            for pair in pair.into_inner() {
                if pair.as_rule() == Rule::args {
                    for pair in pair.into_inner() {
                        // println!("** {:?} {:?}", pair.as_rule(), pair.as_str());
                        let parent = pair.as_rule();
                        if parent == Rule::redirect {
                            for pair in pair.into_inner() {
                                println!("*** {:?} {:?}", pair.as_rule(), pair.as_str());
                                found = true;
                            }
                        }
                    }
                }
            }
        }
        assert!(found);
    }

    #[test]
    fn test_redirect2() {
        init();
        let pairs = ShellParser::parse(Rule::command, r#"ls -al | wc -l > test.log "#)
            .unwrap_or_else(|e| panic!("{}", e));
        let mut found = false;
        for pair in pairs {
            // println!("* {:?} {:?}", pair.as_rule(), pair.as_str());
            for pair in pair.into_inner() {
                // println!("** {:?} {:?}", pair.as_rule(), pair.as_str());
                for pair in pair.into_inner() {
                    // println!("*** {:?} {:?}", pair.as_rule(), pair.as_str());
                    if pair.as_rule() == Rule::simple_command {
                        for pair in pair.into_inner() {
                            if pair.as_rule() == Rule::args {
                                for pair in pair.into_inner() {
                                    // println!(
                                    //     "**** {:?} {:?}",
                                    //     pair.as_rule(),
                                    //     pair.as_str()
                                    // );
                                    let parent = pair.as_rule();
                                    if parent == Rule::redirect {
                                        for _pair in pair.into_inner() {
                                            // println!(
                                            //     "**** {:?} {:?}",
                                            //     pair.as_rule(),
                                            //     pair.as_str()
                                            // );
                                            found = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(found);
    }

    #[test]
    fn parse_glob() {
        init();
        let pairs = ShellParser::parse(Rule::glob_word, "~/Downloads/*.pdf")
            .unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::glob_word, pair.as_rule());
            assert_eq!("~/Downloads/*.pdf", get_string(pair).unwrap());
        }

        let pairs = ShellParser::parse(Rule::simple_command, "ls ~/Downloads/*.pdf")
            .unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            debug!("{:?} {}", pair.as_rule(), pair.as_str());
            assert_eq!(Rule::simple_command, pair.as_rule());
            if Rule::simple_command == pair.as_rule() {
                for pair in pair.into_inner() {
                    debug!("{:?} {}", pair.as_rule(), pair.as_str());
                    if Rule::args == pair.as_rule() {
                        for pair in pair.into_inner() {
                            debug!("{:?} {}", pair.as_rule(), pair.as_str());
                            if Rule::span == pair.as_rule() {
                                for pair in pair.into_inner() {
                                    debug!("{:?} {}", pair.as_rule(), pair.as_str());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
