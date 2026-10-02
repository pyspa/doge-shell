//! Arithmetic AST evaluation against the logical shell environment.
//!
//! Pure integer semantics on `i64` with checked operations: overflow,
//! division by zero, bad shifts, and non-numeric variable values are typed
//! [`ArithmeticExpansionError`] failures, never panics and never
//! debug/release-dependent. `&&` / `||` / `?:` skip their dead branches
//! (including assignments inside them).

use super::parser::{ArithmeticExpr, AssignmentOp, BinaryOp, UnaryOp, parse_integer_literal};
use crate::shell::expansion_host::ExpansionHost;
use std::fmt;

/// Fatal `$((...))` expansion failure.
///
/// A typed semantic error in the same fatal-expansion category as
/// `${VAR:?}`: the top-level evaluator publishes status 1, aborts the
/// current evaluation, and never runs an `||` fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArithmeticExpansionError {
    pub expression: String,
    pub message: String,
}

impl fmt::Display for ArithmeticExpansionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "arithmetic expansion error in '{}': {}",
            self.expression, self.message
        )
    }
}

impl std::error::Error for ArithmeticExpansionError {}

/// Whether `err` (possibly wrapped in `anyhow`) is a typed arithmetic
/// expansion failure. Never classifies by substring matching.
pub(crate) fn is_arithmetic_expansion_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ArithmeticExpansionError>().is_some()
}

fn fail(expression: &str, message: String) -> ArithmeticExpansionError {
    ArithmeticExpansionError {
        expression: expression.to_string(),
        message,
    }
}

/// Resolve a bare identifier through the logical shell environment.
///
/// Unset and empty both mean `0`. A set non-empty value must parse with the
/// same `123` / `077` / `0x10` rules as literals; anything else is a typed
/// error, never a silent `0`.
fn resolve_variable(
    expression: &str,
    name: &str,
    shell: &impl ExpansionHost,
) -> Result<i64, ArithmeticExpansionError> {
    let value = shell.expansion_environment().read().lookup_variable(name);
    match value {
        None => Ok(0),
        Some(text) if text.is_empty() => Ok(0),
        Some(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Ok(0);
            }
            parse_integer_literal(trimmed)
                .map_err(|msg| fail(expression, format!("variable '{name}': {msg}")))
        }
    }
}

fn apply_assignment_op(
    expression: &str,
    op: AssignmentOp,
    current: i64,
    rhs: i64,
) -> Result<i64, ArithmeticExpansionError> {
    match op {
        AssignmentOp::Assign => Ok(rhs),
        AssignmentOp::AddAssign => current
            .checked_add(rhs)
            .ok_or_else(|| fail(expression, "integer overflow".to_string())),
        AssignmentOp::SubAssign => current
            .checked_sub(rhs)
            .ok_or_else(|| fail(expression, "integer overflow".to_string())),
        AssignmentOp::MulAssign => current
            .checked_mul(rhs)
            .ok_or_else(|| fail(expression, "integer overflow".to_string())),
        AssignmentOp::DivAssign => checked_div(expression, current, rhs),
        AssignmentOp::RemAssign => checked_rem(expression, current, rhs),
        AssignmentOp::ShlAssign => checked_shl(expression, current, rhs),
        AssignmentOp::ShrAssign => checked_shr(expression, current, rhs),
        AssignmentOp::BitAndAssign => Ok(current & rhs),
        AssignmentOp::BitXorAssign => Ok(current ^ rhs),
        AssignmentOp::BitOrAssign => Ok(current | rhs),
    }
}

fn checked_div(expression: &str, lhs: i64, rhs: i64) -> Result<i64, ArithmeticExpansionError> {
    if rhs == 0 {
        return Err(fail(expression, "division by zero".to_string()));
    }
    lhs.checked_div(rhs)
        .ok_or_else(|| fail(expression, "integer overflow".to_string()))
}

fn checked_rem(expression: &str, lhs: i64, rhs: i64) -> Result<i64, ArithmeticExpansionError> {
    if rhs == 0 {
        return Err(fail(expression, "modulo by zero".to_string()));
    }
    lhs.checked_rem(rhs)
        .ok_or_else(|| fail(expression, "integer overflow".to_string()))
}

fn checked_shl(expression: &str, lhs: i64, rhs: i64) -> Result<i64, ArithmeticExpansionError> {
    if !(0..64).contains(&rhs) {
        return Err(fail(expression, "invalid shift amount".to_string()));
    }
    lhs.checked_shl(rhs as u32)
        .ok_or_else(|| fail(expression, "invalid shift amount".to_string()))
}

fn checked_shr(expression: &str, lhs: i64, rhs: i64) -> Result<i64, ArithmeticExpansionError> {
    if !(0..64).contains(&rhs) {
        return Err(fail(expression, "invalid shift amount".to_string()));
    }
    lhs.checked_shr(rhs as u32)
        .ok_or_else(|| fail(expression, "invalid shift amount".to_string()))
}

/// Evaluate one AST against the current shell environment.
///
/// Assignments mutate through `set_shell_var` (preserving export state) and
/// yield the stored value. Skipped `&&` / `||` / `?:` branches are never
/// evaluated, so neither their errors nor their assignments happen.
pub(crate) fn eval_expr(
    expression: &str,
    expr: &ArithmeticExpr,
    shell: &mut impl ExpansionHost,
) -> Result<i64, ArithmeticExpansionError> {
    match expr {
        ArithmeticExpr::Integer(value) => Ok(*value),
        ArithmeticExpr::Variable(name) => resolve_variable(expression, name, shell),
        ArithmeticExpr::Unary { op, expr } => {
            let value = eval_expr(expression, expr, shell)?;
            match op {
                UnaryOp::Plus => Ok(value),
                UnaryOp::Minus => value
                    .checked_neg()
                    .ok_or_else(|| fail(expression, "integer overflow".to_string())),
                UnaryOp::Not => Ok(i64::from(value == 0)),
                UnaryOp::BitNot => Ok(!value),
            }
        }
        ArithmeticExpr::Binary { op, lhs, rhs } => match op {
            BinaryOp::LogAnd => {
                let left = eval_expr(expression, lhs, shell)?;
                if left == 0 {
                    return Ok(0);
                }
                let right = eval_expr(expression, rhs, shell)?;
                Ok(i64::from(right != 0))
            }
            BinaryOp::LogOr => {
                let left = eval_expr(expression, lhs, shell)?;
                if left != 0 {
                    return Ok(1);
                }
                let right = eval_expr(expression, rhs, shell)?;
                Ok(i64::from(right != 0))
            }
            _ => {
                let left = eval_expr(expression, lhs, shell)?;
                let right = eval_expr(expression, rhs, shell)?;
                match op {
                    BinaryOp::Mul => left
                        .checked_mul(right)
                        .ok_or_else(|| fail(expression, "integer overflow".to_string())),
                    BinaryOp::Div => checked_div(expression, left, right),
                    BinaryOp::Rem => checked_rem(expression, left, right),
                    BinaryOp::Add => left
                        .checked_add(right)
                        .ok_or_else(|| fail(expression, "integer overflow".to_string())),
                    BinaryOp::Sub => left
                        .checked_sub(right)
                        .ok_or_else(|| fail(expression, "integer overflow".to_string())),
                    BinaryOp::Shl => checked_shl(expression, left, right),
                    BinaryOp::Shr => checked_shr(expression, left, right),
                    BinaryOp::Lt => Ok(i64::from(left < right)),
                    BinaryOp::Le => Ok(i64::from(left <= right)),
                    BinaryOp::Gt => Ok(i64::from(left > right)),
                    BinaryOp::Ge => Ok(i64::from(left >= right)),
                    BinaryOp::Eq => Ok(i64::from(left == right)),
                    BinaryOp::Ne => Ok(i64::from(left != right)),
                    BinaryOp::BitAnd => Ok(left & right),
                    BinaryOp::BitXor => Ok(left ^ right),
                    BinaryOp::BitOr => Ok(left | right),
                    BinaryOp::LogAnd | BinaryOp::LogOr => unreachable!(),
                }
            }
        },
        ArithmeticExpr::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            let cond = eval_expr(expression, condition, shell)?;
            if cond != 0 {
                eval_expr(expression, then_expr, shell)
            } else {
                eval_expr(expression, else_expr, shell)
            }
        }
        ArithmeticExpr::Assignment { target, op, rhs } => {
            if !is_valid_assignment_target(target) {
                return Err(fail(expression, "invalid assignment target".to_string()));
            }
            let rhs_value = eval_expr(expression, rhs, shell)?;
            let current = resolve_variable(expression, target, shell)?;
            let result = apply_assignment_op(expression, *op, current, rhs_value)?;
            shell
                .expansion_environment()
                .write()
                .set_shell_var(target.clone(), result.to_string());
            Ok(result)
        }
    }
}

fn is_valid_assignment_target(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parse and evaluate one expanded body in a single step.
///
/// Syntax is checked before any assignment runs because parsing builds the
/// whole AST first; short-circuit and assignment rules live in `eval_expr`.
pub(crate) fn evaluate_expression(
    expression: &str,
    body: &str,
    shell: &mut impl ExpansionHost,
) -> Result<i64, ArithmeticExpansionError> {
    let ast = super::parser::parse_arithmetic(expression, body)?;
    eval_expr(expression, &ast, shell)
}

#[cfg(test)]
mod tests {
    use super::super::parser::parse_arithmetic;
    use super::*;
    use crate::shell::Shell;

    fn test_shell() -> Shell {
        Shell::new(crate::environment::Environment::new())
    }

    fn eval_ok(body: &str, shell: &mut Shell) -> i64 {
        let ast = parse_arithmetic(body, body).expect("parse");
        eval_expr(body, &ast, shell).expect("eval")
    }

    fn eval_err(body: &str, shell: &mut Shell) -> String {
        let ast = parse_arithmetic(body, body).expect("parse");
        eval_expr(body, &ast, shell).expect_err("must fail").message
    }

    #[test]
    fn precedence_values() {
        let mut shell = test_shell();
        assert_eq!(eval_ok("1 + 2 * 3", &mut shell), 7);
        assert_eq!(eval_ok("(1 + 2) * 3", &mut shell), 9);
        assert_eq!(eval_ok("1 << 2 + 1", &mut shell), 8);
        assert_eq!(eval_ok("-1", &mut shell), -1);
        assert_eq!(eval_ok("+1", &mut shell), 1);
        assert_eq!(eval_ok("!0", &mut shell), 1);
        assert_eq!(eval_ok("!1", &mut shell), 0);
        assert_eq!(eval_ok("~0", &mut shell), -1);
    }

    #[test]
    fn comparison_results_are_zero_or_one() {
        let mut shell = test_shell();
        for (body, expected) in [
            ("1 < 2", 1),
            ("2 <= 2", 1),
            ("3 > 2", 1),
            ("3 >= 3", 1),
            ("2 == 2", 1),
            ("2 != 3", 1),
            ("2 < 1", 0),
            ("1 == 2", 0),
        ] {
            assert_eq!(eval_ok(body, &mut shell), expected, "for {body}");
        }
    }

    #[test]
    fn bitwise_and_logical_values() {
        let mut shell = test_shell();
        assert_eq!(eval_ok("6 & 3", &mut shell), 2);
        assert_eq!(eval_ok("6 ^ 3", &mut shell), 5);
        assert_eq!(eval_ok("6 | 3", &mut shell), 7);
        assert_eq!(eval_ok("1 << 3", &mut shell), 8);
        assert_eq!(eval_ok("8 >> 2", &mut shell), 2);
        assert_eq!(eval_ok("1 && 2", &mut shell), 1);
        assert_eq!(eval_ok("0 && 2", &mut shell), 0);
        assert_eq!(eval_ok("0 || 2", &mut shell), 1);
        assert_eq!(eval_ok("0 || 0", &mut shell), 0);
    }

    #[test]
    fn conditional_values() {
        let mut shell = test_shell();
        assert_eq!(eval_ok("1 ? 2 : 3", &mut shell), 2);
        assert_eq!(eval_ok("0 ? 2 : 3", &mut shell), 3);
    }

    #[test]
    fn assignment_and_right_association() {
        let mut shell = test_shell();
        assert_eq!(eval_ok("X = 3", &mut shell), 3);
        assert_eq!(
            shell.environment.read().lookup_variable("X").as_deref(),
            Some("3")
        );
        assert_eq!(eval_ok("X += 2", &mut shell), 5);
        assert_eq!(eval_ok("X *= 4", &mut shell), 20);
        let mut shell = test_shell();
        assert_eq!(eval_ok("A = B = 3", &mut shell), 3);
        assert_eq!(
            shell.environment.read().lookup_variable("A").as_deref(),
            Some("3")
        );
        assert_eq!(
            shell.environment.read().lookup_variable("B").as_deref(),
            Some("3")
        );
    }

    #[test]
    fn variable_resolution_rules() {
        let mut shell = test_shell();
        // Unset and empty are zero.
        assert_eq!(eval_ok("DOGESH_UNSET_ARITH + 1", &mut shell), 1);
        shell
            .environment
            .write()
            .set_shell_var("DOGESH_EMPTY_ARITH".to_string(), String::new());
        assert_eq!(eval_ok("DOGESH_EMPTY_ARITH + 1", &mut shell), 1);
        // Octal variable text.
        shell
            .environment
            .write()
            .set_shell_var("DOGESH_OCT_ARITH".to_string(), "010".to_string());
        assert_eq!(eval_ok("DOGESH_OCT_ARITH + 1", &mut shell), 9);
        // Invalid text is a typed error.
        shell
            .environment
            .write()
            .set_shell_var("DOGESH_BAD_ARITH".to_string(), "abc".to_string());
        assert!(!eval_err("DOGESH_BAD_ARITH + 1", &mut shell).is_empty());
    }

    #[test]
    fn short_circuit_skips_errors_and_assignments() {
        let mut shell = test_shell();
        shell
            .environment
            .write()
            .set_shell_var("DOGESH_SC".to_string(), "1".to_string());
        assert_eq!(eval_ok("0 && (1 / 0)", &mut shell), 0);
        assert_eq!(eval_ok("1 || (1 / 0)", &mut shell), 1);
        assert_eq!(eval_ok("1 ? 10 : (1 / 0)", &mut shell), 10);
        assert_eq!(eval_ok("0 ? (1 / 0) : 20", &mut shell), 20);
        // Skipped assignment never runs.
        assert_eq!(eval_ok("1 || (DOGESH_SC = 9)", &mut shell), 1);
        assert_eq!(
            shell
                .environment
                .read()
                .lookup_variable("DOGESH_SC")
                .as_deref(),
            Some("1")
        );
        assert_eq!(eval_ok("0 && (DOGESH_SC = 9)", &mut shell), 0);
        assert_eq!(
            shell
                .environment
                .read()
                .lookup_variable("DOGESH_SC")
                .as_deref(),
            Some("1")
        );
    }

    #[test]
    fn division_and_overflow_are_errors() {
        let mut shell = test_shell();
        assert!(!eval_err("1 / 0", &mut shell).is_empty());
        assert!(!eval_err("1 % 0", &mut shell).is_empty());
        assert!(!eval_err("X << -1", &mut shell).is_empty());
        assert!(!eval_err("1 << 64", &mut shell).is_empty());
        assert!(!eval_err("9223372036854775807 + 1", &mut shell).is_empty());
    }

    #[test]
    fn error_is_typed_not_substring() {
        let err = ArithmeticExpansionError {
            expression: "1/0".to_string(),
            message: "division by zero".to_string(),
        };
        let wrapped = anyhow::anyhow!(err.clone());
        assert!(is_arithmetic_expansion_error(&wrapped));
        assert!(!is_arithmetic_expansion_error(&anyhow::anyhow!("1/0")));
    }
}
