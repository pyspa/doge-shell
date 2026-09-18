//! Word-pattern helpers shared with runtime expansion.
//!
//! The parser itself performs no runtime expansion. These pure helpers
//! (brace expansion, glob walks, pattern escaping) are reused by
//! `shell::word_expand` at materialization time.

mod alias;
mod glob;
pub use alias::rewrite_aliases;
pub(crate) use glob::{
    escape_glob_metacharacters, expand_braces, expand_glob_pattern, unescape_glob_metacharacters,
};
