//! Redirect planning: one `redirect` pair becomes `PlannedRedirect`s.
//!
//! Covers file redirects (`>`, `>>`, `<`, `2>`, `&>`, numbered `N>`/`N<`)
//! and fd duplication (`N>&M`, `N>&-`). Pure: targets stay unexpanded
//! `PlannedWord`s until the job is materialized.

use super::{ParseContext, parse_word};
use crate::parser::Rule;
use crate::process::Redirect;
use crate::shell::plan::{PlannedRedirect, PlannedRedirectOp};
use anyhow::{Context as _, Result};
use nix::libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use pest::iterators::Pair;
use std::os::unix::io::RawFd;

/// Build the redirections one `redirect` pair stands for (pure, no resolution).
pub(super) fn parse_redirect(pair: Pair<Rule>, ctx: &ParseContext) -> Result<Vec<PlannedRedirect>> {
    let mut direction = None;
    let mut numbered_fd = None;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::fd_dup => return parse_fd_dup(inner),
            Rule::fd_redirect_direction => {
                for part in inner.into_inner() {
                    match part.as_rule() {
                        Rule::fd_number => numbered_fd = Some(parse_fd(part.as_str())?),
                        rule => direction = Some(rule),
                    }
                }
            }
            Rule::stdout_redirect_direction
            | Rule::stderr_redirect_direction
            | Rule::stdouterr_redirect_direction
            | Rule::stdin_redirect_direction => {
                direction = inner.into_inner().next().map(|rule| rule.as_rule());
            }
            Rule::span => {
                let target = parse_word(inner, ctx)?;
                return Ok(match direction {
                    Some(Rule::stdout_redirect_direction_out) => {
                        vec![PlannedRedirect {
                            fd: STDOUT_FILENO,
                            op: PlannedRedirectOp::WriteFile(target),
                        }]
                    }
                    Some(Rule::stdout_redirect_direction_append) => {
                        vec![PlannedRedirect {
                            fd: STDOUT_FILENO,
                            op: PlannedRedirectOp::AppendFile(target),
                        }]
                    }
                    Some(Rule::stderr_redirect_direction_out) => {
                        vec![PlannedRedirect {
                            fd: STDERR_FILENO,
                            op: PlannedRedirectOp::WriteFile(target),
                        }]
                    }
                    Some(Rule::stderr_redirect_direction_append) => {
                        vec![PlannedRedirect {
                            fd: STDERR_FILENO,
                            op: PlannedRedirectOp::AppendFile(target),
                        }]
                    }
                    Some(Rule::stdouterr_redirect_direction_out) => vec![PlannedRedirect {
                        fd: STDOUT_FILENO,
                        op: PlannedRedirectOp::BothWrite(target),
                    }],
                    Some(Rule::stdouterr_redirect_direction_append) => vec![PlannedRedirect {
                        fd: STDOUT_FILENO,
                        op: PlannedRedirectOp::BothAppend(target),
                    }],
                    Some(Rule::stdin_redirect_direction_in) => vec![PlannedRedirect {
                        fd: STDIN_FILENO,
                        op: PlannedRedirectOp::ReadFile(target),
                    }],
                    Some(Rule::fd_redirect_out) => vec![PlannedRedirect {
                        fd: numbered_fd.unwrap_or(STDOUT_FILENO),
                        op: PlannedRedirectOp::WriteFile(target),
                    }],
                    Some(Rule::fd_redirect_append) => vec![PlannedRedirect {
                        fd: numbered_fd.unwrap_or(STDOUT_FILENO),
                        op: PlannedRedirectOp::AppendFile(target),
                    }],
                    Some(Rule::fd_redirect_in) => vec![PlannedRedirect {
                        fd: numbered_fd.unwrap_or(STDIN_FILENO),
                        op: PlannedRedirectOp::ReadFile(target),
                    }],
                    _ => Vec::new(),
                });
            }
            _ => {}
        }
    }

    Ok(Vec::new())
}

/// `2>&1`, `>&2`, `2>&-`.
fn parse_fd_dup(pair: Pair<Rule>) -> Result<Vec<PlannedRedirect>> {
    let Some(form) = pair.into_inner().next() else {
        return Ok(Vec::new());
    };
    let default_fd = match form.as_rule() {
        Rule::fd_dup_in => STDIN_FILENO,
        _ => STDOUT_FILENO,
    };

    let mut fd = default_fd;
    let mut target = None;
    for inner in form.into_inner() {
        match inner.as_rule() {
            Rule::fd_number => fd = parse_fd(inner.as_str())?,
            Rule::fd_dup_target => target = Some(inner.as_str().to_string()),
            _ => {}
        }
    }

    let Some(target) = target else {
        return Ok(Vec::new());
    };
    Ok(vec![if target == "-" {
        PlannedRedirect {
            fd,
            op: PlannedRedirectOp::Close,
        }
    } else {
        PlannedRedirect {
            fd,
            op: PlannedRedirectOp::DupFrom(parse_fd(&target)?),
        }
    }])
}

fn parse_fd(text: &str) -> Result<RawFd> {
    text.parse::<RawFd>()
        .with_context(|| format!("dsh: invalid file descriptor '{text}'"))
}

/// Convert a concrete planned redirect back for dry assembly.
pub(crate) fn planned_to_concrete(redirect: &PlannedRedirect, target: String) -> Vec<Redirect> {
    match &redirect.op {
        PlannedRedirectOp::DupFrom(from) => vec![Redirect::dup(redirect.fd, *from)],
        PlannedRedirectOp::Close => vec![Redirect::close(redirect.fd)],
        _ => redirect.to_concrete(target),
    }
}
