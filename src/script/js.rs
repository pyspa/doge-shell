use crate::script::{build_extension, Context};
use anyhow::Result;
use deno_core::{JsRuntime, RuntimeOptions};

pub fn execute_js(src: &str, fn_name: &str, add_builtin: bool) -> Result<Context> {
    let ext = build_extension();
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: vec![ext],
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
        assert_eq!("hello\n", res.unwrap().stdout.as_str());
    }
}
