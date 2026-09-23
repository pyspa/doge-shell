//! `ShellOptionCapability for Shell`: `set -o` state access.
//!
//! Split from `shell_proxy.rs` because that trait impl cannot be spread
//! across files: this keeps the frozen `ShellProxy` surface untouched while
//! option state lives behind its own capability trait.

use crate::shell::Shell;
use dsh_types::shell_options::ShellOption;

impl dsh_builtin::shell_capabilities::ShellOptionCapability for Shell {
    fn shell_option_enabled(&self, option: ShellOption) -> bool {
        self.environment.read().shell_options.enabled(option)
    }

    fn set_shell_option(&mut self, option: ShellOption, enabled: bool) {
        self.environment.write().shell_options.set(option, enabled);
    }
}
