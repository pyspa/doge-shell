use crate::config::Config;
use anyhow::Result;
use hashbrown::HashMap;
use log::debug;
use std::fs;
use wasmtime::{Engine, Linker, Module, Store};
use wasmtime_wasi::sync::WasiCtxBuilder;

pub struct WASMEngine {
    engine: Engine,
    pub modules: HashMap<String, Module>,
}

impl std::fmt::Debug for WASMEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("WASMEngine").finish()
    }
}

impl WASMEngine {
    pub fn new(wasm_dir: &Option<String>) -> Self {
        let engine = Engine::default();
        let mut modules: HashMap<String, Module> = HashMap::new();

        if let Some(wasm_dir) = wasm_dir {
            if let Ok(entries) = fs::read_dir(wasm_dir) {
                let entries: Vec<fs::DirEntry> = entries.flatten().collect();
                for entry in entries {
                    if let Ok(path) = entry.path().canonicalize() {
                        if let Some(file) = path.file_stem() {
                            if let Ok(module) = Module::from_file(&engine, &path) {
                                debug!("load wasm {:?} {:?}", &file, &path);
                                modules.insert(file.to_string_lossy().to_string(), module);
                            }
                        }
                    }
                }
            }
        }
        WASMEngine { engine, modules }
    }

    pub fn call(&self, name: &str, args: &[String]) -> Result<()> {
        if let Some(module) = self.modules.get(name) {
            // new linker
            let mut linker = Linker::new(&self.engine);
            wasmtime_wasi::add_to_linker(&mut linker, |s| s)?;

            // TODO use ctx
            let wasi = WasiCtxBuilder::new().inherit_stdio().args(args)?.build();
            let mut store = Store::new(&self.engine, wasi);

            linker.module(&mut store, "", module)?;
            linker
                .get_default(&mut store, "")?
                .typed::<(), (), _>(&store)?
                .call(&mut store, ())?;
        } else {
            eprint!("\runknown wasm command: {}\r\n", name);
        }
        Ok(())
    }
}
