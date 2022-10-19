use crate::builtin::cd;
use crate::process::{Context, ExitStatus};
use crate::shell::Shell;

pub fn command(_ctx: &Context, argv: Vec<String>, shell: &mut Shell) -> ExitStatus {
    let path = argv.get(1).map(|s| s.as_str()).unwrap_or("");

    if let Some(ref mut history) = shell.path_history {
        if let Some(ref path) = history.search_fuzzy_first(path) {
            return cd::move_dir(path, shell);
        }
    }
    ExitStatus::ExitedWith(0)
}
