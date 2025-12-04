use crate::ShellProxy;
use dsh_types::{Context, ExitStatus};

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Case 1: `export` (no arguments) -> print all exported variables
    if argv.len() == 1 {
        let vars = proxy.list_exported_vars();
        let mut output = String::new();
        for (key, value) in vars {
            output.push_str(&format!("declare -x {}=\"{}\"\n", key, value));
        }
        // Remove the last newline before writing
        if let Some(s) = output.strip_suffix('\n')
            && ctx.write_stdout(s).is_err()
        {
            return ExitStatus::ExitedWith(1);
        }
        return ExitStatus::ExitedWith(0);
    }

    // Case 2: `export VAR=value` or `export VAR`
    for arg in &argv[1..] {
        if let Some((var, value)) = arg.split_once('=') {
            // `export VAR=value`
            proxy.set_and_export_var(var.to_string(), value.to_string());
        } else {
            // `export VAR`
            proxy.export_var(arg);
        }
    }

    ExitStatus::ExitedWith(0)
}

pub fn description() -> &'static str {
    "Set export attribute for shell variables"
}
