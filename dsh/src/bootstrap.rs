//! Process-lifetime setup: tracing, the panic handler, background loader
//! thread bookkeeping for startup, and the Herdr shutdown-signal watcher.
use crate::agent_lifecycle;
use anyhow::Result;
use std::sync::Arc;
use tracing::debug;

/// Owns work that is useful only while an interactive shell is alive.
///
/// Short-lived filesystem loader threads are joined when they have already
/// completed and are otherwise detached so shutdown never waits on filesystem
/// I/O. In particular, these tasks must not run on Tokio's blocking pool,
/// because runtime shutdown waits for started `spawn_blocking` work.
#[derive(Default)]
pub(crate) struct StartupBackgroundTasks {
    loader_threads: Vec<std::thread::JoinHandle<()>>,
}

impl StartupBackgroundTasks {
    pub(crate) fn push_loader(&mut self, thread: std::thread::JoinHandle<()>) {
        self.loader_threads.push(thread);
    }
}

impl Drop for StartupBackgroundTasks {
    fn drop(&mut self) {
        for thread in self.loader_threads.drain(..) {
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

pub fn init_tracing() -> Result<()> {
    let log_path = crate::environment::get_state_file("debug.log")
        .unwrap_or_else(|_| std::path::PathBuf::from("./debug.log"));

    let log_file = std::sync::Arc::new(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?,
    );

    let env_filter = tracing_subscriber::EnvFilter::try_from_env("DOGESH_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(env_filter)
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .init();
    Ok(())
}

pub fn setup_panic_handler() {
    std::panic::set_hook(Box::new(|panic_info| {
        // Attempt to restore terminal state first
        let _ = crossterm::terminal::disable_raw_mode();

        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("unnamed");

        let payload = panic_info.payload().downcast_ref::<&str>().map_or_else(
            || {
                if let Some(s) = panic_info.payload().downcast_ref::<String>() {
                    s.clone()
                } else {
                    "Unknown panic payload".to_string()
                }
            },
            |s| (*s).to_string(),
        );

        // Don't show stacktrace for panics related to normal exit
        if payload.contains("Shell terminated by double Ctrl+C")
            || payload.contains("Normal exit")
            || payload.contains("Exit by")
            || payload.contains("exit command")
        {
            // Show only brief message for normal exit
            debug!("Shell exiting normally: {}", payload);
            return;
        }

        let location = panic_info.location().map_or_else(
            || "Unknown location".to_string(),
            |location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            },
        );

        // Get backtrace (if RUST_BACKTRACE=1 is set)
        let backtrace = std::backtrace::Backtrace::capture();
        let backtrace_str = match backtrace.status() {
            std::backtrace::BacktraceStatus::Captured => format!("\nBacktrace:\n{backtrace}"),
            std::backtrace::BacktraceStatus::Disabled => {
                "\nBacktrace: disabled (set RUST_BACKTRACE=1 to enable)".to_string()
            }
            std::backtrace::BacktraceStatus::Unsupported => "\nBacktrace: unsupported".to_string(),
            _ => "\nBacktrace: unknown status".to_string(),
        };

        // Write directly to log file (tracing may not be initialized)
        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f UTC");
        let panic_log = format!(
            "\n=== PANIC OCCURRED ===\n\
            Timestamp: {timestamp}\n\
            Thread: {thread_name}\n\
            Location: {location}\n\
            Message: {payload}{backtrace_str}\n\
            ======================\n"
        );

        // Record logs in multiple ways
        // 1. Write directly to log file
        let log_files = ["debug.log", "panic.log"];
        for log_name in &log_files {
            let log_path = crate::environment::get_state_file(log_name)
                .unwrap_or_else(|_| std::path::PathBuf::from(log_name));

            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path)
            {
                use std::io::Write;
                let _ = writeln!(file, "{panic_log}");
                let _ = file.flush();
            }
        }

        // 2. Also try to output to tracing log (if initialized)
        tracing::error!("PANIC OCCURRED: {payload} at {location}");

        // 3. Also output to stderr (maintain default behavior)
        eprintln!("\n=== doge-shell PANIC ===");
        eprintln!("Message: {payload}");
        eprintln!("Location: {location}");
        eprintln!("Thread: {thread_name}");
        eprintln!("Timestamp: {timestamp}");
        eprintln!("See log files in state directory for detailed information");
        eprintln!("========================\n");
    }));
}

/// SIGTERM/SIGHUP have no handler in this shell today (`Shell::set_signals`
/// only touches SIGINT/SIGQUIT/SIGTSTP/SIGTTIN/SIGTTOU); default disposition
/// terminates the process immediately, running no destructors at all. That's
/// very likely the *most common* way a real Herdr user ends a session
/// (closing the pane), so without this, every such exit would leave stale
/// "working"/"blocked" state in Herdr until the pane itself is torn down.
///
/// Spawned only when Herdr reporting is actually active - a zero-cost no-op
/// for every non-Herdr user. Deliberately doesn't touch `Shell` (which is
/// `!Send`): it holds only the `Send + Sync` lifecycle manager, does its own
/// best-effort release, and exits the whole process directly. This is
/// already an improvement over the default disposition (which never called
/// `release-agent` at all), so it also takes the same opportunity to restore
/// the terminal (mirroring `setup_panic_handler`'s own best-effort
/// `disable_raw_mode` before an abnormal exit) and to exit with this
/// codebase's own `128 + signal` convention for a signal-terminated process
/// (`dsh/src/repl/job_notify.rs::JobNoticeState::exit_code`), rather than a
/// bare `0` that would misreport a forced shutdown as a clean exit to
/// whatever waits on this process.
pub(crate) fn spawn_herdr_shutdown_signal_watcher(
    lifecycle: Arc<agent_lifecycle::AgentLifecycleManager>,
) {
    use tokio::signal::unix::{SignalKind, signal};

    tokio::spawn(async move {
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                debug!("herdr lifecycle: failed to install SIGTERM watcher: {e}");
                return;
            }
        };
        let mut hup = match signal(SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                debug!("herdr lifecycle: failed to install SIGHUP watcher: {e}");
                return;
            }
        };
        let received = tokio::select! {
            _ = term.recv() => nix::sys::signal::Signal::SIGTERM,
            _ = hup.recv() => nix::sys::signal::Signal::SIGHUP,
        };
        lifecycle.shutdown();
        let _ = crossterm::terminal::disable_raw_mode();
        std::process::exit(128 + received as i32);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn startup_background_tasks_do_not_wait_for_running_loader_threads() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let mut tasks = StartupBackgroundTasks::default();
        tasks.push_loader(std::thread::spawn(move || {
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
        }));

        let started = std::time::Instant::now();
        drop(tasks);
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "dropping startup tasks waited for a running filesystem loader"
        );
        let _ = release_tx.send(());
    }

    #[test]
    #[ignore] // Ignore in normal test runs (for manual execution)
    fn test_panic_handler() {
        // Use test log files
        let test_log_files = ["debug.log", "panic.log"]
            .iter()
            .map(|name| crate::environment::get_state_file(name).unwrap())
            .collect::<Vec<_>>();

        // Remove existing log files
        for log_file in &test_log_files {
            let _ = fs::remove_file(log_file);
        }

        // Set up panic handler
        setup_panic_handler();

        // Trigger panic in separate thread
        let handle = thread::spawn(|| {
            panic!("Test panic for logging verification");
        });

        // Wait for panic
        let _ = handle.join();

        // Wait a bit then check log files
        thread::sleep(Duration::from_millis(200));

        // Check if log files are created and panic info is recorded
        let mut found_panic_log = false;
        for log_file in &test_log_files {
            if let Ok(content) = fs::read_to_string(log_file)
                && content.contains("PANIC OCCURRED")
                && content.contains("Test panic for logging verification")
            {
                found_panic_log = true;
                println!("Panic information found in {:?}", log_file);
                break;
            }
        }

        assert!(
            found_panic_log,
            "Panic information not found in any log file"
        );
        println!("Panic handler test passed - check log files for details");
    }
}
