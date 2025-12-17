use super::{Rule, ShellParser};
use anyhow::{Result, anyhow};
use pest::iterators::{Pair, Pairs};
use pest::{Parser, Span};
use tracing::debug;

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
        Rule::span => {
            if let Some(inner) = pair.into_inner().next() {
                get_string(inner)
            } else {
                Some("".to_string())
            }
        }
        Rule::word | Rule::glob_word => {
            let s = pair.as_str();
            if !s.contains('\\') {
                return Some(s.to_string());
            }
            let mut res = String::with_capacity(s.len());
            let mut chars = s.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    if let Some(next) = chars.next() {
                        res.push(next);
                    } else {
                        // Trailing backslash? Treat as literal or ignore.
                        // Shells usually treat trailing backslash as line continuation,
                        // but here we are in a single string.
                        // For now just push it (or ignore?)
                        // If it parsed as escape_sequence, it MUST have a next char?
                        // escape_sequence = { "\\" ~ ANY }
                        // ANY matches any char including newline?
                        // If EOL, ANY fails?
                        // So a trailing backslash at VERY END of input might not parse as word?
                        // Let's just push it if it happens.
                        res.push('\\');
                    }
                } else {
                    res.push(c);
                }
            }
            Some(res)
        }
        _ => Some(pair.as_str().to_string()),
    }
}

pub fn get_pos_word(input: &str, pos: usize) -> Result<Option<(Rule, Span<'_>)>> {
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
            // println!("search_pos_word {:?} {:?}", pair.as_rule(), pair.as_str());
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

pub fn get_words_from_pairs<'a>(pairs: Pairs<'a, Rule>, pos: usize) -> Vec<(Rule, Span<'a>, bool)> {
    let mut result: Vec<(Rule, Span<'a>, bool)> = Vec::with_capacity(16);
    for pair in pairs {
        match pair.as_rule() {
            Rule::commands | Rule::command => {
                for pair in pair.into_inner() {
                    to_words(pair, pos, &mut result);
                }
            }
            _ => return result,
        }
    }
    result
}

pub fn get_words(input: &str, pos: usize) -> Result<Vec<(Rule, Span<'_>, bool)>> {
    let pairs = ShellParser::parse(Rule::commands, input).map_err(|e| anyhow!(e))?;
    Ok(get_words_from_pairs(pairs, pos))
}

fn to_words<'a>(pair: Pair<'a, Rule>, pos: usize, out: &mut Vec<(Rule, Span<'a>, bool)>) {
    match pair.as_rule() {
        Rule::argv0 => {
            for pair in pair.into_inner() {
                for pair in pair.into_inner() {
                    if let Some((span, current)) = get_span(pair, pos) {
                        out.push((Rule::argv0, span, current));
                    };
                }
            }
        }
        Rule::args => {
            for pair in pair.into_inner() {
                for pair in pair.into_inner() {
                    if let Some((span, current)) = get_span(pair, pos) {
                        out.push((Rule::args, span, current));
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
                            to_words(inner_pair, pos, out);
                        }
                    }
                    Rule::argv0 => {
                        for pair in inner_pair.into_inner() {
                            for pair in pair.into_inner() {
                                if let Some((span, current)) = get_span(pair, pos) {
                                    out.push((Rule::argv0, span, current));
                                };
                            }
                        }
                    }
                    Rule::args => {
                        for pair in inner_pair.into_inner() {
                            for pair in pair.into_inner() {
                                if let Some((span, current)) = get_span(pair, pos) {
                                    out.push((Rule::args, span, current));
                                };
                            }
                        }
                    }

                    Rule::commands | Rule::command => {
                        // Handle nested commands (with &&, ||, ;)
                        to_words(inner_pair, pos, out);
                    }
                    Rule::command_list_sep => {
                        // Skip command separators like &&, ||, ;
                    }
                    _ => {
                        // debug!(
                        //    "to_words missing {:?} {:?}",
                        //    inner_pair.as_rule(),
                        //    inner_pair.as_str()
                        //);
                    }
                }
            }
        }
    }
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
