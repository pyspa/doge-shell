use super::ShellProxy;
use anyhow::{Context as _, Result};
use dsh_types::{Context, ExitStatus};
use std::fs::File;
use std::io::{Read, Write};
use std::mem;
use std::os::unix::io::FromRawFd;

pub fn description() -> &'static str {
    "Pipe content to external editor"
}

pub fn command(ctx: &Context, _argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match pipe_to_editor(ctx, proxy) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            let _ = ctx.write_stderr(&format!("eview: {}", e));
            ExitStatus::ExitedWith(1)
        }
    }
}

fn pipe_to_editor(ctx: &Context, proxy: &mut dyn ShellProxy) -> Result<()> {
    // 1. Read from stdin (ctx.infile)
    // CRITICAL: unsafe usage of FromRawFd requires correct ownership handling.
    // We must NOT drop the File, as it would close the fd which belongs to Context.
    let mut content = Vec::new();
    let mut file = unsafe { File::from_raw_fd(ctx.infile) };

    // Read content
    let result = file.read_to_end(&mut content);

    // CRITICAL: Forget the file to prevent closing fd
    mem::forget(file);

    result.context("failed to read from stdin")?;

    let content_str = String::from_utf8_lossy(&content);

    // 2. Open editor
    let edited = proxy.open_editor(&content_str, "txt")?;

    // 3. Write result to stdout (ctx.outfile)
    // Also ensuring we output a newline if one is missing, acting like cat/echo
    let mut outfile = unsafe { File::from_raw_fd(ctx.outfile) };

    let write_res = (|| -> std::io::Result<()> {
        outfile.write_all(edited.as_bytes())?;
        if !edited.ends_with('\n') {
            outfile.write_all(b"\n")?;
        }
        Ok(())
    })();

    // CRITICAL: Forget the file
    mem::forget(outfile);

    write_res.context("failed to write to stdout")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;
    use dsh_types::Context;
    use nix::unistd::pipe;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    #[test]
    fn test_eview_pipe() -> Result<()> {
        // Setup input pipe
        let (read_in, write_in) = pipe()?;

        // Setup output pipe
        let (read_out, write_out) = pipe()?;

        // Write to input pipe
        let mut input_writer = File::from(write_in);
        input_writer.write_all(b"original content")?;
        drop(input_writer); // Close write end so read ends

        // Setup Context
        let mut ctx = Context::new(nix::unistd::getpid(), nix::unistd::getpid(), None, true);
        ctx.infile = read_in.as_raw_fd();
        ctx.outfile = write_out.as_raw_fd();

        // Setup proxy
        let mut proxy = TestShellProxy {
            open_editor_response: Some("edited content".to_string()),
            ..TestShellProxy::default()
        };

        // Run command
        let status = command(&ctx, vec![], &mut proxy);
        assert_eq!(status, ExitStatus::ExitedWith(0));

        // Verify proxy calls
        assert_eq!(
            proxy.open_editor_calls,
            vec![("original content".to_string(), "txt".to_string())]
        );

        // Verify output
        drop(write_out);

        let mut output_reader = File::from(read_out);
        let mut output_content = String::new();
        output_reader.read_to_string(&mut output_content)?;

        assert_eq!(output_content, "edited content\n");

        Ok(())
    }
}
