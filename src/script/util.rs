use rust_lisp::model::Value;
use rust_lisp::parser::{parse, ParseError};

pub fn make_list(vec: Vec<String>) -> Result<Value, ParseError> {
    let mut buf = String::new();
    let list: Vec<String> = vec.iter().map(|x| format!("\"{}\"", x)).collect();
    let list = list.join(" ");
    buf.push('(');
    buf.push_str(list.as_str());
    buf.push(')');
    let x = parse(buf.as_str()).next().unwrap();
    x
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn test_make_list() {
        let args = make_list(vec!["aaa".to_string(), "bbb".to_string()]).ok();
        println!("list {:?}", args);
    }
}
