use pest_derive::Parser;

#[derive(Parser, Debug, Clone)]
#[grammar = "shell.pest"]
pub struct ShellParser;

pub mod ast;
pub mod expansion;
pub mod highlight;

#[cfg(test)]
mod tests;

// Re-exports
pub use ast::{get_pos_word, get_string, get_words, get_words_from_pairs};
pub use expansion::{expand_alias, parse_with_expansion};
pub use highlight::{
    HighlightKind, HighlightResult, HighlightToken, collect_highlight_tokens_from_pairs,
    highlight_error_token,
};
