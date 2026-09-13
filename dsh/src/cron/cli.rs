//! Argument parsing and table rendering for the `cron` builtin.
//!
//! Kept as pure functions over `&[String]` — no `&mut Shell` anywhere in this
//! module — so a change to an option's grammar can be tested directly, the
//! same reason `sched.rs` split `parse_add` out of its `add()` handler. The
//! parsing itself has to live in the `dsh` crate rather than `dsh-builtin`
//! (unlike `sched`'s): the store it validates against (`SqliteCronStore`)
//! depends on `rusqlite`, which only `dsh` carries, so nothing here can be
//! moved down without the store following it.

pub mod parse;
pub mod render;

pub use parse::*;
pub use render::*;
