//! Arithmetic expression recursive-descent parser.
//!
//! Produces an [`ArithmeticExpr`] AST without evaluating anything: syntax
//! errors are reported before any assignment can run, and `&&` / `||` /
//! `?:` stay lazy for the evaluator. No shell state is touched here.

use super::eval::ArithmeticExpansionError;
use super::token::{Token, tokenize};

/// One parsed `$((...))` body after shell expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArithmeticExpr {
    Integer(i64),
    Variable(String),
    Unary {
        op: UnaryOp,
        expr: Box<ArithmeticExpr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<ArithmeticExpr>,
        rhs: Box<ArithmeticExpr>,
    },
    Conditional {
        condition: Box<ArithmeticExpr>,
        then_expr: Box<ArithmeticExpr>,
        else_expr: Box<ArithmeticExpr>,
    },
    Assignment {
        target: String,
        op: AssignmentOp,
        rhs: Box<ArithmeticExpr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Plus,
    Minus,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinaryOp {
    Mul,
    Div,
    Rem,
    Add,
    Sub,
    Shl,
    Shr,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    BitAnd,
    BitXor,
    BitOr,
    LogAnd,
    LogOr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssignmentOp {
    Assign,
    MulAssign,
    DivAssign,
    RemAssign,
    AddAssign,
    SubAssign,
    ShlAssign,
    ShrAssign,
    BitAndAssign,
    BitXorAssign,
    BitOrAssign,
}

struct Cursor<'a> {
    tokens: &'a [Token],
    pos: usize,
    expression: String,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.pos).cloned();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn error(&self, message: String) -> ArithmeticExpansionError {
        ArithmeticExpansionError {
            expression: self.expression.clone(),
            message,
        }
    }
}

fn describe(tok: Option<&Token>) -> String {
    match tok {
        None => "end of expression".to_string(),
        Some(Token::Integer(s) | Token::Ident(s) | Token::Op(s)) => format!("'{s}'"),
        Some(Token::LParen) => "'('".to_string(),
        Some(Token::RParen) => "')'".to_string(),
        Some(Token::Question) => "'?'".to_string(),
        Some(Token::Colon) => "':'".to_string(),
    }
}

fn is_assignment_op(op: &str) -> bool {
    matches!(
        op,
        "=" | "*=" | "/=" | "%=" | "+=" | "-=" | "<<=" | ">>=" | "&=" | "^=" | "|="
    )
}

fn assignment_op_from(op: &str) -> AssignmentOp {
    match op {
        "=" => AssignmentOp::Assign,
        "*=" => AssignmentOp::MulAssign,
        "/=" => AssignmentOp::DivAssign,
        "%=" => AssignmentOp::RemAssign,
        "+=" => AssignmentOp::AddAssign,
        "-=" => AssignmentOp::SubAssign,
        "<<=" => AssignmentOp::ShlAssign,
        ">>=" => AssignmentOp::ShrAssign,
        "&=" => AssignmentOp::BitAndAssign,
        "^=" => AssignmentOp::BitXorAssign,
        "|=" => AssignmentOp::BitOrAssign,
        _ => AssignmentOp::Assign,
    }
}

/// Parse an expanded expression string into an AST.
///
/// `expression` is the diagnostic source (`$((` / `))` stripped); `body` is
/// the text to parse (usually the same). Empty input is a syntax error.
pub(crate) fn parse_arithmetic(
    expression: &str,
    body: &str,
) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    let tokens = tokenize(expression, body)?;
    if tokens.is_empty() {
        return Err(ArithmeticExpansionError {
            expression: expression.to_string(),
            message: "empty arithmetic expression".to_string(),
        });
    }
    let mut cursor = Cursor {
        tokens: &tokens,
        pos: 0,
        expression: expression.to_string(),
    };
    let expr = parse_assignment(&mut cursor)?;
    if let Some(extra) = cursor.peek() {
        return Err(cursor.error(format!("unexpected trailing {}", describe(Some(extra)))));
    }
    Ok(expr)
}

fn parse_assignment(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    // Right-associative: `a = b = 3` nests to the right. Only a bare
    // identifier is a valid target; anything else with an assignment
    // operator is an invalid-lvalue error, not a silent misparse.
    if let Some(Token::Ident(name)) = cursor.peek().cloned()
        && let Some(Token::Op(op)) = cursor.tokens.get(cursor.pos + 1).cloned()
        && is_assignment_op(&op)
    {
        let target = name.clone();
        cursor.next();
        cursor.next();
        let rhs = parse_assignment(cursor)?;
        return Ok(ArithmeticExpr::Assignment {
            target,
            op: assignment_op_from(&op),
            rhs: Box::new(rhs),
        });
    }
    // The valid bare-identifier case already returned above, so any
    // assignment operator following a successfully parsed conditional
    // (e.g. `1 = 2`, `(A) = 3`) is an invalid target. Single parse, no
    // speculative re-parsing.
    let lhs = parse_conditional(cursor)?;
    if let Some(Token::Op(op)) = cursor.peek().cloned()
        && is_assignment_op(&op)
    {
        return Err(cursor.error("invalid assignment target".to_string()));
    }
    Ok(lhs)
}

fn parse_conditional(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    let condition = parse_logical_or(cursor)?;
    if matches!(cursor.peek(), Some(Token::Question)) {
        cursor.next();
        let then_expr = parse_assignment(cursor)?;
        match cursor.next() {
            Some(Token::Colon) => {}
            other => {
                return Err(
                    cursor.error(format!("expected ':', found {}", describe(other.as_ref())))
                );
            }
        }
        let else_expr = parse_assignment(cursor)?;
        return Ok(ArithmeticExpr::Conditional {
            condition: Box::new(condition),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
        });
    }
    Ok(condition)
}

fn parse_logical_or(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    let mut lhs = parse_logical_and(cursor)?;
    while matches!(cursor.peek(), Some(Token::Op(op)) if op == "||") {
        cursor.next();
        let rhs = parse_logical_and(cursor)?;
        lhs = ArithmeticExpr::Binary {
            op: BinaryOp::LogOr,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }
    Ok(lhs)
}

fn parse_logical_and(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    let mut lhs = parse_bit_or(cursor)?;
    while matches!(cursor.peek(), Some(Token::Op(op)) if op == "&&") {
        cursor.next();
        let rhs = parse_bit_or(cursor)?;
        lhs = ArithmeticExpr::Binary {
            op: BinaryOp::LogAnd,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }
    Ok(lhs)
}

fn parse_bit_or(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    let mut lhs = parse_bit_xor(cursor)?;
    while matches!(cursor.peek(), Some(Token::Op(op)) if op == "|") {
        cursor.next();
        let rhs = parse_bit_xor(cursor)?;
        lhs = ArithmeticExpr::Binary {
            op: BinaryOp::BitOr,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }
    Ok(lhs)
}

fn parse_bit_xor(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    let mut lhs = parse_bit_and(cursor)?;
    while matches!(cursor.peek(), Some(Token::Op(op)) if op == "^") {
        cursor.next();
        let rhs = parse_bit_and(cursor)?;
        lhs = ArithmeticExpr::Binary {
            op: BinaryOp::BitXor,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }
    Ok(lhs)
}

fn parse_bit_and(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    let mut lhs = parse_equality(cursor)?;
    while matches!(cursor.peek(), Some(Token::Op(op)) if op == "&") {
        cursor.next();
        let rhs = parse_equality(cursor)?;
        lhs = ArithmeticExpr::Binary {
            op: BinaryOp::BitAnd,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }
    Ok(lhs)
}

fn parse_equality(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    let mut lhs = parse_relational(cursor)?;
    loop {
        let op = match cursor.peek() {
            Some(Token::Op(op)) if op == "==" => BinaryOp::Eq,
            Some(Token::Op(op)) if op == "!=" => BinaryOp::Ne,
            _ => break,
        };
        cursor.next();
        let rhs = parse_relational(cursor)?;
        lhs = ArithmeticExpr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }
    Ok(lhs)
}

fn parse_relational(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    let mut lhs = parse_shift(cursor)?;
    loop {
        let op = match cursor.peek() {
            Some(Token::Op(op)) if op == "<" => BinaryOp::Lt,
            Some(Token::Op(op)) if op == "<=" => BinaryOp::Le,
            Some(Token::Op(op)) if op == ">" => BinaryOp::Gt,
            Some(Token::Op(op)) if op == ">=" => BinaryOp::Ge,
            _ => break,
        };
        cursor.next();
        let rhs = parse_shift(cursor)?;
        lhs = ArithmeticExpr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }
    Ok(lhs)
}

fn parse_shift(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    let mut lhs = parse_additive(cursor)?;
    loop {
        let op = match cursor.peek() {
            Some(Token::Op(op)) if op == "<<" => BinaryOp::Shl,
            Some(Token::Op(op)) if op == ">>" => BinaryOp::Shr,
            _ => break,
        };
        cursor.next();
        let rhs = parse_additive(cursor)?;
        lhs = ArithmeticExpr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }
    Ok(lhs)
}

fn parse_additive(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    let mut lhs = parse_multiplicative(cursor)?;
    loop {
        let op = match cursor.peek() {
            Some(Token::Op(op)) if op == "+" => BinaryOp::Add,
            Some(Token::Op(op)) if op == "-" => BinaryOp::Sub,
            _ => break,
        };
        cursor.next();
        let rhs = parse_multiplicative(cursor)?;
        lhs = ArithmeticExpr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }
    Ok(lhs)
}

fn parse_multiplicative(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    let mut lhs = parse_unary(cursor)?;
    loop {
        let op = match cursor.peek() {
            Some(Token::Op(op)) if op == "*" => BinaryOp::Mul,
            Some(Token::Op(op)) if op == "/" => BinaryOp::Div,
            Some(Token::Op(op)) if op == "%" => BinaryOp::Rem,
            _ => break,
        };
        cursor.next();
        let rhs = parse_unary(cursor)?;
        lhs = ArithmeticExpr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }
    Ok(lhs)
}

fn parse_unary(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    match cursor.peek().cloned() {
        Some(Token::Op(op)) if op == "+" => {
            cursor.next();
            Ok(ArithmeticExpr::Unary {
                op: UnaryOp::Plus,
                expr: Box::new(parse_unary(cursor)?),
            })
        }
        Some(Token::Op(op)) if op == "-" => {
            cursor.next();
            Ok(ArithmeticExpr::Unary {
                op: UnaryOp::Minus,
                expr: Box::new(parse_unary(cursor)?),
            })
        }
        Some(Token::Op(op)) if op == "!" => {
            cursor.next();
            Ok(ArithmeticExpr::Unary {
                op: UnaryOp::Not,
                expr: Box::new(parse_unary(cursor)?),
            })
        }
        Some(Token::Op(op)) if op == "~" => {
            cursor.next();
            Ok(ArithmeticExpr::Unary {
                op: UnaryOp::BitNot,
                expr: Box::new(parse_unary(cursor)?),
            })
        }
        _ => parse_primary(cursor),
    }
}

fn parse_primary(cursor: &mut Cursor) -> Result<ArithmeticExpr, ArithmeticExpansionError> {
    match cursor.next() {
        Some(Token::Integer(text)) => {
            let value = parse_integer_literal(&text).map_err(|msg| ArithmeticExpansionError {
                expression: cursor.expression.clone(),
                message: msg,
            })?;
            Ok(ArithmeticExpr::Integer(value))
        }
        Some(Token::Ident(name)) => Ok(ArithmeticExpr::Variable(name)),
        Some(Token::LParen) => {
            let inner = parse_assignment(cursor)?;
            match cursor.next() {
                Some(Token::RParen) => Ok(inner),
                other => {
                    Err(cursor.error(format!("expected ')', found {}", describe(other.as_ref()))))
                }
            }
        }
        other => Err(cursor.error(format!(
            "expected a value, found {}",
            describe(other.as_ref())
        ))),
    }
}

/// Parse `123` / `077` / `0x10` into `i64`.
///
/// Leading `0` selects octal (digits `8`/`9` are an error); `0x`/`0X`
/// selects hexadecimal. Overflow is an error, never wrapping.
pub(crate) fn parse_integer_literal(text: &str) -> Result<i64, String> {
    if text.len() > 2 && (text.starts_with("0x") || text.starts_with("0X")) {
        let digits = &text[2..];
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("invalid hexadecimal literal '{text}'"));
        }
        return i64::from_str_radix(digits, 16)
            .map_err(|_| format!("integer literal '{text}' out of range"));
    }
    if text.len() > 1 && text.starts_with('0') {
        if !text.chars().all(|c| matches!(c, '0'..='7')) {
            return Err(format!("invalid octal literal '{text}'"));
        }
        return i64::from_str_radix(text, 8)
            .map_err(|_| format!("integer literal '{text}' out of range"));
    }
    text.parse::<i64>()
        .map_err(|_| format!("integer literal '{text}' out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(body: &str) -> ArithmeticExpr {
        parse_arithmetic(body, body).expect("parse")
    }

    fn parse_err(body: &str) -> String {
        parse_arithmetic(body, body).expect_err("must fail").message
    }

    #[test]
    fn literal_bases() {
        assert_eq!(parse_ok("0"), ArithmeticExpr::Integer(0));
        assert_eq!(parse_ok("1"), ArithmeticExpr::Integer(1));
        assert_eq!(parse_ok("123"), ArithmeticExpr::Integer(123));
        assert_eq!(parse_ok("010"), ArithmeticExpr::Integer(8));
        assert_eq!(parse_ok("077"), ArithmeticExpr::Integer(63));
        assert_eq!(parse_ok("0x10"), ArithmeticExpr::Integer(16));
        assert_eq!(parse_ok("0Xff"), ArithmeticExpr::Integer(255));
    }

    #[test]
    fn precedence_and_parens() {
        // 1 + 2 * 3 == 7, not 9.
        let expr = parse_ok("1 + 2 * 3");
        match expr {
            ArithmeticExpr::Binary {
                op: BinaryOp::Add, ..
            } => {}
            other => panic!("expected additive root, got {other:?}"),
        }
        // (1 + 2) * 3 root is Mul.
        let expr = parse_ok("(1 + 2) * 3");
        match expr {
            ArithmeticExpr::Binary {
                op: BinaryOp::Mul, ..
            } => {}
            other => panic!("expected multiplicative root, got {other:?}"),
        }
        // Shift binds looser than addition: 1 << 2 + 1 == 1 << 3.
        let expr = parse_ok("1 << 2 + 1");
        match expr {
            ArithmeticExpr::Binary {
                op: BinaryOp::Shl,
                rhs,
                ..
            } => match *rhs {
                ArithmeticExpr::Binary {
                    op: BinaryOp::Add, ..
                } => {}
                other => panic!("expected additive rhs, got {other:?}"),
            },
            other => panic!("expected shift root, got {other:?}"),
        }
    }

    #[test]
    fn unary_shapes() {
        assert!(matches!(
            parse_ok("-1"),
            ArithmeticExpr::Unary {
                op: UnaryOp::Minus,
                ..
            }
        ));
        assert!(matches!(
            parse_ok("+1"),
            ArithmeticExpr::Unary {
                op: UnaryOp::Plus,
                ..
            }
        ));
        assert!(matches!(
            parse_ok("!0"),
            ArithmeticExpr::Unary {
                op: UnaryOp::Not,
                ..
            }
        ));
        assert!(matches!(
            parse_ok("~0"),
            ArithmeticExpr::Unary {
                op: UnaryOp::BitNot,
                ..
            }
        ));
    }

    #[test]
    fn comparison_and_bitwise_shapes() {
        for (body, op) in [
            ("1 < 2", BinaryOp::Lt),
            ("2 <= 2", BinaryOp::Le),
            ("3 > 2", BinaryOp::Gt),
            ("3 >= 3", BinaryOp::Ge),
            ("2 == 2", BinaryOp::Eq),
            ("2 != 3", BinaryOp::Ne),
            ("1 & 2", BinaryOp::BitAnd),
            ("1 ^ 2", BinaryOp::BitXor),
            ("1 | 2", BinaryOp::BitOr),
            ("1 << 2", BinaryOp::Shl),
            ("4 >> 1", BinaryOp::Shr),
            ("1 && 0", BinaryOp::LogAnd),
            ("1 || 0", BinaryOp::LogOr),
        ] {
            let expr = parse_ok(body);
            match expr {
                ArithmeticExpr::Binary { op: found, .. } => assert_eq!(found, op, "for {body}"),
                other => panic!("for {body}: expected binary, got {other:?}"),
            }
        }
    }

    #[test]
    fn conditional_and_assignment_shapes() {
        assert!(matches!(
            parse_ok("1 ? 2 : 3"),
            ArithmeticExpr::Conditional { .. }
        ));
        assert!(matches!(
            parse_ok("X = 3"),
            ArithmeticExpr::Assignment { .. }
        ));
        assert!(matches!(
            parse_ok("X += 2"),
            ArithmeticExpr::Assignment {
                op: AssignmentOp::AddAssign,
                ..
            }
        ));
        assert!(matches!(
            parse_ok("X *= 4"),
            ArithmeticExpr::Assignment {
                op: AssignmentOp::MulAssign,
                ..
            }
        ));
        // Right association: A = (B = 3).
        match parse_ok("A = B = 3") {
            ArithmeticExpr::Assignment { target, rhs, .. } => {
                assert_eq!(target, "A");
                assert!(matches!(*rhs, ArithmeticExpr::Assignment { .. }));
            }
            other => panic!("expected nested assignment, got {other:?}"),
        }
    }

    #[test]
    fn invalid_inputs_are_typed_errors() {
        for body in ["1 +", "(1 + 2", "1 = 2", "X << -1"] {
            // `X << -1` parses (shift of a unary) but fails at evaluation;
            // the rest must fail here.
            if *body == *"X << -1" {
                let _ = parse_ok(body);
                continue;
            }
            let msg = parse_err(body);
            assert!(!msg.is_empty(), "for {body:?}");
        }
        assert!(!parse_err("1 +").is_empty());
        assert!(!parse_err("(1 + 2").is_empty());
        assert!(!parse_err("1 = 2").is_empty());
        // Comma and increments never parse.
        assert!(!parse_err("1, 2").is_empty());
        assert!(!parse_err("++X").is_empty());
        assert!(!parse_err("X++").is_empty());
    }
}
