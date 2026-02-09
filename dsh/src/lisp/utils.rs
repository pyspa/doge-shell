use crate::lisp::model::{
    FloatType, HashMapRc, IntType, List, RuntimeError, Symbol, TableRc, Value,
};
use std::collections::HashMap;
use std::{any::Any, rc::Rc};

/// Given a `Value` assumed to be a `Value::List()`, grab the item at `index`
/// and err if there isn't one.
pub fn require_arg<'a>(
    func_or_form_name: &str,
    args: &'a [Value],
    index: usize,
) -> Result<&'a Value, RuntimeError> {
    args.get(index).ok_or_else(|| RuntimeError {
        msg: format!(
            "\"{}\" requires an argument {}",
            func_or_form_name,
            index + 1
        ),
    })
}

/// Given a `Value` assumed to be a `Value::List()`, and some type T, grab the
/// item at `index` in the list and try converting it to type T. RuntimeError if
/// the argument doesn't exist, or if it is the wrong type.
pub fn require_typed_arg<'a, T>(
    func_or_form_name: &str,
    args: &'a [Value],
    index: usize,
) -> Result<T, RuntimeError>
where
    T: TryFrom<&'a Value> + TypeName,
{
    require_arg(func_or_form_name, args, index)?
        .try_into()
        .map_err(|_| RuntimeError {
            msg: format!(
                "\"{}\" requires argument {} to be a {}; got {}",
                func_or_form_name,
                index + 1,
                T::get_name(),
                args.get(index).unwrap_or(&Value::NIL)
            ),
        })
}

pub trait TypeName {
    fn get_name() -> &'static str;
}

impl TypeName for IntType {
    fn get_name() -> &'static str {
        "int"
    }
}

impl TypeName for FloatType {
    fn get_name() -> &'static str {
        "float"
    }
}

impl TypeName for &String {
    fn get_name() -> &'static str {
        "string"
    }
}

impl TypeName for &Symbol {
    fn get_name() -> &'static str {
        "symbol"
    }
}

impl TypeName for &List {
    fn get_name() -> &'static str {
        "list"
    }
}

impl TypeName for &HashMapRc {
    fn get_name() -> &'static str {
        "hash map"
    }
}

impl TypeName for &Rc<dyn Any> {
    fn get_name() -> &'static str {
        "foreign value"
    }
}

impl TypeName for &TableRc {
    fn get_name() -> &'static str {
        "table"
    }
}

#[allow(dead_code)]
pub fn make_list(vec: Vec<String>) -> Value {
    let lst = vec.iter().map(|x| Value::String(x.to_string()));
    Value::List(List::from_iter(lst))
}

#[allow(dead_code)]
pub fn unquote(s: &str) -> String {
    let quote = match s.chars().next() {
        Some(c) => c,
        None => return String::new(), // Empty string
    };

    if quote != '"' && quote != '\'' && quote != '`' {
        return s.to_string();
    }
    let s = &s[1..s.len() - 1];
    s.to_string()
}

pub fn list_of_strings(name: &str, value: &Value) -> Result<Vec<String>, RuntimeError> {
    match value {
        Value::List(list) if *list == List::NIL => Ok(Vec::new()),
        Value::List(list) => list
            .into_iter()
            .map(|item| match item {
                Value::String(s) => Ok(s),
                other => Err(RuntimeError {
                    msg: format!("\"{name}\" expects a list of strings; got element {other}"),
                }),
            })
            .collect(),
        Value::False => Ok(Vec::new()),
        other => Err(RuntimeError {
            msg: format!("\"{name}\" expects a list of strings or NIL; got {other}"),
        }),
    }
}

pub fn list_of_pairs(name: &str, value: &Value) -> Result<HashMap<String, String>, RuntimeError> {
    match value {
        Value::List(list) if *list == List::NIL => Ok(HashMap::new()),
        Value::List(list) => {
            let mut map = HashMap::new();
            for entry in list.into_iter() {
                match entry {
                    Value::List(pair) => {
                        let mut iter = pair.into_iter();
                        let key = match iter.next() {
                            Some(Value::String(s)) => s,
                            Some(other) => {
                                return Err(RuntimeError {
                                    msg: format!(
                                        "\"{name}\" expects env entries as (key value); got key {other}"
                                    ),
                                });
                            }
                            None => {
                                return Err(RuntimeError {
                                    msg: format!(
                                        "\"{name}\" expects env entries with two elements"
                                    ),
                                });
                            }
                        };
                        let value = match iter.next() {
                            Some(Value::String(s)) => s,
                            Some(other) => {
                                return Err(RuntimeError {
                                    msg: format!(
                                        "\"{name}\" expects env entries as (key value); got value {other}"
                                    ),
                                });
                            }
                            None => {
                                return Err(RuntimeError {
                                    msg: format!(
                                        "\"{name}\" expects env entries with two elements"
                                    ),
                                });
                            }
                        };
                        map.insert(key, value);
                    }
                    other => {
                        return Err(RuntimeError {
                            msg: format!("\"{name}\" expects env entries as lists; got {other}"),
                        });
                    }
                }
            }
            Ok(map)
        }
        Value::False => Ok(HashMap::new()),
        other => Err(RuntimeError {
            msg: format!("\"{name}\" expects a list of key/value pairs or NIL; got {other}"),
        }),
    }
}

pub fn optional_string(name: &str, value: &Value) -> Result<Option<String>, RuntimeError> {
    match value {
        Value::List(list) if *list == List::NIL => Ok(None),
        Value::False => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        other => Err(RuntimeError {
            msg: format!("\"{name}\" expects a string or NIL; got {other}"),
        }),
    }
}

pub fn optional_bool(name: &str, value: &Value) -> Result<Option<bool>, RuntimeError> {
    match value {
        Value::List(list) if *list == List::NIL => Ok(None),
        Value::False => Ok(Some(false)),
        Value::True => Ok(Some(true)),
        other => Err(RuntimeError {
            msg: format!("\"{name}\" expects a boolean or NIL; got {other}"),
        }),
    }
}
