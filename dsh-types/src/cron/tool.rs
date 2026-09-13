//! What the `cron_manage` chat tool asks the host to do.
//!
//! Lives here, not in `dsh-builtin` or `dsh`, because both need to name the
//! same shape: `dsh-builtin` parses the tool call's JSON into this and enforces
//! confirmation and grant limits before dispatching it; `dsh` turns it into the
//! same argv `dsh/src/cron/cli/parse.rs` already validates, so nothing about
//! schedule parsing, grant canonicalisation or the duplicate-name rule is
//! reimplemented for this third entry point. Every field is a raw string (or a
//! list of them) for the same reason - a value not yet parsed is a value that
//! has not yet been validated twice in two different ways.
//!
//! `CronToolAction` intentionally has no `notepad` variant: a job's notepad
//! directory is already in its own `read`/`write` grant
//! (`dsh/src/cron/run_job.rs::notepad_grant`), so the model reads and writes
//! it with the `read_file`/`edit` tools it already has. Adding a tool action
//! for it would be a second way to do something one already covers.

/// One `cron_manage` call, before it is turned into `cron`'s own argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CronToolAction {
    List,
    Show,
    History,
    Incidents,
    Status,
    Doctor,
    Create,
    Update,
    Pause,
    Resume,
    Remove,
    Run,
    Ack,
}

impl CronToolAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Show => "show",
            Self::History => "history",
            Self::Incidents => "incidents",
            Self::Status => "status",
            Self::Doctor => "doctor",
            Self::Create => "create",
            Self::Update => "update",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Remove => "remove",
            Self::Run => "run",
            Self::Ack => "ack",
        }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "list" => Self::List,
            "show" => Self::Show,
            "history" => Self::History,
            "incidents" => Self::Incidents,
            "status" => Self::Status,
            "doctor" => Self::Doctor,
            "create" => Self::Create,
            "update" => Self::Update,
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            "remove" => Self::Remove,
            "run" => Self::Run,
            "ack" => Self::Ack,
            _ => return Err(format!("{name}: unknown cron_manage action")),
        })
    }

    /// Whether this action changes a job (or acknowledges an incident) rather
    /// than only reading store state.
    ///
    /// Every write action asks the user first (`confirm_agent_action`); under
    /// an unattended task that always means `TaskStatus::InputRequired`, the
    /// same halt any other tool's write already produces there.
    pub fn is_write(self) -> bool {
        matches!(
            self,
            Self::Create
                | Self::Update
                | Self::Pause
                | Self::Resume
                | Self::Remove
                | Self::Run
                | Self::Ack
        )
    }
}

impl std::fmt::Display for CronToolAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One `cron_manage` call, parameters flattened across every action.
///
/// Which fields matter depends on `action`; unused ones are simply left at
/// their default. `dsh::cron::tool::impl CronToolHost for Shell` is the only
/// place that reads them, and it does so by building the same argv a person
/// would type and handing it to `cli::parse_add`/`parse_edit`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CronToolRequest {
    pub action: Option<CronToolAction>,
    /// Selector for every action but `create`: a job name or id.
    pub job: Option<String>,
    /// `--name` on `create`/`update`.
    pub name: Option<String>,
    /// The schedule string (`create`'s positional argument, `update`'s
    /// `--schedule`).
    pub schedule: Option<String>,
    /// A shell job's command line.
    pub command: Option<String>,
    /// An agent job's goal (`create --agent` / `update --goal`).
    pub goal: Option<String>,
    pub cwd: Option<String>,
    /// Whether `create` registers an agent job rather than a shell job.
    pub agent: bool,
    pub tokens: Option<String>,
    pub max_tokens_per_day: Option<String>,
    /// `--check`, repeatable.
    pub check: Vec<String>,
    pub on: Option<String>,
    pub timeout: Option<String>,
    pub catchup: Option<String>,
    pub read: Vec<String>,
    pub write: Vec<String>,
    pub allow_command: Vec<String>,
    pub allow_mcp: Vec<String>,
    pub network: Vec<String>,
    pub env: Vec<String>,
    pub sandbox: bool,
    /// `create`: replace an existing job of the same name.
    pub force: bool,
    /// `history`.
    pub limit: Option<String>,
    /// `history --failed`.
    pub failed: bool,
    /// `ack`.
    pub incident_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_round_trips_through_its_spelling() {
        for action in [
            CronToolAction::List,
            CronToolAction::Show,
            CronToolAction::History,
            CronToolAction::Incidents,
            CronToolAction::Status,
            CronToolAction::Doctor,
            CronToolAction::Create,
            CronToolAction::Update,
            CronToolAction::Pause,
            CronToolAction::Resume,
            CronToolAction::Remove,
            CronToolAction::Run,
            CronToolAction::Ack,
        ] {
            assert_eq!(CronToolAction::parse(action.as_str()), Ok(action));
        }
    }

    #[test]
    fn an_unknown_action_is_an_error() {
        assert!(CronToolAction::parse("delete-everything").is_err());
    }

    #[test]
    fn only_the_actions_that_change_something_need_confirmation() {
        for action in [
            CronToolAction::List,
            CronToolAction::Show,
            CronToolAction::History,
            CronToolAction::Incidents,
            CronToolAction::Status,
            CronToolAction::Doctor,
        ] {
            assert!(!action.is_write(), "{action} should not need confirmation");
        }
        for action in [
            CronToolAction::Create,
            CronToolAction::Update,
            CronToolAction::Pause,
            CronToolAction::Resume,
            CronToolAction::Remove,
            CronToolAction::Run,
            CronToolAction::Ack,
        ] {
            assert!(action.is_write(), "{action} should need confirmation");
        }
    }
}
