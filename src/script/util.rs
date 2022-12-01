use rust_lisp::model::{List, Value};
use std::iter::FromIterator;

pub fn make_list(vec: Vec<String>) -> Value {
    let lst = vec.iter().map(|x| Value::String(x.to_string()));
    Value::List(List::from_iter(lst))
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn test_make_list() {
        let args = make_list(vec!["aaa".to_string(), "bbb".to_string()]);
        println!("list {:?}", args);
    }
}
