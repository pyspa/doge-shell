//! Builtin command handlers for the shell dispatch system.
//!
//! This module contains handlers for shell builtin commands that are executed
//! directly by the dispatch function rather than as external processes.

pub mod exit;
pub mod history;
pub mod jobs;
pub mod lisp;
pub mod reload;
pub mod var;
pub mod z;
