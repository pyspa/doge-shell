//! Arithmetic expression tokenizer.
//!
//! Splits an expanded `$((...))` body into integer, identifier, operator,
//! and punctuation tokens. `++` / `--` / `,` are rejected here so they can
//! never silently mean `+ +x`.

use super::eval::ArithmeticExpansionError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    Integer(String),
    Ident(String),
    Op(String),
    LParen,
    RParen,
    Question,
    Colon,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Split an expanded expression into tokens.
///
/// `expression` is the diagnostic source for errors; `body` is the text to
/// split (usually the expanded arithmetic string).
pub(crate) fn tokenize(
    expression: &str,
    body: &str,
) -> Result<Vec<Token>, ArithmeticExpansionError> {
    let fail = |msg: &str| ArithmeticExpansionError {
        expression: expression.to_string(),
        message: msg.to_string(),
    };
    let mut tokens = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if is_ident_start(c) {
            let start = i;
            while i < chars.len() && is_ident_char(chars[i]) {
                i += 1;
            }
            tokens.push(Token::Ident(chars[start..i].iter().collect()));
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            if c == '0' && i + 1 < chars.len() && (chars[i + 1] == 'x' || chars[i + 1] == 'X') {
                i += 2;
                let hex_start = i;
                while i < chars.len() && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
                if hex_start == i {
                    return Err(fail("invalid hexadecimal literal"));
                }
                tokens.push(Token::Integer(chars[start..i].iter().collect()));
            } else {
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i < chars.len() && is_ident_start(chars[i]) {
                    return Err(fail("invalid numeric literal"));
                }
                tokens.push(Token::Integer(chars[start..i].iter().collect()));
            }
            continue;
        }
        match c {
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '?' => {
                tokens.push(Token::Question);
                i += 1;
            }
            ':' => {
                tokens.push(Token::Colon);
                i += 1;
            }
            '+' => {
                if i + 1 < chars.len() && chars[i + 1] == '+' {
                    return Err(fail("unsupported operator '++'"));
                }
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Op("+=".to_string()));
                    i += 2;
                } else {
                    tokens.push(Token::Op("+".to_string()));
                    i += 1;
                }
            }
            '-' => {
                if i + 1 < chars.len() && chars[i + 1] == '-' {
                    return Err(fail("unsupported operator '--'"));
                }
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Op("-=".to_string()));
                    i += 2;
                } else {
                    tokens.push(Token::Op("-".to_string()));
                    i += 1;
                }
            }
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Op("*=".to_string()));
                    i += 2;
                } else {
                    tokens.push(Token::Op("*".to_string()));
                    i += 1;
                }
            }
            '/' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Op("/=".to_string()));
                    i += 2;
                } else {
                    tokens.push(Token::Op("/".to_string()));
                    i += 1;
                }
            }
            '%' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Op("%=".to_string()));
                    i += 2;
                } else {
                    tokens.push(Token::Op("%".to_string()));
                    i += 1;
                }
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '<' {
                    if i + 2 < chars.len() && chars[i + 2] == '=' {
                        tokens.push(Token::Op("<<=".to_string()));
                        i += 3;
                    } else {
                        tokens.push(Token::Op("<<".to_string()));
                        i += 2;
                    }
                } else if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Op("<=".to_string()));
                    i += 2;
                } else {
                    tokens.push(Token::Op("<".to_string()));
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '>' {
                    if i + 2 < chars.len() && chars[i + 2] == '=' {
                        tokens.push(Token::Op(">>=".to_string()));
                        i += 3;
                    } else {
                        tokens.push(Token::Op(">>".to_string()));
                        i += 2;
                    }
                } else if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Op(">=".to_string()));
                    i += 2;
                } else {
                    tokens.push(Token::Op(">".to_string()));
                    i += 1;
                }
            }
            '=' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Op("==".to_string()));
                    i += 2;
                } else {
                    tokens.push(Token::Op("=".to_string()));
                    i += 1;
                }
            }
            '!' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Op("!=".to_string()));
                    i += 2;
                } else {
                    tokens.push(Token::Op("!".to_string()));
                    i += 1;
                }
            }
            '&' => {
                if i + 1 < chars.len() && chars[i + 1] == '&' {
                    tokens.push(Token::Op("&&".to_string()));
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Op("&=".to_string()));
                    i += 2;
                } else {
                    tokens.push(Token::Op("&".to_string()));
                    i += 1;
                }
            }
            '|' => {
                if i + 1 < chars.len() && chars[i + 1] == '|' {
                    tokens.push(Token::Op("||".to_string()));
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Op("|=".to_string()));
                    i += 2;
                } else {
                    tokens.push(Token::Op("|".to_string()));
                    i += 1;
                }
            }
            '^' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Op("^=".to_string()));
                    i += 2;
                } else {
                    tokens.push(Token::Op("^".to_string()));
                    i += 1;
                }
            }
            '~' => {
                tokens.push(Token::Op("~".to_string()));
                i += 1;
            }
            ',' => {
                return Err(fail("unsupported operator ','"));
            }
            ';' => {
                return Err(fail("unexpected ';'"));
            }
            '`' | '\'' | '"' | '$' | '\\' | '{' | '}' | '[' | ']' => {
                return Err(fail(&format!("unexpected character '{c}'")));
            }
            _ => {
                return Err(fail(&format!("unexpected character '{c}'")));
            }
        }
    }
    Ok(tokens)
}
