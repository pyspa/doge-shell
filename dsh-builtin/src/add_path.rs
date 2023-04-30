use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

pub fn command(_ctx: &Context, args: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    proxy.insert_path(0, &args[1]);
    ExitStatus::ExitedWith(0)
}
