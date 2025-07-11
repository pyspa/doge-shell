use nix::sys::termios::{Termios, tcgetattr};
use nix::unistd::isatty;
use std::os::unix::io::RawFd;
use tracing::{debug, warn};

/// ターミナル状態を表現する構造体
#[derive(Debug, Clone)]
pub struct TerminalState {
    /// 指定されたファイルディスクリプタがターミナルかどうか
    pub is_terminal: bool,
    /// ターミナルの設定（ターミナルの場合のみ）
    pub tmodes: Option<Termios>,
    /// ジョブ制御がサポートされているかどうか
    pub supports_job_control: bool,
}

impl TerminalState {
    /// 指定されたファイルディスクリプタのターミナル状態を検出
    pub fn detect(fd: RawFd) -> Self {
        let is_terminal = isatty(fd).unwrap_or(false);
        debug!("Terminal detection for fd {}: {}", fd, is_terminal);

        let tmodes = if is_terminal {
            match tcgetattr(fd) {
                Ok(tmodes) => {
                    debug!("Successfully retrieved terminal modes for fd {}", fd);
                    Some(tmodes)
                }
                Err(err) => {
                    warn!("Failed to get terminal attributes for fd {}: {}", fd, err);
                    None
                }
            }
        } else {
            debug!("Fd {} is not a terminal, skipping tcgetattr", fd);
            None
        };

        let supports_job_control = is_terminal && tmodes.is_some();
        debug!(
            "Job control support for fd {}: {}",
            fd, supports_job_control
        );

        Self {
            is_terminal,
            tmodes,
            supports_job_control,
        }
    }

    /// デフォルトのターミナル状態（非ターミナル環境用）
    pub fn non_terminal() -> Self {
        Self {
            is_terminal: false,
            tmodes: None,
            supports_job_control: false,
        }
    }

    /// ターミナル設定を取得（存在する場合）
    pub fn get_tmodes(&self) -> Option<&Termios> {
        self.tmodes.as_ref()
    }

    /// ジョブ制御が利用可能かチェック
    pub fn can_control_jobs(&self) -> bool {
        self.supports_job_control
    }
}

/// Enumeration representing shell execution modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellMode {
    /// Interactive mode (both stdin and stdout are terminals)
    Interactive,
    /// Pipeline mode (stdin is terminal, stdout is non-terminal)
    Pipeline,
    /// Script mode (both stdin and stdout are non-terminals)
    Script,
    /// Background mode
    Background,
}

impl ShellMode {
    /// 現在の環境からシェルモードを検出
    pub fn detect() -> Self {
        let stdin_is_tty = isatty(libc::STDIN_FILENO).unwrap_or(false);
        let stdout_is_tty = isatty(libc::STDOUT_FILENO).unwrap_or(false);

        match (stdin_is_tty, stdout_is_tty) {
            (true, true) => ShellMode::Interactive,
            (true, false) => ShellMode::Pipeline,
            (false, _) => ShellMode::Script,
        }
    }

    /// このモードでジョブ制御がサポートされているか
    pub fn supports_job_control(&self) -> bool {
        matches!(self, ShellMode::Interactive)
    }

    /// このモードで対話的な操作が可能か
    pub fn is_interactive(&self) -> bool {
        matches!(self, ShellMode::Interactive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_state_non_terminal() {
        let state = TerminalState::non_terminal();
        assert!(!state.is_terminal);
        assert!(state.tmodes.is_none());
        assert!(!state.supports_job_control);
        assert!(!state.can_control_jobs());
    }

    #[test]
    fn test_shell_mode_detection() {
        // この テストは実際の環境に依存するため、
        // 基本的な動作のみをテスト
        let mode = ShellMode::detect();

        // モードが有効な値であることを確認
        match mode {
            ShellMode::Interactive
            | ShellMode::Pipeline
            | ShellMode::Script
            | ShellMode::Background => {
                // OK
            }
        }
    }

    #[test]
    fn test_shell_mode_job_control_support() {
        assert!(ShellMode::Interactive.supports_job_control());
        assert!(!ShellMode::Pipeline.supports_job_control());
        assert!(!ShellMode::Script.supports_job_control());
        assert!(!ShellMode::Background.supports_job_control());
    }

    #[test]
    fn test_shell_mode_interactivity() {
        assert!(ShellMode::Interactive.is_interactive());
        assert!(!ShellMode::Pipeline.is_interactive());
        assert!(!ShellMode::Script.is_interactive());
        assert!(!ShellMode::Background.is_interactive());
    }
}
