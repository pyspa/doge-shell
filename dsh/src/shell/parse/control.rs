//! Reject unsupported control syntax before aliases or runtime expansion.
//!
//! The editor's tolerant grammar is unchanged. Inspect raw command-word spans
//! rather than decoded argv: quotes, escapes and runtime values are command
//! data, and arguments/assignment names are not reserved-word positions.

use crate::parser::Rule;
use anyhow::Result;
use pest::iterators::{Pair, Pairs};

pub(super) fn validate(pairs: Pairs<'_, Rule>) -> Result<()> {
    for pair in pairs {
        validate_pair(pair)?;
    }
    Ok(())
}

fn validate_pair(pair: Pair<'_, Rule>) -> Result<()> {
    if pair.as_rule() == Rule::argv0
        && let Some(keyword) =
            dsh_types::safety_policy::compound_statement_keyword(pair.as_str())
        // Grouping has its own existing syntax/semantics; this change only
        // rejects word-based control constructs from the shared policy list.
        && !matches!(keyword, "{" | "}" | "(" | ")")
    {
        anyhow::bail!("syntax error: unsupported control keyword `{keyword}`");
    }
    // Include command/process substitutions in arguments, assignments and
    // redirects, even when execution or operand expansion would be gated out.
    for inner in pair.into_inner() {
        validate_pair(inner)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;
    use crate::parser::ShellParser;
    use crate::shell::parse::parse_execution_plan;
    use pest::Parser as _;
    use std::sync::Arc;

    #[test]
    fn all_control_words_are_rejected_only_at_unquoted_command_positions() {
        for keyword in [
            "if", "then", "elif", "else", "fi", "for", "while", "until", "do", "done", "case",
            "esac", "select", "function",
        ] {
            for input in [
                keyword.to_string(),
                format!("echo before; {keyword}; echo after"),
                format!("echo before | {keyword}"),
                format!("false && {keyword}"),
                format!("true || {keyword}"),
                format!("X=value > marker {keyword}"),
                format!("echo $({keyword})"),
                format!("echo > $({keyword})"),
                format!("X=$({keyword})"),
                format!("cat <({keyword})"),
                format!("echo > >({keyword})"),
                format!("echo ${{X:-$({keyword})}}"),
                format!("echo $((1 + $({keyword})))"),
            ] {
                let pairs = ShellParser::parse(Rule::commands, &input).expect("editor parse");
                let err = validate(pairs).expect_err(&input);
                assert!(err.to_string().contains(&format!("`{keyword}`")), "{err}");
            }
            for input in [
                format!("echo {keyword}"),
                format!("echo '{keyword}'"),
                format!("'{keyword}'"),
                format!("\"{keyword}\""),
                format!("\\{keyword}"),
                format!("{keyword}''"),
                format!("./{keyword}"),
                format!("{keyword}=value"),
                format!("echo > {keyword}"),
                format!("echo $(echo {keyword})"),
                format!("echo $(({keyword}=1))"),
            ] {
                parse_execution_plan(&input, Environment::new()).expect(&input);
            }
        }
        for input in [
            "iftop",
            "$COMMAND",
            "(echo hello)",
            "echo # existing syntax",
        ] {
            parse_execution_plan(input, Environment::new()).expect(input);
        }
    }

    #[test]
    fn raw_keywords_cannot_be_hidden_by_aliases() {
        let environment = Environment::new();
        environment
            .write()
            .variable_state
            .alias
            .insert("if".into(), "echo hidden".into());
        parse_execution_plan("if false", environment).expect_err("raw syntax must win");
    }

    #[test]
    fn aliases_are_validated_including_nested_command_positions() {
        let environment = Environment::new();
        environment
            .write()
            .variable_state
            .alias
            .insert("bad".into(), "if false; then echo wrong; fi".into());
        for input in [
            "bad",
            "echo before; bad; echo after",
            "echo $(bad)",
            "cat <(bad)",
            "X=value bad",
        ] {
            let err = parse_execution_plan(input, Arc::clone(&environment)).expect_err(input);
            assert!(
                err.to_string().contains("unsupported control keyword `if`"),
                "{err}"
            );
        }
        environment
            .write()
            .variable_state
            .alias
            .insert("words".into(), "echo if for while".into());
        for input in ["words", "echo $(words)"] {
            parse_execution_plan(input, Arc::clone(&environment)).expect(input);
        }
    }
}
