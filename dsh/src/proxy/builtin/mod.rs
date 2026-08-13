//! Builtin command handlers for the shell dispatch system.
//!
//! This module contains handlers for shell builtin commands that are executed
//! directly by the dispatch function rather than as external processes.

pub mod abbr;
pub mod blocks_persistent;
pub mod blocks_tui;
pub mod exit;
pub mod history;
pub mod jobs;
pub mod lisp;
pub mod reload;
pub mod var;
pub mod z;
