//! POSIX arithmetic expansion AST and evaluation.
//!
//! Planning stays side-effect-free: pest pairs become [`PlannedWord`] bodies,
//! and the expanded expression string becomes this AST only at
//! materialization time. Evaluation reads the logical [`Environment`],
//! writes assignments through `set_shell_var`, and never spawns processes.
//! All integer math is `i64` with checked operations so Linux/macOS and
//! debug/release behave identically.

pub(crate) mod eval;
pub(crate) mod parser;
pub(crate) mod token;

pub(crate) use eval::{evaluate_expression, is_arithmetic_expansion_error};
