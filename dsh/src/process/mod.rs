#![allow(clippy::module_inception)]

pub mod async_io;
pub mod builtin;
pub mod child_exec;
pub mod fork;
pub mod io;
pub mod job;
pub mod job_process;
#[cfg(test)]
mod job_process_tests;
pub mod job_pty;
pub mod job_wait;
pub mod process;
pub mod pty;
pub mod redirect;
pub mod reexec;
pub mod signal;
pub mod state;
pub mod wait;

pub use builtin::BuiltinProcess;
pub use job::Job;
pub use job_process::JobProcess;
pub use process::Process;
pub use pty::Pty;
pub use redirect::Redirect;
pub use state::{ListOp, ProcessState, SubshellType, signal_exit_status};
pub use wait::{WaitPidObservation, wait_pid_job};
