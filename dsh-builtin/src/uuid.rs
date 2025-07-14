use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use uuid::Uuid;

pub fn command(ctx: &Context, _args: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    let id = Uuid::new_v4();
    match ctx.write_stdout(&id.to_string()) {
        Err(err) => {
            let _ = ctx.write_stderr(&format!("uuid: {err}")); // TODO err check
            ExitStatus::ExitedWith(1)
        }
        _ => ExitStatus::ExitedWith(0),
    }
}
