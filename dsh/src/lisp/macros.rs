#[allow(unused_macros)]
#[macro_export]
macro_rules! lisp {


    // Embed a Rust expression with { }
    ( { $e:expr } ) => {
        $e
    };


    // Lists
    ( ( $($val:tt)* ) ) => {
        $crate::lisp::model::Value::List([ $(lisp!{ $val }),* ].iter().collect::<$crate::lisp::model::List>())
    };


    // 🦀 Very special!
    // Special atoms
    (nil) => { $crate::model::Value::NIL   };
    (NIL) => { $crate::model::Value::NIL   };
    (t) =>   { $crate::model::Value::True  };
    (T) =>   { $crate::model::Value::True  };
    (f) =>   { $crate::model::Value::False };
    (F) =>   { $crate::model::Value::False };


    // Symbols
    ($sym:ident) => {
        $crate::lisp::model::Value::Symbol($crate::lisp::model::Symbol(String::from(stringify!( $sym ))))
    };
    // these aren't valid Rust identifiers
    ( + ) =>  { $crate::model::Value::Symbol($crate::model::Symbol(String::from("+"))) };
    ( - ) =>  { $crate::model::Value::Symbol($crate::model::Symbol(String::from("-"))) };
    ( * ) =>  { $crate::model::Value::Symbol($crate::model::Symbol(String::from("*"))) };
    ( / ) =>  { $crate::model::Value::Symbol($crate::model::Symbol(String::from("/"))) };
    ( == ) => { $crate::model::Value::Symbol($crate::model::Symbol(String::from("=="))) };
    ( != ) => { $crate::model::Value::Symbol($crate::model::Symbol(String::from("!="))) };
    ( < ) =>  { $crate::model::Value::Symbol($crate::model::Symbol(String::from("<"))) };
    ( <= ) => { $crate::model::Value::Symbol($crate::model::Symbol(String::from("<="))) };
    ( > ) =>  { $crate::model::Value::Symbol($crate::model::Symbol(String::from(">"))) };
    ( >= ) => { $crate::model::Value::Symbol($crate::model::Symbol(String::from(">="))) };


    // Literals
    ($e:literal) => {
        // HACK: Macros don't have a good way to
        // distinguish different kinds of literals,
        // so we just kick those out to be parsed
        // at runtime.
        $crate::parser::parse(stringify!($e)).next().unwrap().unwrap()
    };
}
