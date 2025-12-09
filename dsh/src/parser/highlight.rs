use super::Rule;
use pest::Span;
use pest::error::InputLocation;
use pest::iterators::{Pair, Pairs};
use std::cmp::min;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    Command,
    Argument,
    Variable,
    SingleQuoted,
    DoubleQuoted,
    Redirect,
    Pipe,
    Operator,
    Background,
    ProcSubstitution,
    Error,
    Bareword,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightToken {
    pub start: usize,
    pub end: usize,
    pub kind: HighlightKind,
}

#[derive(Debug, Default, Clone)]
pub struct HighlightResult {
    pub tokens: Vec<HighlightToken>,
    pub error: Option<HighlightToken>,
}

pub fn collect_highlight_tokens_from_pairs(
    pairs: Pairs<Rule>,
    input_len: usize,
) -> HighlightResult {
    let mut result = HighlightResult::default();
    // Initialize Vec capacity with estimated token count
    // pairs.clone().count() is heavy, so use heuristic based on input length
    result.tokens.reserve(min(input_len / 5, 256));
    for pair in pairs {
        collect_highlight_from_pair(pair, HighlightContext::None, &mut result.tokens);
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HighlightContext {
    None,
    Command,
    Argument,
}

fn collect_highlight_from_pair(
    pair: Pair<Rule>,
    ctx: HighlightContext,
    out: &mut Vec<HighlightToken>,
) {
    match pair.as_rule() {
        Rule::argv0 => {
            for inner in pair.into_inner() {
                collect_highlight_from_pair(inner, HighlightContext::Command, out);
            }
        }
        Rule::args => {
            for inner in pair.into_inner() {
                collect_highlight_from_pair(inner, HighlightContext::Argument, out);
            }
        }
        Rule::word | Rule::glob_word => {
            let kind = match ctx {
                HighlightContext::Command => HighlightKind::Command,
                HighlightContext::Argument => HighlightKind::Argument,
                HighlightContext::None => HighlightKind::Bareword,
            };
            push_token(pair.as_span(), kind, out);
        }
        Rule::variable => push_token(pair.as_span(), HighlightKind::Variable, out),
        Rule::s_quoted => push_token(pair.as_span(), HighlightKind::SingleQuoted, out),
        Rule::d_quoted => push_token(pair.as_span(), HighlightKind::DoubleQuoted, out),
        Rule::stdout_redirect_direction
        | Rule::stderr_redirect_direction
        | Rule::stdouterr_redirect_direction
        | Rule::stdin_redirect_direction
        | Rule::stdin_redirect_direction_in => {
            push_token(pair.as_span(), HighlightKind::Redirect, out);
        }
        Rule::proc_subst_direction | Rule::proc_subst_direction_in => {
            push_token(pair.as_span(), HighlightKind::ProcSubstitution, out);
        }
        Rule::pipeline_op => push_token(pair.as_span(), HighlightKind::Pipe, out),
        Rule::background_op => push_token(pair.as_span(), HighlightKind::Background, out),
        Rule::and_op | Rule::or_op | Rule::sequential_op => {
            push_token(pair.as_span(), HighlightKind::Operator, out);
        }
        _ => {
            for inner in pair.into_inner() {
                collect_highlight_from_pair(inner, ctx, out);
            }
        }
    }
}

fn push_token(span: Span, kind: HighlightKind, out: &mut Vec<HighlightToken>) {
    let start = span.start();
    let end = span.end();
    if start == end {
        return;
    }
    out.push(HighlightToken { start, end, kind });
}

pub fn highlight_error_token(input: &str, location: InputLocation) -> Option<HighlightToken> {
    let len = input.len();
    match location {
        InputLocation::Pos(pos) => {
            if len == 0 {
                return None;
            }
            let mut start = pos.min(len);
            if start == len {
                start = start.saturating_sub(1);
            }
            let mut end = input[start..]
                .chars()
                .next()
                .map(|ch| start + ch.len_utf8())
                .unwrap_or(start);
            if end <= start {
                end = (start + 1).min(len);
            }
            Some(HighlightToken {
                start,
                end,
                kind: HighlightKind::Error,
            })
        }
        InputLocation::Span((start, end)) => {
            if len == 0 {
                return None;
            }
            let s = start.min(len.saturating_sub(1));
            let mut e = end.min(len);
            if e <= s {
                e = (s + 1).min(len);
            }
            Some(HighlightToken {
                start: s,
                end: e,
                kind: HighlightKind::Error,
            })
        }
    }
}
