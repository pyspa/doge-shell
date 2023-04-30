use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

pub fn command(_ctx: &Context, args: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let path = shellexpand::tilde(&args[1]);
    proxy.insert_path(0, &path);
    ExitStatus::ExitedWith(0)
}
