//! Immutable expansion state carried by one concrete pipeline stage.
use crate::environment::{Environment, child_snapshot::ChildShellSnapshot};
use parking_lot::RwLock;
use std::sync::Arc;

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) enum StageEnvironment {
    #[default]
    Current,
    Isolated(Box<ChildShellSnapshot>),
}
impl std::fmt::Debug for StageEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Current => "Current",
            Self::Isolated(_) => "Isolated",
        })
    }
}
impl StageEnvironment {
    pub fn snapshot(&self, current: &Arc<RwLock<Environment>>) -> ChildShellSnapshot {
        match self {
            Self::Current => ChildShellSnapshot::capture(&current.read()),
            Self::Isolated(snapshot) => (**snapshot).clone(),
        }
    }
    pub fn environment(&self, current: &Arc<RwLock<Environment>>) -> Arc<RwLock<Environment>> {
        match self {
            Self::Current => current.clone(),
            Self::Isolated(snapshot) => {
                let env = Environment::isolated_expansion(&current.read());
                let mut guard = env.write();
                // Only logical execution projections: no chdir or live session mutation.
                guard.variable_state.variables = snapshot.variables.clone();
                guard.variable_state.exported_vars = snapshot.exported_vars.clone();
                guard.variable_state.paths = snapshot.paths.clone();
                drop(guard);
                env
            }
        }
    }
}
