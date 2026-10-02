//! Logical word-expansion state borrows the existing runtime owner.
use crate::environment::Environment;
use crate::shell::Shell;
use parking_lot::RwLock;
use std::sync::Arc;

/// Expansion reads/writes logical state; only runtime() may access the
/// existing session, process groups and resource registry. Hosts never own
/// or replace a Shell, including on errors and future cancellation.
pub trait ExpansionHost {
    fn expansion_environment(&self) -> &Arc<RwLock<Environment>>;
    fn runtime(&mut self) -> &mut Shell;
}
impl ExpansionHost for Shell {
    fn expansion_environment(&self) -> &Arc<RwLock<Environment>> {
        &self.environment
    }
    fn runtime(&mut self) -> &mut Shell {
        self
    }
}

pub(crate) struct StageExpansionHost<'a> {
    pub environment: Arc<RwLock<Environment>>,
    runtime: &'a mut Shell,
}
impl<'a> StageExpansionHost<'a> {
    pub fn new(runtime: &'a mut Shell, environment: Arc<RwLock<Environment>>) -> Self {
        Self {
            environment,
            runtime,
        }
    }
}
impl ExpansionHost for StageExpansionHost<'_> {
    fn expansion_environment(&self) -> &Arc<RwLock<Environment>> {
        &self.environment
    }
    fn runtime(&mut self) -> &mut Shell {
        self.runtime
    }
}
