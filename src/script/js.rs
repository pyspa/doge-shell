use anyhow::Result;
use deno_core::error::AnyError;
use deno_core::{op, Extension, JsRuntime, OpState, RuntimeOptions};

#[derive(Debug, Clone)]
pub struct Context {
    pub buf: String,
}

impl Context {
    pub fn new() -> Self {
        Context { buf: String::new() }
    }
}

#[op]
pub fn op_print(state: &mut OpState, msg: String, _is_err: bool) -> Result<(), AnyError> {
    // TODO check stdout or stderr
    let ctx: &mut Context = state.borrow_mut();
    ctx.buf.push_str(msg.as_str());
    Ok(())
}

pub fn execute_js(src: &str, fn_name: &str, add_builtin: bool) -> Result<Context> {
    let exts = Extension::builder()
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
        .build();

    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: vec![exts],
        ..Default::default()
    });

    let mut src = src.to_string();

    if add_builtin {
        src.push_str(
            r#"
function print(value) {
  Deno.core.print(value.toString()+"\n");
}
"#,
        );
    }

    // call named func
    src.push_str(format!("\n{}();", fn_name).as_str());

    let _ = runtime.execute_script("<usage>", src.as_str())?;
    let op_state = runtime.op_state();
    let state = op_state.borrow();

    let ctx: &Context = state.borrow();
    Ok(ctx.clone())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn init() {
        let _ = env_logger::try_init();
    }

    #[test]
    fn test_execute_js() {
        let res = execute_js(
            r#"
    function hello() {
      print("hello");
    }
    function hello2() {
      print("hello2");
    }
    "#,
            "hello",
            true,
        );

        assert!(res.is_ok());
        assert_eq!("hello\n", res.unwrap().buf.as_str());
    }
}
