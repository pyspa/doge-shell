/// The underlying type used to store Lisp integers.
pub type IntType = i64;
/// The underlying type used to store Lisp floats.
pub type FloatType = f64;

mod env;
mod lambda;
mod list;
mod runtime_error;
mod symbol;
pub mod table;
pub(crate) mod value;

pub use env::Env;
pub use lambda::Lambda;
pub use list::List;
pub use runtime_error::RuntimeError;
pub use symbol::Symbol;
pub use table::{CmpValue, Record, Table, TableRc};
pub use value::{HashMapRc, Value};
