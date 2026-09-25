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
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionPlan {
    pub lists: Vec<PlannedAndOrList>,
}

impl ExecutionPlan {
    pub fn is_empty(&self) -> bool {
        self.lists.iter().all(|list| list.jobs.is_empty())
    }

    /// Flattened job projection for static inspection (safety checks,
    /// tests). Never starts async execution.
    pub fn iter_jobs(&self) -> impl Iterator<Item = &PlannedJob> {
        self.lists.iter().flat_map(|list| list.jobs.iter())
    }

    pub fn first_job_mut(&mut self) -> Option<&mut PlannedJob> {
        self.lists.iter_mut().find_map(|list| list.jobs.first_mut())
    }
}

/// One AND-OR list: `&&`/`||`-gated pipelines sharing one execution
/// environment, separated from the next list by `;` (foreground) or `&`
/// (asynchronous). `&` is ownership of the whole list, never a
/// per-command flag.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlannedAndOrList {
    /// User-facing source text of the list body (without the separator).
    pub source: String,
    /// `&&`/`||`-gated pipelines in execution order.
    pub jobs: Vec<PlannedJob>,
    pub execution: ListExecutionMode,
}

/// Whether an AND-OR list runs inline or as an isolated background helper.
///
/// `&&`/`||` gating (`ListOp`) stays intra-list control flow on a separate
/// axis; it is never mixed into this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ListExecutionMode {
    Foreground,
    Asynchronous,
}

impl PlannedAndOrList {
    /// The body a helper evaluates: the same list, normalized to foreground
    /// so the helper runs it as an ordinary chain instead of re-spawning
    /// itself asynchronously.
    pub fn isolated_body_plan(&self) -> ExecutionPlan {
        let mut body = self.clone();
        body.execution = ListExecutionMode::Foreground;
        ExecutionPlan { lists: vec![body] }
    }

    /// User-facing source for the job table and notices.
    pub fn display_source(&self) -> String {
        match self.execution {
            ListExecutionMode::Foreground => self.source.clone(),
            ListExecutionMode::Asynchronous => format!("{} &", self.source),
        }
    }
}

/// One `&&`/`||`-gated pipeline: stages plus its gating and flags.
///
/// `ListOp` gates the *next* pipeline inside the same AND-OR list. There is
/// no background flag here: a job always runs in the foreground of its own
/// list's execution environment, and asynchrony belongs to
/// [`PlannedAndOrList::execution`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlannedJob {
    /// User-facing source text (for safety messages and `Job.cmd`).
    pub source: String,
    /// Pipeline stages in execution order.
    pub stages: Vec<PlannedCommand>,
    /// Separator after this job: None / && / ||.
    pub list_op: ListOp,
    pub capture_output: bool,
    pub struct_pipe_exprs: Vec<String>,
    pub subshell: SubshellType,
    /// Synthetic pipeline head (Smart Pipe previous output). `None` for
    /// ordinary jobs; `Some(PreviousOutput)` makes materialization prepend
    /// a `SyntheticSource` stage carrying the history bytes.
    #[serde(default)]
    pub pipeline_source: Option<PlannedPipelineSource>,
}

/// A synthetic pipeline head: finite bytes, not a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlannedPipelineSource {
    PreviousOutput,
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum QuoteMode {
    Unquoted,
    Single,
    Double,
}

/// One shell word from the source: the parts that together form it.
///
/// A `PlannedWord` is not one final argv entry. Runtime expansion may turn it
/// into zero, one, or many fields.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlannedSubstitution {
    pub source: String,
    pub kind: PlannedSubstitutionKind,
    pub plan: Box<ExecutionPlan>,
}

/// Direction of a `<(...)` / `>(...)` process substitution.
///
/// This is pure execution-plan data, distinct from [`SubshellType`]: the
/// latter classifies the helper's execution environment (identical for both
/// directions), while this records which way bytes flow through `/dev/fd/N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProcessSubstitutionDirection {
    /// `<(...)`: outer command reads from the special fd.
    Read,
    /// `>(...)`: outer command writes to the special fd.
    Write,
}

/// Type-safe classification of a deferred substitution body.
///
/// `Option<ProcessSubstitutionDirection>` paired with `SubshellType` could
/// express invalid states (e.g. `CommandSubstitution` + `Write`), so the
/// direction lives only on the `Process` variant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlannedSubstitutionKind {
    Command,
    Subshell,
    Process(ProcessSubstitutionDirection),
}

/// One `NAME=value` prefix or standalone assignment. The value stays a word
/// until the selected job is materialized.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlannedRedirectOp {
    ReadFile(PlannedWord),
    WriteFile(PlannedWord),
    AppendFile(PlannedWord),
    BothWrite(PlannedWord),
    BothAppend(PlannedWord),
    DupFrom(RawFd),
    Close,
}
