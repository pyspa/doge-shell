use crate::script::{build_extension, Context};
use anyhow::{anyhow, bail, Error, Result};
use deno_ast::{MediaType, ParseParams, SourceTextInfo};
use deno_core::{
    resolve_import, resolve_path, JsRuntime, ModuleLoader, ModuleSource, ModuleSourceFuture,
    ModuleSpecifier, ModuleType, RuntimeOptions,
};
use futures::FutureExt;
use std::pin::Pin;
use std::rc::Rc;

struct TypescriptModuleLoader;

impl ModuleLoader for TypescriptModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _is_main: bool,
    ) -> Result<ModuleSpecifier, Error> {
        Ok(resolve_import(specifier, referrer)?)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<ModuleSpecifier>,
        _is_dyn_import: bool,
    ) -> Pin<Box<ModuleSourceFuture>> {
        let module_specifier = module_specifier.clone();
        async move {
            let path = module_specifier
                .to_file_path()
                .map_err(|_| anyhow!("Only file: URLs are supported."))?;

            let media_type = MediaType::from(&path);
            let (module_type, should_transpile) = match MediaType::from(&path) {
                MediaType::JavaScript | MediaType::Mjs | MediaType::Cjs => {
                    (ModuleType::JavaScript, false)
                }
                MediaType::Jsx => (ModuleType::JavaScript, true),
                MediaType::TypeScript
                | MediaType::Mts
                | MediaType::Cts
                | MediaType::Dts
                | MediaType::Dmts
                | MediaType::Dcts
                | MediaType::Tsx => (ModuleType::JavaScript, true),
                MediaType::Json => (ModuleType::Json, false),
                _ => bail!("Unknown extension {:?}", path.extension()),
            };

            let code = std::fs::read_to_string(&path)?;
            let code = if should_transpile {
                let parsed = deno_ast::parse_module(ParseParams {
                    specifier: module_specifier.to_string(),
                    text_info: SourceTextInfo::from_string(code),
                    media_type,
                    capture_tokens: false,
                    scope_analysis: false,
                    maybe_syntax: None,
                })?;

                parsed.transpile(&Default::default())?.text
            } else {
                code
            };
            let module = ModuleSource {
                code: code.into_bytes().into_boxed_slice(),
                module_type,
                module_url_specified: module_specifier.to_string(),
                module_url_found: module_specifier.to_string(),
            };
            Ok(module)
        }
        .boxed_local()
    }
}

pub fn execute_ts(file: &str) -> Result<Context> {
    let exts = build_extension();
    let mut runtime = JsRuntime::new(RuntimeOptions {
        module_loader: Some(Rc::new(TypescriptModuleLoader)),
        extensions: vec![exts],
        ..Default::default()
    });
    let main_module = resolve_path(file)?;

    let future = async {
        let mod_id = runtime.load_main_module(&main_module, None).await?;
        let result = runtime.mod_evaluate(mod_id);
        runtime.run_event_loop(false).await?;

        result.await?
    };
    async_std::task::block_on(future)?;

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
    fn test_execute_ts() {
        let file = "./tests/test.ts";
        let res = execute_ts(file);
        assert!(res.is_ok());
        assert_eq!("Hello Dog!", res.unwrap().stdout.as_str());
    }
}
