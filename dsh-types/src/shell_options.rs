//! Shell option domain for POSIX-compatible `set -o` / `set +o` control.
//!
//! `ShellOption` is the closed set of option names the shell understands
//! (today only `pipefail`); `ShellOptions` is the copyable runtime state
//! `Environment` owns. Callers resolve names through `ShellOption::parse`
//! and query state through `ShellOptions::enabled`, never by comparing
//! `"pipefail"` strings ad hoc, so adding a second option only extends
//! `ShellOption::ALL`.
//!
//! `Default` is pipefail OFF, preserving the historical pipeline behavior
//! (pipeline status is the tail stage's status) until `set -o pipefail`
//! opts in.

use serde::{Deserialize, Serialize};

/// A shell option controllable via `set -o` / `set +o`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShellOption {
    Pipefail,
}

impl ShellOption {
    /// All known options in deterministic display order.
    pub const ALL: [ShellOption; 1] = [ShellOption::Pipefail];

    pub const fn name(self) -> &'static str {
        match self {
            ShellOption::Pipefail => "pipefail",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "pipefail" => Some(Self::Pipefail),
            _ => None,
        }
    }
}

/// Runtime state for `set -o` options.
///
/// Copyable by design: `Environment::extend`, the Lisp rollback snapshot,
/// and the re-exec `ChildShellSnapshot` all duplicate this value so no two
/// owners share mutable option state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellOptions {
    pipefail: bool,
}

impl ShellOptions {
    pub fn enabled(&self, option: ShellOption) -> bool {
        match option {
            ShellOption::Pipefail => self.pipefail,
        }
    }

    pub fn set(&mut self, option: ShellOption, enabled: bool) {
        match option {
            ShellOption::Pipefail => self.pipefail = enabled,
        }
    }

    pub fn pipefail(&self) -> bool {
        self.pipefail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_pipefail_off() {
        let options = ShellOptions::default();
        assert!(!options.enabled(ShellOption::Pipefail));
        assert!(!options.pipefail());
    }

    #[test]
    fn set_toggles_pipefail() {
        let mut options = ShellOptions::default();
        options.set(ShellOption::Pipefail, true);
        assert!(options.enabled(ShellOption::Pipefail));
        options.set(ShellOption::Pipefail, false);
        assert!(!options.enabled(ShellOption::Pipefail));
    }

    #[test]
    fn option_names_round_trip() {
        assert_eq!(ShellOption::Pipefail.name(), "pipefail");
        assert_eq!(ShellOption::parse("pipefail"), Some(ShellOption::Pipefail));
        assert_eq!(ShellOption::parse("errexit"), None);
        assert_eq!(ShellOption::ALL, [ShellOption::Pipefail]);
    }
}
