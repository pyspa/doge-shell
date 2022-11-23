pub mod js;
pub mod ts;
use deno_core::error::AnyError;
use deno_core::{op, Extension, OpState};

#[derive(Debug, Clone)]
pub struct Context {
    pub stdout: String,
    pub stderr: String,
}

impl Context {
    pub fn new() -> Self {
        Context {
            stdout: String::new(),
            stderr: String::new(),
        }
    }
}

#[op]
pub fn op_print(state: &mut OpState, msg: String, is_err: bool) -> Result<(), AnyError> {
    let ctx: &mut Context = state.borrow_mut();
    if is_err {
        ctx.stderr.push_str(msg.as_str());
    } else {
        ctx.stdout.push_str(msg.as_str());
    }
    Ok(())
}

pub fn build_extension() -> Extension {
    Extension::builder()
        .state(move |state| {
            let ctx = Context::new();
            state.put::<Context>(ctx);
            Ok(())
        })
        .middleware(|op| match op.name {
            "op_print" => op_print::decl(),
            _ => op,
        })
        .ops(vec![]) // custom function
        .build()
}

pub use crate::script::js::execute_js;
pub use crate::script::ts::execute_ts;
