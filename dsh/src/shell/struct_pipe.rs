//! Desugars the `|:` structured-pipe DSL into the S-expressions the existing
//! `table-*` Lisp functions already understand.
//!
//! `ps aux |: where cpu > 5 | select pid command` becomes
//! `(table-select (table-where-cmp $_ "cpu" ">" 5) (list "pid" "command"))`.
//! This is pure text-in, text-out: no terminal, no Lisp engine, nothing that
//! needs a running shell. It runs once per `struct_pipe_dsl` match at parse
//! time (see `dsh/src/shell/parse.rs`'s `Rule::struct_pipe_command` arm), so
//! the desugared S-expression is what ends up in `Job::struct_pipe_exprs`,
//! gets re-run verbatim by `blocks rerun`, and is what a `!` chat or
//! `blocks show` would display -- there is no separate "original DSL text"
//! kept anywhere.
//!
//! The Lisp reader's string literal has no escape syntax at all (see
//! `dsh/src/lisp/parser.rs`'s `parse_string`: the first `"` after the
//! opening one closes the string, full stop). Any DSL value containing a
//! `"` therefore cannot be represented and is rejected here rather than
//! silently mis-parsed downstream.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesugarError {
    /// The DSL text starts with `(` but pest's `lisp_expr` arm didn't match
    /// it -- almost certainly unbalanced parentheses in a Lisp expression,
    /// not DSL text at all.
    UnbalancedLispExpr,
    /// A pipeline stage (between two `|`, or the whole DSL) is empty.
    EmptyStage,
    /// The first word of a stage is not a known DSL keyword.
    UnknownStage(String),
    /// A stage got the wrong number of words.
    WrongArgumentCount {
        stage: &'static str,
        expected: &'static str,
    },
    /// `where`'s operator word/symbol was not recognised.
    UnknownOperator(String),
    /// `>` `>=` `<` `<=` need a numeric, unquoted value.
    ComparisonNeedsNumber { op: String },
    /// `head`/`tail`'s count is not a non-negative integer.
    InvalidCount(String),
    /// A quote was opened but never closed.
    UnterminatedQuote,
    /// A value contains `"`, which the Lisp reader cannot represent in a
    /// string literal (no escape syntax).
    UnsupportedQuoteInValue,
}

impl fmt::Display for DesugarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnbalancedLispExpr => write!(
                f,
                "`|:` expression starts with '(' but its parentheses never balance"
            ),
            Self::EmptyStage => write!(f, "`|:` expression is empty"),
            Self::UnknownStage(word) => write!(
                f,
                "unknown `|:` stage '{word}' (expected one of: where, select, sort, head, tail, count, distinct, rename, group-by, count-by, sum, avg, min, max, to-json, to-csv)"
            ),
            Self::WrongArgumentCount { stage, expected } => {
                write!(f, "`{stage}` expects {expected}")
            }
            Self::UnknownOperator(op) => write!(
                f,
                "unknown `where` operator '{op}' (expected one of: > >= < <= = == != is contains)"
            ),
            Self::ComparisonNeedsNumber { op } => write!(
                f,
                "`where ... {op} ...` needs an unquoted numeric value on the right"
            ),
            Self::InvalidCount(text) => {
                write!(f, "'{text}' is not a non-negative integer")
            }
            Self::UnterminatedQuote => write!(f, "unterminated quote in `|:` expression"),
            Self::UnsupportedQuoteInValue => write!(
                f,
                "a `|:` value cannot contain '\"' -- the Lisp reader has no string escape syntax"
            ),
        }
    }
}

impl std::error::Error for DesugarError {}

/// One DSL word. `quoted` records whether it came from `"..."`/`'...'` so a
/// numeric-looking quoted value (`where id == "007"`) is still emitted as a
/// string, not a number.
struct Token {
    text: String,
    quoted: bool,
}

/// Desugars `|:` DSL text (already trimmed of the leading `|:` and
/// surrounding whitespace by the grammar) into a Lisp S-expression string.
///
/// Text starting with `(` is rejected outright: `lisp_expr` in `shell.pest`
/// tries first and only falls through to this DSL path when its parentheses
/// never balance, so reaching here with a leading `(` means the S-expression
/// was malformed, not that the user wrote DSL.
pub fn desugar(expr: &str) -> Result<String, DesugarError> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return Err(DesugarError::EmptyStage);
    }
    if trimmed.starts_with('(') {
        return Err(DesugarError::UnbalancedLispExpr);
    }

    let mut acc = "$_".to_string();
    for stage_text in split_stages(trimmed)? {
        let tokens = tokenize(&stage_text)?;
        acc = desugar_stage(&tokens, &acc)?;
    }
    Ok(acc)
}

/// Desugars `expr`, falling back to a Lisp form that raises `err` as a
/// runtime error instead of propagating it as a parse-time `Result::Err`.
///
/// `dsh/src/shell/parse.rs` builds one job list per `;`/`&&`/`||`-joined
/// line and discards the whole thing on the first `Result::Err` (a
/// pre-existing property of the parser, not new here) -- so a hard error
/// from a malformed `|:` stage would silently take every other command on
/// the same line down with it. Wrapping the message in
/// `(dsh-struct-pipe-error "...")` instead means only *this* job's
/// struct-pipe step fails, and only when it actually runs, through the
/// same "Struct pipe error: ..." path any other struct-pipe failure
/// already goes through.
pub fn desugar_or_error_call(expr: &str) -> String {
    match desugar(expr) {
        Ok(lisp) => lisp,
        Err(err) => {
            // The Lisp reader has no string escape syntax at all (see the
            // module doc), so `"` can't be embedded literally; `expr` is
            // arbitrary user text (an `UnterminatedQuote` error's `expr` may
            // itself contain one), so sanitise rather than risk
            // `lisp_string_literal` rejecting it and losing the message.
            let sanitized = format!("'{}': {err}", expr.trim()).replace('"', "'");
            format!("(dsh-struct-pipe-error \"{sanitized}\")")
        }
    }
}

/// Splits DSL text on unquoted `|` (the DSL's own stage separator -- distinct
/// from the shell's `|:`/`|` operators, which the grammar has already
/// stopped at before this text is handed over).
fn split_stages(s: &str) -> Result<Vec<String>, DesugarError> {
    let mut stages = Vec::new();
    let mut current = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                current.push(c);
                let quote = c;
                loop {
                    match chars.next() {
                        Some(c2) => {
                            current.push(c2);
                            if c2 == quote {
                                break;
                            }
                        }
                        None => return Err(DesugarError::UnterminatedQuote),
                    }
                }
            }
            '|' => {
                stages.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    stages.push(current.trim().to_string());

    if stages.iter().any(|stage| stage.is_empty()) {
        return Err(DesugarError::EmptyStage);
    }
    Ok(stages)
}

/// Splits one stage into whitespace-separated words, honouring quotes.
fn tokenize(s: &str) -> Result<Vec<Token>, DesugarError> {
    let mut tokens = Vec::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            let mut buf = String::new();
            loop {
                match chars.next() {
                    Some(c2) if c2 == quote => break,
                    Some(c2) => buf.push(c2),
                    None => return Err(DesugarError::UnterminatedQuote),
                }
            }
            tokens.push(Token {
                text: buf,
                quoted: true,
            });
        } else {
            let mut buf = String::new();
            buf.push(c);
            while let Some(&next) = chars.peek() {
                if next.is_whitespace() || next == '"' || next == '\'' {
                    break;
                }
                buf.push(next);
                chars.next();
            }
            tokens.push(Token {
                text: buf,
                quoted: false,
            });
        }
    }
    Ok(tokens)
}

/// `^-?\d+(\.\d+)?$` -- deliberately stricter than `str::parse::<f64>`
/// (which also accepts `inf`/`nan`/scientific notation) so every literal we
/// call numeric here is one the Lisp number reader also accepts verbatim
/// (`dsh/src/lisp/parser.rs`'s `parse_number`).
fn is_numeric_literal(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    if s.is_empty() {
        return false;
    }
    let (int_part, frac_part) = match s.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    if int_part.is_empty() || !int_part.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    match frac_part {
        Some(f) => !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()),
        None => true,
    }
}

/// Renders `s` as a Lisp string literal. Errors if `s` contains `"`: the
/// reader has no escape syntax, so there is no way to represent it.
fn lisp_string_literal(s: &str) -> Result<String, DesugarError> {
    if s.contains('"') {
        return Err(DesugarError::UnsupportedQuoteInValue);
    }
    Ok(format!("\"{s}\""))
}

/// Emits a token as a Lisp literal: quoted tokens and non-numeric barewords
/// become strings, unquoted numeric barewords become bare number literals.
fn emit_value(token: &Token) -> Result<String, DesugarError> {
    if !token.quoted && is_numeric_literal(&token.text) {
        Ok(token.text.clone())
    } else {
        lisp_string_literal(&token.text)
    }
}

fn desugar_stage(tokens: &[Token], input: &str) -> Result<String, DesugarError> {
    let Some(keyword) = tokens.first() else {
        return Err(DesugarError::EmptyStage);
    };
    match keyword.text.to_ascii_lowercase().as_str() {
        "where" => desugar_where(tokens, input),
        "select" => desugar_select(tokens, input),
        "sort" => desugar_sort(tokens, input),
        "head" => desugar_head_tail(tokens, input, "table-head", "head"),
        "tail" => desugar_head_tail(tokens, input, "table-tail", "tail"),
        "count" => desugar_nullary(tokens, input, "table-count", "count"),
        "to-json" => desugar_nullary(tokens, input, "json-stringify", "to-json"),
        "to-csv" => desugar_nullary(tokens, input, "csv-stringify", "to-csv"),
        "distinct" => desugar_unary_column(tokens, input, "table-distinct", "distinct"),
        "group-by" => desugar_unary_column(tokens, input, "table-group-by", "group-by"),
        "count-by" => desugar_unary_column(tokens, input, "table-count-by", "count-by"),
        "sum" => desugar_unary_column(tokens, input, "table-sum", "sum"),
        "avg" => desugar_unary_column(tokens, input, "table-avg", "avg"),
        "min" => desugar_unary_column(tokens, input, "table-min", "min"),
        "max" => desugar_unary_column(tokens, input, "table-max", "max"),
        "rename" => desugar_rename(tokens, input),
        other => Err(DesugarError::UnknownStage(other.to_string())),
    }
}

fn desugar_where(tokens: &[Token], input: &str) -> Result<String, DesugarError> {
    if tokens.len() != 4 {
        return Err(DesugarError::WrongArgumentCount {
            stage: "where",
            expected: "3 arguments: column, operator, value",
        });
    }
    let column = lisp_string_literal(&tokens[1].text)?;
    let op = tokens[2].text.as_str();
    let value = &tokens[3];

    match op {
        ">" | ">=" | "<" | "<=" => {
            if value.quoted || !is_numeric_literal(&value.text) {
                return Err(DesugarError::ComparisonNeedsNumber { op: op.to_string() });
            }
            Ok(format!(
                "(table-where-cmp {input} {column} \"{op}\" {})",
                value.text
            ))
        }
        // `=` is accepted alongside `==` because `table-where-cmp`'s own
        // model (`Table::where_cmp`) already treats them as synonyms; the
        // DSL rejecting one of the two would be a gratuitous inconsistency
        // with the primitive it desugars to.
        "=" | "==" | "!=" => {
            if !value.quoted && is_numeric_literal(&value.text) {
                Ok(format!(
                    "(table-where-cmp {input} {column} \"{op}\" {})",
                    value.text
                ))
            } else if op == "!=" {
                Ok(format!(
                    "(table-where-ne {input} {column} {})",
                    emit_value(value)?
                ))
            } else {
                Ok(format!(
                    "(table-where-eq {input} {column} {})",
                    emit_value(value)?
                ))
            }
        }
        _ if op.eq_ignore_ascii_case("is") => Ok(format!(
            "(table-where-eq {input} {column} {})",
            emit_value(value)?
        )),
        _ if op.eq_ignore_ascii_case("contains") => Ok(format!(
            "(table-where-contains {input} {column} {})",
            lisp_string_literal(&value.text)?
        )),
        other => Err(DesugarError::UnknownOperator(other.to_string())),
    }
}

fn desugar_select(tokens: &[Token], input: &str) -> Result<String, DesugarError> {
    if tokens.len() < 2 {
        return Err(DesugarError::WrongArgumentCount {
            stage: "select",
            expected: "at least one column name",
        });
    }
    let columns = tokens[1..]
        .iter()
        .map(|t| lisp_string_literal(&t.text))
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    Ok(format!("(table-select {input} (list {columns}))"))
}

fn desugar_sort(tokens: &[Token], input: &str) -> Result<String, DesugarError> {
    if tokens.len() != 2 {
        return Err(DesugarError::WrongArgumentCount {
            stage: "sort",
            expected: "exactly one column name (optionally prefixed with '-' for descending)",
        });
    }
    let raw = tokens[1].text.as_str();
    let (column, desc) = match raw.strip_prefix('-') {
        Some(rest) if !rest.is_empty() => (rest, true),
        _ => (raw, false),
    };
    let column = lisp_string_literal(column)?;
    if desc {
        Ok(format!("(table-order-by {input} {column} :desc)"))
    } else {
        Ok(format!("(table-order-by {input} {column})"))
    }
}

fn desugar_head_tail(
    tokens: &[Token],
    input: &str,
    func: &str,
    stage: &'static str,
) -> Result<String, DesugarError> {
    if tokens.is_empty() || tokens.len() > 2 {
        return Err(DesugarError::WrongArgumentCount {
            stage,
            expected: "an optional row count",
        });
    }
    let count = if let Some(count_token) = tokens.get(1) {
        count_token
            .text
            .parse::<usize>()
            .map_err(|_| DesugarError::InvalidCount(count_token.text.clone()))?
    } else {
        10
    };
    Ok(format!("({func} {input} {count})"))
}

fn desugar_nullary(
    tokens: &[Token],
    input: &str,
    func: &str,
    stage: &'static str,
) -> Result<String, DesugarError> {
    if tokens.len() != 1 {
        return Err(DesugarError::WrongArgumentCount {
            stage,
            expected: "no arguments",
        });
    }
    Ok(format!("({func} {input})"))
}

fn desugar_unary_column(
    tokens: &[Token],
    input: &str,
    func: &str,
    stage: &'static str,
) -> Result<String, DesugarError> {
    if tokens.len() != 2 {
        return Err(DesugarError::WrongArgumentCount {
            stage,
            expected: "exactly one column name",
        });
    }
    let column = lisp_string_literal(&tokens[1].text)?;
    Ok(format!("({func} {input} {column})"))
}

fn desugar_rename(tokens: &[Token], input: &str) -> Result<String, DesugarError> {
    if tokens.len() != 3 {
        return Err(DesugarError::WrongArgumentCount {
            stage: "rename",
            expected: "2 arguments: old column name, new column name",
        });
    }
    let old = lisp_string_literal(&tokens[1].text)?;
    let new = lisp_string_literal(&tokens[2].text)?;
    Ok(format!("(table-rename {input} {old} {new})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_paren_falls_through_to_lisp_error() {
        assert_eq!(
            desugar("(table-head $_ 3"),
            Err(DesugarError::UnbalancedLispExpr)
        );
    }

    #[test]
    fn where_numeric_comparison() {
        assert_eq!(
            desugar("where cpu > 5").unwrap(),
            r#"(table-where-cmp $_ "cpu" ">" 5)"#
        );
        assert_eq!(
            desugar("where cpu >= 5.5").unwrap(),
            r#"(table-where-cmp $_ "cpu" ">=" 5.5)"#
        );
        assert_eq!(
            desugar("where cpu < -1").unwrap(),
            r#"(table-where-cmp $_ "cpu" "<" -1)"#
        );
    }

    #[test]
    fn where_equality_dispatches_on_value_shape() {
        // Unquoted numeric -> comparison.
        assert_eq!(
            desugar("where pid == 42").unwrap(),
            r#"(table-where-cmp $_ "pid" "==" 42)"#
        );
        // Bareword -> eq.
        assert_eq!(
            desugar("where status == Up").unwrap(),
            r#"(table-where-eq $_ "status" "Up")"#
        );
        // Quoted, numeric-looking -> still a string, not a number.
        assert_eq!(
            desugar(r#"where id == "007""#).unwrap(),
            r#"(table-where-eq $_ "id" "007")"#
        );
    }

    #[test]
    fn where_accepts_bare_equals_like_table_where_cmp_does() {
        assert_eq!(
            desugar("where pid = 42").unwrap(),
            r#"(table-where-cmp $_ "pid" "=" 42)"#
        );
        assert_eq!(
            desugar("where status = Up").unwrap(),
            r#"(table-where-eq $_ "status" "Up")"#
        );
    }

    #[test]
    fn where_ne_dispatches_on_value_shape() {
        assert_eq!(
            desugar("where pid != 1").unwrap(),
            r#"(table-where-cmp $_ "pid" "!=" 1)"#
        );
        assert_eq!(
            desugar("where status != Up").unwrap(),
            r#"(table-where-ne $_ "status" "Up")"#
        );
    }

    #[test]
    fn where_is_and_contains() {
        assert_eq!(
            desugar("where status is Up").unwrap(),
            r#"(table-where-eq $_ "status" "Up")"#
        );
        assert_eq!(
            desugar("where command contains rm").unwrap(),
            r#"(table-where-contains $_ "command" "rm")"#
        );
        assert_eq!(
            desugar(r#"where name contains "a b""#).unwrap(),
            r#"(table-where-contains $_ "name" "a b")"#
        );
    }

    #[test]
    fn where_comparison_rejects_non_numeric() {
        assert_eq!(
            desugar("where cpu > high"),
            Err(DesugarError::ComparisonNeedsNumber {
                op: ">".to_string()
            })
        );
        assert_eq!(
            desugar(r#"where cpu > "5""#),
            Err(DesugarError::ComparisonNeedsNumber {
                op: ">".to_string()
            })
        );
    }

    #[test]
    fn where_unknown_operator() {
        assert_eq!(
            desugar("where cpu near 5"),
            Err(DesugarError::UnknownOperator("near".to_string()))
        );
    }

    #[test]
    fn select_multiple_columns() {
        assert_eq!(
            desugar("select pid command").unwrap(),
            r#"(table-select $_ (list "pid" "command"))"#
        );
    }

    #[test]
    fn sort_ascending_and_descending() {
        assert_eq!(desugar("sort cpu").unwrap(), r#"(table-order-by $_ "cpu")"#);
        assert_eq!(
            desugar("sort -cpu").unwrap(),
            r#"(table-order-by $_ "cpu" :desc)"#
        );
    }

    #[test]
    fn head_and_tail_default_and_explicit() {
        assert_eq!(desugar("head").unwrap(), "(table-head $_ 10)");
        assert_eq!(desugar("head 3").unwrap(), "(table-head $_ 3)");
        assert_eq!(desugar("tail").unwrap(), "(table-tail $_ 10)");
        assert_eq!(desugar("tail 5").unwrap(), "(table-tail $_ 5)");
    }

    #[test]
    fn head_rejects_non_integer_count() {
        assert_eq!(
            desugar("head many"),
            Err(DesugarError::InvalidCount("many".to_string()))
        );
    }

    #[test]
    fn count_and_export_forms() {
        assert_eq!(desugar("count").unwrap(), "(table-count $_)");
        assert_eq!(desugar("to-json").unwrap(), "(json-stringify $_)");
        assert_eq!(desugar("to-csv").unwrap(), "(csv-stringify $_)");
    }

    #[test]
    fn unary_column_stages() {
        assert_eq!(
            desugar("distinct user").unwrap(),
            r#"(table-distinct $_ "user")"#
        );
        assert_eq!(
            desugar("group-by user").unwrap(),
            r#"(table-group-by $_ "user")"#
        );
        assert_eq!(
            desugar("count-by user").unwrap(),
            r#"(table-count-by $_ "user")"#
        );
        assert_eq!(desugar("sum cpu").unwrap(), r#"(table-sum $_ "cpu")"#);
        assert_eq!(desugar("avg cpu").unwrap(), r#"(table-avg $_ "cpu")"#);
        assert_eq!(desugar("min cpu").unwrap(), r#"(table-min $_ "cpu")"#);
        assert_eq!(desugar("max cpu").unwrap(), r#"(table-max $_ "cpu")"#);
    }

    #[test]
    fn rename_stage() {
        assert_eq!(
            desugar("rename cpu cpu_percent").unwrap(),
            r#"(table-rename $_ "cpu" "cpu_percent")"#
        );
    }

    #[test]
    fn unary_column_stage_rejects_wrong_argument_count() {
        assert_eq!(
            desugar("sum"),
            Err(DesugarError::WrongArgumentCount {
                stage: "sum",
                expected: "exactly one column name",
            })
        );
        assert_eq!(
            desugar("rename cpu"),
            Err(DesugarError::WrongArgumentCount {
                stage: "rename",
                expected: "2 arguments: old column name, new column name",
            })
        );
    }

    #[test]
    fn chained_stages_nest_around_the_previous_result() {
        assert_eq!(
            desugar("where cpu > 5 | select pid command | head 3").unwrap(),
            r#"(table-head (table-select (table-where-cmp $_ "cpu" ">" 5) (list "pid" "command")) 3)"#
        );
    }

    #[test]
    fn unknown_stage_is_reported() {
        assert_eq!(
            desugar("frobnicate"),
            Err(DesugarError::UnknownStage("frobnicate".to_string()))
        );
    }

    #[test]
    fn empty_and_blank_are_rejected() {
        assert_eq!(desugar(""), Err(DesugarError::EmptyStage));
        assert_eq!(desugar("   "), Err(DesugarError::EmptyStage));
        assert_eq!(
            desugar("where cpu > 5 | | head"),
            Err(DesugarError::EmptyStage)
        );
    }

    #[test]
    fn unterminated_quote_is_reported() {
        assert_eq!(
            desugar(r#"where name is "unterminated"#),
            Err(DesugarError::UnterminatedQuote)
        );
    }

    #[test]
    fn value_with_embedded_quote_is_rejected() {
        assert_eq!(
            desugar(r#"where name is 'has "quote"'"#),
            Err(DesugarError::UnsupportedQuoteInValue)
        );
    }

    #[test]
    fn desugar_or_error_call_passes_through_success() {
        assert_eq!(
            desugar_or_error_call("where cpu > 5"),
            r#"(table-where-cmp $_ "cpu" ">" 5)"#
        );
    }

    #[test]
    fn desugar_or_error_call_turns_failure_into_a_runtime_error_form() {
        let call = desugar_or_error_call("frobnicate");
        assert!(call.starts_with("(dsh-struct-pipe-error \""));
        assert!(call.contains("frobnicate"));
        assert!(call.contains("unknown"));
        // The message must not contain an unescaped `"` -- the Lisp reader
        // has no string escape syntax, so that would truncate the literal.
        let inner = call
            .strip_prefix("(dsh-struct-pipe-error \"")
            .and_then(|s| s.strip_suffix("\")"))
            .unwrap();
        assert!(!inner.contains('"'));
    }

    #[test]
    fn desugar_or_error_call_sanitizes_a_message_containing_a_quote() {
        // `expr` itself contains `"` (an unterminated double-quoted value);
        // the generated call must still be well-formed.
        let call = desugar_or_error_call(r#"where name is "unterminated"#);
        assert!(call.starts_with("(dsh-struct-pipe-error \""));
        assert!(call.ends_with("\")"));
        let inner = &call[call.find('"').unwrap() + 1..call.rfind('"').unwrap()];
        assert!(!inner.contains('"'));
    }
}
