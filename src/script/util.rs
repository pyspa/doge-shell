use rust_lisp::model::{List, Value};
use std::iter::FromIterator;

pub fn make_list(vec: Vec<String>) -> Value {
    let lst = vec.iter().map(|x| Value::String(x.to_string()));
    Value::List(List::from_iter(lst))
}

pub fn unquote(s: &str) -> String {
    let quote = s.chars().next().unwrap();

    if quote != '"' && quote != '\'' && quote != '`' {
        return s.to_string();
    }
    let s = &s[1..s.len() - 1];
    s.to_string()
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn test_make_list() {
        let args = make_list(vec!["aaa".to_string(), "bbb".to_string()]);
        println!("list {:?}", args);
    }

    #[test]
    fn test_unquote() {
        let test = unquote("\" test \"");
        println!("{}", test);
    }
}
