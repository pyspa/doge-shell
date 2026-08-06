pub mod renderer;
pub mod title;

use std::sync::OnceLock;

/// Whether this process may mutate the terminal it was started from.
///
/// dsh drives the terminal directly: raw mode, DECSTBM margins, `tcsetpgrp`,
/// and a PTY input proxy that reads the real tty. Under `cargo test` the test
/// binary inherits the *developer's* terminal on fd 0, so every one of those
/// paths would reach out and change it — and unlike the shell, a test binary
/// never restores anything on the way out.
///
/// `ctx.interactive` is not a sufficient guard: `Context::new_safe` derives it
/// from `isatty(STDIN_FILENO)`, which is true when tests run from a terminal.
/// This is the explicit gate instead.
///
/// Always false in unit-test builds. `DSH_NO_TERMINAL_CONTROL` disables it for
/// manual debugging and for harnesses that drive dsh as a subprocess.
pub(crate) fn terminal_control_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| !cfg!(test) && std::env::var_os("DSH_NO_TERMINAL_CONTROL").is_none())
}
