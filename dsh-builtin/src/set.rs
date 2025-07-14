use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use getopts::Options;

fn print_usage(ctx: &Context, cmd_name: &str, opts: Options) {
    let brief = format!("Usage: {cmd_name} [OPTIONS] KEY VALUE");
    ctx.write_stdout(&opts.usage(&brief)).ok();
}

pub fn command(ctx: &Context, args: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let cmd_name = args[0].clone();
    let mut opts = Options::new();
    opts.optflag("x", "export", "exported environment variable");
    opts.optflag("h", "help", "print this help menu");
    let matches = match opts.parse(&args[1..]) {
        Ok(m) => m,
        Err(err) => {
            ctx.write_stderr(&format!("{err}")).ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    if matches.opt_present("h") || matches.free.len() != 2 {
        print_usage(ctx, &cmd_name, opts);
        return ExitStatus::ExitedWith(0);
    }

    let key = matches.free[0].clone();
    let val = matches.free[1].clone();

    if matches.opt_present("x") {
        // export environment variable
        proxy.set_env_var(key, val);
    } else {
        proxy.set_var(key, val);
    }
    ExitStatus::ExitedWith(0)
}
