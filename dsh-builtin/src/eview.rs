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
    use dsh_types::{Context, mcp::McpServerConfig};
    use nix::unistd::pipe;
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::unix::io::FromRawFd;

    struct MockShellProxy {
        pub captured_content: String,
        pub file_extension: String,
        pub return_content: String,
    }

    impl MockShellProxy {
        fn new() -> Self {
            Self {
                captured_content: String::new(),
                file_extension: String::new(),
                return_content: "edited content".to_string(),
            }
        }
    }

    #[allow(unused)]
    impl ShellProxy for MockShellProxy {
        fn exit_shell(&mut self) {}
        fn dispatch(&mut self, _ctx: &Context, _cmd: &str, _argv: Vec<String>) -> Result<()> {
            Ok(())
        }
        fn save_path_history(&mut self, _path: &str) {}
        fn changepwd(&mut self, _path: &str) -> Result<()> {
            Ok(())
        }
        fn insert_path(&mut self, _index: usize, _path: &str) {}
        fn get_var(&mut self, _key: &str) -> Option<String> {
            None
        }
        fn set_var(&mut self, _key: String, _value: String) {}
        fn set_env_var(&mut self, _key: String, _value: String) {}
        fn get_alias(&mut self, _name: &str) -> Option<String> {
            None
        }
        fn set_alias(&mut self, _name: String, _command: String) {}
        fn list_aliases(&mut self) -> HashMap<String, String> {
            HashMap::new()
        }
        fn add_abbr(&mut self, _name: String, _expansion: String) {}
        fn remove_abbr(&mut self, _name: &str) -> bool {
            false
        }
        fn list_abbrs(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn get_abbr(&self, _name: &str) -> Option<String> {
            None
        }
        fn list_mcp_servers(&mut self) -> Vec<McpServerConfig> {
            Vec::new()
        }
        fn list_execute_allowlist(&mut self) -> Vec<String> {
            Vec::new()
        }
        fn list_exported_vars(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn export_var(&mut self, _key: &str) -> bool {
            false
        }
        fn set_and_export_var(&mut self, _key: String, _value: String) {}
        fn get_current_dir(&self) -> Result<std::path::PathBuf> {
            Ok(std::path::PathBuf::from("/"))
        }
        fn get_lisp_var(&self, _key: &str) -> Option<String> {
            None
        }
        fn capture_command(&mut self, _ctx: &Context, _cmd: &str) -> Result<(i32, String, String)> {
            Ok((0, String::new(), String::new()))
        }

        fn open_editor(&mut self, content: &str, extension: &str) -> Result<String> {
            self.captured_content = content.to_string();
            self.file_extension = extension.to_string();
            Ok(self.return_content.clone())
        }
    }

    #[test]
    fn test_eview_pipe() -> Result<()> {
        // Setup input pipe
        let (read_in, write_in) = pipe()?;

        // Setup output pipe
        let (read_out, write_out) = pipe()?;

        // Write to input pipe
        let mut input_writer = unsafe { File::from_raw_fd(write_in) };
        input_writer.write_all(b"original content")?;
        drop(input_writer); // Close write end so read ends

        // Setup Context
        let mut ctx = Context::new(nix::unistd::getpid(), nix::unistd::getpid(), None, true);
        ctx.infile = read_in;
        ctx.outfile = write_out;

        // Setup MockProxy
        let mut proxy = MockShellProxy::new();

        // Run command
        let status = command(&ctx, vec![], &mut proxy);
        assert_eq!(status, ExitStatus::ExitedWith(0));

        // Verify proxy calls
        assert_eq!(proxy.captured_content, "original content");
        assert_eq!(proxy.file_extension, "txt");

        // Verify output
        nix::unistd::close(write_out)?;

        let mut output_reader = unsafe { File::from_raw_fd(read_out) };
        let mut output_content = String::new();
        output_reader.read_to_string(&mut output_content)?;

        assert_eq!(output_content, "edited content\n");

        Ok(())
    }
}
