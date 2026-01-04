use doge_shell::environment::Environment;
use doge_shell::shell::Shell;
use doge_shell::shell::eval::eval_str;
use dsh_types::Context;

fn create_test_shell() -> Shell {
    let env = Environment::new();
    Shell::new(env)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_binary_output_capture() {
    let mut shell = create_test_shell();
    let pid = nix::unistd::getpid();

    let mut ctx = Context::new(pid, pid, None, false);

    // Command that produces invalid UTF-8 (binary data)
    // We use printf to generate a specific byte sequence that is invalid UTF-8
    // \xff is invalid in UTF-8
    let cmd = "printf '\\xff\\xff\\xff'";

    // We want to capture this into a variable or just run it via eval_str
    // The critical part is that it shouldn't panic when the shell tries to read the output
    // for command substitution or similar.

    // Test 1: Variable expansion with binary data
    let input = format!("x=$({})", cmd);
    let result: anyhow::Result<i32> = eval_str(&mut shell, &mut ctx, input.clone(), false).await;

    assert!(
        result.is_ok(),
        "Shell panicked or errored on binary input: {:?}",
        result.err()
    );

    // Check if the variable contains the replacement character (replaces invalid bytes)
    // The exact behavior depends on String::from_utf8_lossy, typically U+FFFD
    let env_guard = shell.environment.read();
    let var_content = env_guard.variables.get("$x");

    assert!(var_content.is_some(), "Variable $x was not set");
    let content = var_content.unwrap();
    assert!(
        content.contains('\u{FFFD}'),
        "Variable content should contain replacement char: {}",
        content
    );
}
