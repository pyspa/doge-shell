use crate::{
    lisp,
    lisp::model::{FloatType, IntType, List, Symbol, Value},
};

use std::fmt::Display;

/// Parse a string of Lisp code into a series of s-expressions. There
/// are more than one expressions when the base string has more than one
/// independent parenthesized lists at its root.
pub fn parse(code: &str) -> impl Iterator<Item = Result<Value, ParseError>> + '_ {
    let mut index = 0;
    index = consume_whitespace_and_comments(code, index);

    std::iter::from_fn(move || {
        if let Some(res) = parse_expression(code, index) {
            if let Ok(res) = res {
                index = res.index;
                index = consume_whitespace_and_comments(code, index);

                Some(Ok(res.parsed.into_value()))
            } else {
                Some(Err(res.unwrap_err()))
            }
        } else if index < code.len() {
            let err_index = index;
            index = code.len(); // Stop iteration
            Some(Err(ParseError {
                msg: format!("Unexpected token at index {}", err_index),
            }))
        } else {
            None
        }
    })
}

/// A slightly more convenient data structure for building the parse tree, before
/// eventually converting it into proper s-expressions.
#[derive(Debug, Clone)]
enum ParseTree {
    Atom(Value),
    List(Vec<ParseTree>),
    Quoted(Box<ParseTree>),
    Comma(Box<ParseTree>),
}

impl ParseTree {
    pub fn into_value(self) -> Value {
        match self {
            ParseTree::Atom(value) => value,
            ParseTree::List(vec) => Value::List(
                vec.into_iter()
                    .map(|parse_tree| parse_tree.into_value())
                    .collect::<List>(),
            ),
            ParseTree::Quoted(inner) => lisp! { (quote {inner.into_value()}) },
            ParseTree::Comma(inner) => lisp! { (comma {inner.into_value()}) },
        }
    }
}

/**
 * An error that occurred while parsing a string as lisp code
 */
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub msg: String,
    // pub line: i32,
}

impl Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "Parse error: {}", self.msg)
    }
}

#[derive(Clone, Debug)]
struct ParsedAndIndex {
    pub parsed: ParseTree,
    pub index: usize,
}

type ParseResult = Option<Result<ParsedAndIndex, ParseError>>;
type ConsumeResult = Option<usize>;

fn parse_expression(code: &str, index: usize) -> ParseResult {
    for func in [parse_list, parse_atom] {
        let res = func(code, index);

        if res.is_some() {
            return res;
        }
    }

    None
}

fn parse_list(code: &str, index: usize) -> ParseResult {
    let mut index = consume(code, index, "(")?;
    let mut members = vec![];

    index = consume_whitespace_and_comments(code, index);

    while let Some(res) = parse_expression(code, index) {
        if let Ok(res) = res {
            index = res.index;
            members.push(res.parsed);
            index = consume_whitespace_and_comments(code, index);
        } else {
            return Some(res);
        }
    }

    if let Some(index) = consume(code, index, ")") {
        Some(Ok(ParsedAndIndex {
            parsed: ParseTree::List(members),
            index,
        }))
    } else {
        Some(Err(ParseError {
            msg: format!("Unclosed list at index {index}"),
        }))
    }
}

fn parse_atom(code: &str, index: usize) -> ParseResult {
    for func in [
        parse_quoted,
        parse_comma,
        parse_nil,
        parse_false,
        parse_true,
        parse_number,
        parse_string,
        parse_symbol,
    ] {
        let res = func(code, index);

        if res.is_some() {
            return res;
        }
    }

    None
}

fn parse_quoted(code: &str, index: usize) -> ParseResult {
    let index = consume(code, index, "'")?;
    let res = parse_expression(code, index)?;

    if let Ok(ParsedAndIndex { parsed, index }) = res {
        Some(Ok(ParsedAndIndex {
            parsed: ParseTree::Quoted(Box::new(parsed)),
            index,
        }))
    } else {
        Some(res)
    }
}

fn parse_comma(code: &str, index: usize) -> ParseResult {
    let index = consume(code, index, ",")?;
    let res = parse_expression(code, index)?;

    if let Ok(ParsedAndIndex { parsed, index }) = res {
        Some(Ok(ParsedAndIndex {
            parsed: ParseTree::Comma(Box::new(parsed)),
            index,
        }))
    } else {
        Some(res)
    }
}

fn parse_nil(code: &str, index: usize) -> ParseResult {
    let index = consume(code, index, "nil")?;

    if next_char_is_break(code, index) {
        Some(Ok(ParsedAndIndex {
            parsed: ParseTree::Atom(Value::NIL),
            index,
        }))
    } else {
        None
    }
}

fn parse_false(code: &str, index: usize) -> ParseResult {
    let index = consume(code, index, "f")?;

    if next_char_is_break(code, index) {
        Some(Ok(ParsedAndIndex {
            parsed: ParseTree::Atom(Value::False),
            index,
        }))
    } else {
        None
    }
}

fn parse_true(code: &str, index: usize) -> ParseResult {
    let index = consume(code, index, "t")?;

    if next_char_is_break(code, index) {
        Some(Ok(ParsedAndIndex {
            parsed: ParseTree::Atom(Value::True),
            index,
        }))
    } else {
        None
    }
}

fn parse_number(code: &str, index: usize) -> ParseResult {
    let (front_end, front_last_char) = consume_while(code, index, |(index, ch)| {
        (index == 0 && ch == '-') || ch.is_numeric()
    })?;

    if front_last_char.is_numeric() {
        let back = consume_while(code, front_end, |(index, ch)| {
            (index == 0 && ch == '.') || ch.is_numeric()
        });

        if let Some((back_end, _)) = back {
            if back_end >= front_end + 2 {
                if next_char_is_break(code, back_end)
                    && let Ok(float) = code.get(index..back_end).unwrap_or("").parse::<FloatType>()
                {
                    return Some(Ok(ParsedAndIndex {
                        parsed: ParseTree::Atom(Value::Float(float)),
                        index: back_end,
                    }));
                }
            } else if code.as_bytes().get(back_end - 1) == Some(&b'.') {
                return Some(Err(ParseError {
                    msg: format!("Expected decimal value after '.' at index {}", back_end - 1),
                }));
            }
        }

        if next_char_is_break(code, front_end)
            && let Ok(int) = code.get(index..front_end).unwrap_or("").parse::<IntType>()
        {
            return Some(Ok(ParsedAndIndex {
                parsed: ParseTree::Atom(Value::Int(int)),
                index: front_end,
            }));
        }
    }

    None
}

fn parse_string(code: &str, index: usize) -> ParseResult {
    // `end` is the byte just past the last character inside the quotes.
    let (end, _) = consume_while(code, index, |(index, ch)| {
        (index == 0 && ch == '"') || (index > 0 && ch != '"')
    })?;

    if end > index + 1 {
        if code.as_bytes().get(end) == Some(&b'"') {
            Some(Ok(ParsedAndIndex {
                parsed: ParseTree::Atom(Value::String(
                    code.get(index + 1..end).unwrap_or("").to_owned(),
                )),
                index: end + 1,
            }))
        } else {
            Some(Err(ParseError {
                msg: format!("Unclosed string at index {end}"),
            }))
        }
    } else {
        None
    }
}

fn parse_symbol(code: &str, index: usize) -> ParseResult {
    let (last_index, _) = consume_while(code, index, |(index, ch)| {
        (index == 0 && is_symbol_start(ch)) || (index > 0 && is_symbolic(ch))
    })?;

    if last_index > index {
        Some(Ok(ParsedAndIndex {
            parsed: ParseTree::Atom(Value::Symbol(Symbol(
                code.get(index..last_index).unwrap_or("").to_owned(),
            ))),
            index: last_index,
        }))
    } else {
        None
    }
}

fn consume(code: &str, index: usize, s: &str) -> ConsumeResult {
    let slice = code.get(index..).unwrap_or("");

    if slice.len() >= s.len()
        && slice
            .chars()
            .zip(s.chars())
            .all(|(a, b)| a.eq_ignore_ascii_case(&b))
    {
        Some(index + s.len())
    } else {
        None
    }
}

fn consume_whitespace_and_comments(code: &str, index: usize) -> usize {
    let mut semicolons = 0;

    consume_while(code, index, move |(_, ch)| {
        if ch == ';' {
            semicolons += 1;
        } else if semicolons < 2 || ch == '\n' {
            semicolons = 0;
        }

        ch.is_whitespace() || ch == ';' || semicolons >= 2
    })
    .map(|(end, _)| end)
    .unwrap_or(index)
}

/// Consumes characters from `index` while `pred` holds and returns the byte
/// offset just past the last consumed character, together with that
/// character. The offset is a char boundary even when the last character is
/// multibyte, so callers can slice `code` with it directly.
fn consume_while<F: FnMut((usize, char)) -> bool>(
    code: &str,
    index: usize,
    mut pred: F,
) -> Option<(usize, char)> {
    code.get(index..)
        .unwrap_or("")
        .char_indices()
        .take_while(|(i, c)| pred((*i, *c)))
        .last()
        .map(|(last_index, ch)| (last_index + index + ch.len_utf8(), ch))
}

fn is_symbol_start(c: char) -> bool {
    !c.is_numeric() && is_symbolic(c)
}

fn is_symbolic(c: char) -> bool {
    !c.is_whitespace() && !SPECIAL_TOKENS.contains(&c)
}

fn next_char_is_break(code: &str, index: usize) -> bool {
    code.get(index..)
        .and_then(|s| s.chars().next())
        .map(|ch| ch.is_whitespace() || SPECIAL_TOKENS.contains(&ch))
        .unwrap_or(true)
}

const SPECIAL_TOKENS: [char; 5] = ['(', ')', '\'', ',', ';'];
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_unexpected_token() {
        let code = "(+ 1 2) )";
        let mut parser = parse(code);

        // First expression OK
        let first = parser.next();
        assert!(first.is_some());
        assert!(first.unwrap().is_ok());

        // Second should be Error
        let second = parser.next();
        assert!(second.is_some());
        let res = second.unwrap();
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().msg, "Unexpected token at index 8");
    }

    fn parse_all(code: &str) -> Vec<Value> {
        parse(code)
            .map(|r| r.unwrap_or_else(|e| panic!("parse error for {code:?}: {}", e.msg)))
            .collect()
    }

    #[test]
    fn parses_strings_ending_in_multibyte_characters() {
        assert_eq!(parse_all("\"あ\""), vec![Value::String("あ".into())]);
        assert_eq!(parse_all("\"aあ\""), vec![Value::String("aあ".into())]);
        assert_eq!(
            parse_all("\"あb\" 1"),
            vec![Value::String("あb".into()), Value::Int(1)]
        );
    }

    #[test]
    fn parses_multibyte_symbols_and_separators() {
        assert_eq!(
            parse_all("名前"),
            vec![Value::Symbol(Symbol("名前".into()))]
        );
        // U+3000 IDEOGRAPHIC SPACE is whitespace and separates list items.
        let parsed = parse_all("(a\u{3000}b)");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].to_string(), "(a b)");
    }

    #[test]
    fn trailing_multibyte_comment_without_newline_is_ignored() {
        assert_eq!(parse_all("1 ;; 設定"), vec![Value::Int(1)]);
        assert_eq!(parse_all("1.5 ;; 設定"), vec![Value::Float(1.5)]);
    }
}
