//! Side-effect-free execution plan: what parsing produces before anything runs.
//!
//! `ExecutionPlan` is pure data (no pids, fds, processes). The evaluator gates
//! on `&&`/`||` first, then materializes only the selected jobs, authorizing
//! each substitution body and each final argv through `SafetyGuard`.
//!
//! Planning preserves word structure and performs no runtime word expansion.
//! Alias rewriting is syntax-time only. Variable, tilde, brace/glob and
//! substitution expansion happen only when a selected job is materialized.
//! The final concrete argv is authorized after expansion.

use crate::process::ListOp;
use crate::process::SubshellType;
use crate::process::redirect::RedirectOp as ConcreteRedirectOp;
use std::os::unix::io::RawFd;

/// A whole input line, parsed without running anything.
#[derive(Debug, Clone, Default)]
pub struct ExecutionPlan {
    pub jobs: Vec<PlannedJob>,
}

impl ExecutionPlan {
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

/// One `;`/`&&`/`||`-separated job: a pipeline plus its gating and flags.
#[derive(Debug, Clone)]
pub struct PlannedJob {
    /// User-facing source text (for safety messages and `Job.cmd`).
    pub source: String,
    /// Pipeline stages in execution order.
    pub stages: Vec<PlannedCommand>,
    /// Separator after this job: None / && / ||.
    pub list_op: ListOp,
    pub foreground: bool,
    pub capture_output: bool,
    pub struct_pipe_exprs: Vec<String>,
    pub subshell: SubshellType,
}

impl PlannedJob {
    pub fn contains_dynamic_expansion(&self) -> bool {
        self.stages.iter().any(|stage| {
            stage.argv.iter().any(|word| word.is_dynamic())
                || stage.redirects.iter().any(|redirect| redirect.is_dynamic())
                || stage
                    .env_overrides
                    .iter()
                    .any(|assignment| assignment.is_dynamic())
        })
    }

    pub fn is_assignment_only(&self) -> bool {
        !self.stages.is_empty()
            && self
                .stages
                .iter()
                .all(|stage| stage.argv.is_empty() && !stage.env_overrides.is_empty())
    }
}

/// One pipeline stage: word templates plus per-process redirections and env.
#[derive(Debug, Clone)]
pub struct PlannedCommand {
    pub argv: Vec<PlannedWord>,
    pub redirects: Vec<PlannedRedirect>,
    pub env_overrides: Vec<PlannedAssignment>,
}

impl PlannedCommand {
    pub fn is_empty(&self) -> bool {
        self.argv.is_empty() && self.redirects.is_empty() && self.env_overrides.is_empty()
    }
}

/// Whether a part of a word was quoted, which decides if its value may split,
/// glob, or expand a tilde later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteMode {
    Unquoted,
    Single,
    Double,
}

/// One shell word from the source: the parts that together form it.
///
/// A `PlannedWord` is not one final argv entry. Runtime expansion may turn it
/// into zero, one, or many fields.
#[derive(Debug, Clone)]
pub struct PlannedWord {
    /// User source for diagnostics/tests.
    pub source: String,
    /// Parts that together form one shell word.
    pub parts: Vec<WordPart>,
}

impl PlannedWord {
    pub fn empty() -> Self {
        Self {
            source: String::new(),
            parts: Vec::new(),
        }
    }

    pub fn is_dynamic(&self) -> bool {
        self.parts.iter().any(|part| match part {
            WordPart::Literal(literal) => {
                literal.tilde_candidate || literal.pattern_active || literal.brace_active
            }
            WordPart::Variable { .. } | WordPart::Substitution { .. } => true,
        })
    }
}

/// A fragment of a word: static text, a deferred variable, or a deferred
/// substitution body.
#[derive(Debug, Clone)]
pub enum WordPart {
    Literal(PlannedLiteral),
    Variable {
        source: String,
        quote: QuoteMode,
    },
    Substitution {
        substitution: PlannedSubstitution,
        quote: QuoteMode,
    },
}

/// Static text inside a word, with the flags runtime expansion needs.
#[derive(Debug, Clone)]
pub struct PlannedLiteral {
    /// Quote/escape-removed text for argv.
    pub text: String,
    /// Raw spelling for pattern generation.
    pub raw: String,
    pub quote: QuoteMode,
    /// Whether `* ? [` from this part may match pathnames.
    pub pattern_active: bool,
    /// Whether a source brace pattern from this part may expand.
    pub brace_active: bool,
    /// Whether this part is a bare leading `~` tilde candidate.
    pub tilde_candidate: bool,
}

/// A deferred substitution body: its own plan, evaluated only after gating
/// and authorization.
#[derive(Debug, Clone)]
pub struct PlannedSubstitution {
    pub source: String,
    pub kind: SubshellType,
    pub plan: Box<ExecutionPlan>,
}

/// One `NAME=value` prefix or standalone assignment. The value stays a word
/// until the selected job is materialized.
#[derive(Debug, Clone)]
pub struct PlannedAssignment {
    pub name: String,
    pub value: PlannedWord,
}

impl PlannedAssignment {
    pub fn is_dynamic(&self) -> bool {
        self.value.is_dynamic()
    }
}

/// One redirection in source order. File targets stay words until the
/// selected job is materialized.
#[derive(Debug, Clone)]
pub struct PlannedRedirect {
    pub fd: RawFd,
    pub op: PlannedRedirectOp,
}

impl PlannedRedirect {
    pub fn is_dynamic(&self) -> bool {
        match &self.op {
            PlannedRedirectOp::ReadFile(word)
            | PlannedRedirectOp::WriteFile(word)
            | PlannedRedirectOp::AppendFile(word)
            | PlannedRedirectOp::BothWrite(word)
            | PlannedRedirectOp::BothAppend(word) => word.is_dynamic(),
            PlannedRedirectOp::DupFrom(_) | PlannedRedirectOp::Close => false,
        }
    }

    /// Expand the target word once and build the concrete redirects.
    /// `&>` forms produce two entries sharing one expansion.
    pub fn to_concrete(&self, target: String) -> Vec<crate::process::Redirect> {
        use crate::process::Redirect;
        match &self.op {
            PlannedRedirectOp::ReadFile(_) => vec![Redirect {
                fd: self.fd,
                op: ConcreteRedirectOp::ReadFile(target),
            }],
            PlannedRedirectOp::WriteFile(_) => vec![Redirect {
                fd: self.fd,
                op: ConcreteRedirectOp::WriteFile(target),
            }],
            PlannedRedirectOp::AppendFile(_) => vec![Redirect {
                fd: self.fd,
                op: ConcreteRedirectOp::AppendFile(target),
            }],
            PlannedRedirectOp::BothWrite(_) => Redirect::both(target, false),
            PlannedRedirectOp::BothAppend(_) => Redirect::both(target, true),
            PlannedRedirectOp::DupFrom(_) | PlannedRedirectOp::Close => Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum PlannedRedirectOp {
    ReadFile(PlannedWord),
    WriteFile(PlannedWord),
    AppendFile(PlannedWord),
    BothWrite(PlannedWord),
    BothAppend(PlannedWord),
    DupFrom(RawFd),
    Close,
}
