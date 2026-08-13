use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;

pub fn execute(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    let limit = argv
        .get(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(20);
    let failed = argv.get(2).is_some_and(|value| value == "true");
    let json = argv.get(3).is_some_and(|value| value == "true");
    let Some(history) = shell.cmd_history.as_ref() else {
        ctx.write_stdout(if json {
            "[]"
        } else {
            "No persistent command blocks available."
        })?;
        return Ok(());
    };
    let history = history.lock();
    let events = history.command_events_filtered(Some("all"), limit, failed)?;
    if json {
        ctx.write_stdout(&serde_json::to_string(&events)?)?;
    } else if events.is_empty() {
        ctx.write_stdout("No persistent command blocks available.")?;
    } else {
        for (offset, event) in events.iter().enumerate() {
            ctx.write_stdout(&format!(
                "{:>5}  {:>4}  {:>8}  {:<12} {}",
                offset + 1,
                event.exit_code.unwrap_or_default(),
                event.duration_ms.unwrap_or_default(),
                event.author,
                event.command
            ))?;
        }
    }
    Ok(())
}
