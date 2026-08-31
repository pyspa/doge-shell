use crate::completion::command::CompletionCandidate;
use anyhow::Result;

/// Signals as *this* platform numbers them.
///
/// Only 1-15 are fixed across Unixes; past that Linux and macOS diverge
/// completely -- `SIGUSR1` is 10 on Linux and 30 on macOS, `SIGCHLD` 17 versus
/// 20 -- and each has signals the other lacks (`SIGSTKFLT`/`SIGPWR` against
/// `SIGEMT`/`SIGINFO`). `kill -<number>` completion is only useful if it offers
/// the numbers the running kernel actually accepts, so the table is per target.
/// `the_table_uses_this_platforms_signal_numbers` holds each entry to `libc`.
#[cfg(not(target_os = "macos"))]
const SIGNALS: &[(&str, i32, &str)] = &[
    ("SIGHUP", 1, "Hangup"),
    ("SIGINT", 2, "Interrupt"),
    ("SIGQUIT", 3, "Quit"),
    ("SIGILL", 4, "Illegal instruction"),
    ("SIGTRAP", 5, "Trace trap"),
    ("SIGABRT", 6, "Abort"),
    ("SIGBUS", 7, "Bus error"),
    ("SIGFPE", 8, "Floating point exception"),
    ("SIGKILL", 9, "Kill (cannot be caught)"),
    ("SIGUSR1", 10, "User defined signal 1"),
    ("SIGSEGV", 11, "Segmentation violation"),
    ("SIGUSR2", 12, "User defined signal 2"),
    ("SIGPIPE", 13, "Broken pipe"),
    ("SIGALRM", 14, "Alarm clock"),
    ("SIGTERM", 15, "Termination"),
    ("SIGSTKFLT", 16, "Stack fault"),
    ("SIGCHLD", 17, "Child status changed"),
    ("SIGCONT", 18, "Continue"),
    ("SIGSTOP", 19, "Stop (cannot be caught)"),
    ("SIGTSTP", 20, "Terminal stop"),
    ("SIGTTIN", 21, "Background read from tty"),
    ("SIGTTOU", 22, "Background write to tty"),
    ("SIGURG", 23, "Urgent I/O condition"),
    ("SIGXCPU", 24, "CPU time limit exceeded"),
    ("SIGXFSZ", 25, "File size limit exceeded"),
    ("SIGVTALRM", 26, "Virtual timer expired"),
    ("SIGPROF", 27, "Profiling timer expired"),
    ("SIGWINCH", 28, "Window size changed"),
    ("SIGIO", 29, "I/O possible"),
    ("SIGPWR", 30, "Power failure"),
    ("SIGSYS", 31, "Bad system call"),
];

/// See the Linux table above. Mirrors `kill -l` on macOS.
#[cfg(target_os = "macos")]
const SIGNALS: &[(&str, i32, &str)] = &[
    ("SIGHUP", 1, "Hangup"),
    ("SIGINT", 2, "Interrupt"),
    ("SIGQUIT", 3, "Quit"),
    ("SIGILL", 4, "Illegal instruction"),
    ("SIGTRAP", 5, "Trace trap"),
    ("SIGABRT", 6, "Abort"),
    ("SIGEMT", 7, "Emulator trap"),
    ("SIGFPE", 8, "Floating point exception"),
    ("SIGKILL", 9, "Kill (cannot be caught)"),
    ("SIGBUS", 10, "Bus error"),
    ("SIGSEGV", 11, "Segmentation violation"),
    ("SIGSYS", 12, "Bad system call"),
    ("SIGPIPE", 13, "Broken pipe"),
    ("SIGALRM", 14, "Alarm clock"),
    ("SIGTERM", 15, "Termination"),
    ("SIGURG", 16, "Urgent I/O condition"),
    ("SIGSTOP", 17, "Stop (cannot be caught)"),
    ("SIGTSTP", 18, "Terminal stop"),
    ("SIGCONT", 19, "Continue"),
    ("SIGCHLD", 20, "Child status changed"),
    ("SIGTTIN", 21, "Background read from tty"),
    ("SIGTTOU", 22, "Background write to tty"),
    ("SIGIO", 23, "I/O possible"),
    ("SIGXCPU", 24, "CPU time limit exceeded"),
    ("SIGXFSZ", 25, "File size limit exceeded"),
    ("SIGVTALRM", 26, "Virtual timer expired"),
    ("SIGPROF", 27, "Profiling timer expired"),
    ("SIGWINCH", 28, "Window size changed"),
    ("SIGINFO", 29, "Status request"),
    ("SIGUSR1", 30, "User defined signal 1"),
    ("SIGUSR2", 31, "User defined signal 2"),
];

/// Generator for signal name completion
pub struct SignalGenerator;

impl SignalGenerator {
    pub fn new() -> Self {
        Self
    }

    pub fn generate_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        let token_upper = current_token.to_uppercase();

        let candidates: Vec<CompletionCandidate> = SIGNALS
            .iter()
            .filter_map(|(name, num, desc)| {
                // Match by signal name (with or without SIG prefix)
                let name_without_sig = name.strip_prefix("SIG").unwrap_or(name);

                if current_token.is_empty()
                    || name.starts_with(&token_upper)
                    || name_without_sig.starts_with(&token_upper)
                    || num.to_string().starts_with(current_token)
                {
                    Some(CompletionCandidate::argument(
                        name.to_string(),
                        Some(format!("{} ({})", desc, num)),
                    ))
                } else {
                    None
                }
            })
            .collect();

        Ok(candidates)
    }

    /// Generate candidates with just the signal name (without SIG prefix)
    pub fn generate_short_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        let token_upper = current_token.to_uppercase();

        let candidates: Vec<CompletionCandidate> = SIGNALS
            .iter()
            .filter_map(|(name, num, desc)| {
                let short_name = name.strip_prefix("SIG").unwrap_or(name);

                if current_token.is_empty()
                    || short_name.starts_with(&token_upper)
                    || num.to_string().starts_with(current_token)
                {
                    Some(CompletionCandidate::argument(
                        short_name.to_string(),
                        Some(format!("{} ({})", desc, num)),
                    ))
                } else {
                    None
                }
            })
            .collect();

        Ok(candidates)
    }
}

impl Default for SignalGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number `libc` gives this platform for `name`, or `None` when the
    /// platform has no such signal.
    fn libc_number(name: &str) -> Option<i32> {
        Some(match name {
            "SIGHUP" => libc::SIGHUP,
            "SIGINT" => libc::SIGINT,
            "SIGQUIT" => libc::SIGQUIT,
            "SIGILL" => libc::SIGILL,
            "SIGTRAP" => libc::SIGTRAP,
            "SIGABRT" => libc::SIGABRT,
            "SIGBUS" => libc::SIGBUS,
            "SIGFPE" => libc::SIGFPE,
            "SIGKILL" => libc::SIGKILL,
            "SIGUSR1" => libc::SIGUSR1,
            "SIGSEGV" => libc::SIGSEGV,
            "SIGUSR2" => libc::SIGUSR2,
            "SIGPIPE" => libc::SIGPIPE,
            "SIGALRM" => libc::SIGALRM,
            "SIGTERM" => libc::SIGTERM,
            "SIGCHLD" => libc::SIGCHLD,
            "SIGCONT" => libc::SIGCONT,
            "SIGSTOP" => libc::SIGSTOP,
            "SIGTSTP" => libc::SIGTSTP,
            "SIGTTIN" => libc::SIGTTIN,
            "SIGTTOU" => libc::SIGTTOU,
            "SIGURG" => libc::SIGURG,
            "SIGXCPU" => libc::SIGXCPU,
            "SIGXFSZ" => libc::SIGXFSZ,
            "SIGVTALRM" => libc::SIGVTALRM,
            "SIGPROF" => libc::SIGPROF,
            "SIGWINCH" => libc::SIGWINCH,
            "SIGIO" => libc::SIGIO,
            "SIGSYS" => libc::SIGSYS,
            #[cfg(target_os = "macos")]
            "SIGEMT" => libc::SIGEMT,
            #[cfg(target_os = "macos")]
            "SIGINFO" => libc::SIGINFO,
            #[cfg(not(target_os = "macos"))]
            "SIGSTKFLT" => libc::SIGSTKFLT,
            #[cfg(not(target_os = "macos"))]
            "SIGPWR" => libc::SIGPWR,
            _ => return None,
        })
    }

    /// The whole point of splitting the table per target: offering `kill -17`
    /// as `SIGCHLD` on macOS (where it stops the process) would be worse than
    /// offering nothing.
    #[test]
    fn the_table_uses_this_platforms_signal_numbers() {
        for (name, number, _) in SIGNALS {
            let expected = libc_number(name)
                .unwrap_or_else(|| panic!("{name} is in the table but unknown to libc here"));
            assert_eq!(
                *number, expected,
                "{name} is {number} in the table but {expected} on this platform"
            );
        }
    }

    /// Numbers have to be unique, or filtering by one offers two signals.
    #[test]
    fn the_table_has_no_duplicate_numbers() {
        let mut numbers: Vec<i32> = SIGNALS.iter().map(|(_, number, _)| *number).collect();
        numbers.sort_unstable();
        let count = numbers.len();
        numbers.dedup();
        assert_eq!(
            numbers.len(),
            count,
            "duplicate signal numbers in the table"
        );
    }

    #[test]
    fn test_signal_generator_creates() {
        let generator = SignalGenerator::new();
        let _ = generator;
    }

    #[test]
    fn test_signal_generator_all_signals() {
        let generator = SignalGenerator::new();
        let result = generator.generate_candidates("").unwrap();
        assert_eq!(result.len(), 31, "Expected 31 signals");
        assert!(result.iter().any(|c| c.text == "SIGTERM"));
        assert!(result.iter().any(|c| c.text == "SIGKILL"));
        assert!(result.iter().any(|c| c.text == "SIGHUP"));
    }

    #[test]
    fn test_signal_generator_filter_by_name() {
        let generator = SignalGenerator::new();
        let result = generator.generate_candidates("SIGK").unwrap();
        assert!(result.iter().any(|c| c.text == "SIGKILL"));
        assert!(!result.iter().any(|c| c.text == "SIGTERM"));
    }

    #[test]
    fn test_signal_generator_filter_by_name_without_sig_prefix() {
        let generator = SignalGenerator::new();
        // Should match SIGKILL when typing "KILL" (without SIG prefix)
        let result = generator.generate_candidates("KILL").unwrap();
        assert!(
            result.iter().any(|c| c.text == "SIGKILL"),
            "Expected SIGKILL when filtering by 'KILL'"
        );
    }

    #[test]
    fn test_signal_generator_filter_by_number() {
        let generator = SignalGenerator::new();
        let result = generator.generate_candidates("9").unwrap();
        assert!(result.iter().any(|c| c.text == "SIGKILL"));
        // Should not match SIGTERM (15), SIGUSR1 (10), etc.
        assert!(!result.iter().any(|c| c.text == "SIGTERM"));
    }

    #[test]
    fn test_signal_generator_filter_by_number_two_digits() {
        let generator = SignalGenerator::new();
        let result = generator.generate_candidates("15").unwrap();
        assert!(
            result.iter().any(|c| c.text == "SIGTERM"),
            "Expected SIGTERM for signal 15"
        );
        assert_eq!(result.len(), 1, "Expected only one match for '15'");
    }

    #[test]
    fn test_signal_generator_short_names() {
        let generator = SignalGenerator::new();
        let result = generator.generate_short_candidates("KILL").unwrap();
        assert!(result.iter().any(|c| c.text == "KILL"));
        assert!(!result.iter().any(|c| c.text == "SIGKILL"));
    }

    #[test]
    fn test_signal_generator_has_description() {
        let generator = SignalGenerator::new();
        let result = generator.generate_candidates("SIGTERM").unwrap();
        assert!(!result.is_empty());
        let sigterm = result.iter().find(|c| c.text == "SIGTERM").unwrap();
        assert!(sigterm.description.is_some());
        let desc = sigterm.description.as_ref().unwrap();
        assert!(
            desc.contains("15"),
            "Expected description to contain signal number"
        );
        assert!(
            desc.contains("Termination"),
            "Expected description to contain signal meaning"
        );
    }

    #[test]
    fn test_signal_generator_case_insensitive() {
        let generator = SignalGenerator::new();
        let lower = generator.generate_candidates("sigk").unwrap();
        let upper = generator.generate_candidates("SIGK").unwrap();
        assert_eq!(lower.len(), upper.len());
    }
}
