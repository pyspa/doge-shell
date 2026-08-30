//! Warp-style browser for the command blocks the shell already records.
//!
//! Every command's output is captured into a [`CommandBlock`] (the observer in
//! `repl::key_handlers::execution` is always on), but until now that data was
//! only reachable through `blocks list`/`blocks show` and was discarded when the
//! shell exited. This browses it: scroll past runs with their output, fold long
//! output away, copy it, re-run it, or jump to the directory it ran in.
//!
//! [`CommandBlock`]: dsh_types::command_block::CommandBlock

pub mod model;
mod render;

pub use model::{BlockBrowser, BrowserOutcome, OutputStream};
pub use render::run;
