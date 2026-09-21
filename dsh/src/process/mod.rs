#![allow(clippy::module_inception)]

pub mod async_io;
pub mod async_list;
pub mod builtin;
pub mod child_exec;
pub mod fork;
pub mod io;
pub mod job;
pub mod job_process;
mod job_process_launch;
#[cfg(test)]
mod job_process_tests;
pub mod job_pty;
pub mod job_wait;
pub mod launch_outcome;
pub mod no_command_process;
pub mod pipeline_source;
pub mod process;
pub mod pty;
pub mod redirect;
pub mod reexec;
pub mod signal;
pub mod state;
pub mod wait;

pub use async_list::AsyncListProcess;
pub use builtin::{BuiltinExecutionPlacement, BuiltinProcess, builtin_execution_placement};
pub use job::Job;
pub use job_process::JobProcess;
pub use launch_outcome::{CommandFailure, JobLaunchOutcome};
pub use no_command_process::NoCommandProcess;
pub use pipeline_source::PipelineSourceProcess;
pub use process::Process;
pub use pty::Pty;
pub use redirect::Redirect;
pub use state::{ListOp, ProcessState, SubshellType, signal_exit_status};
pub use wait::{WaitPidObservation, wait_pid_job};
