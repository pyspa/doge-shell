use crate::environment::Environment;
use crate::shell::APP_NAME;
use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::{cell::RefCell, rc::Rc};
use tracing::debug;
use wasmer::{Instance, Module, Store};
use wasmer_compiler_cranelift::Cranelift;
use wasmer_wasi::WasiState;

pub struct WasmEngine {
    environment: Rc<RefCell<Environment>>,
    pub modules: HashMap<String, Module>,
    store: Store,
}

impl std::fmt::Debug for WasmEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("WASMEngine").finish()
    }
}

impl WasmEngine {
    pub fn new(environment: Rc<RefCell<Environment>>) -> Self {
        let xdg_dir =
            xdg::BaseDirectories::with_prefix(APP_NAME).expect("failed get xdg directory");
        let wasm_dir = xdg_dir
            .place_config_file("wasm")
            .expect("failed get path")
            .to_string_lossy()
            .to_string();
        Self::from_path(environment, &wasm_dir)
    }

    pub fn from_path(environment: Rc<RefCell<Environment>>, wasm_dir: &str) -> Self {
        let store = Store::new(Cranelift::default());
        let mut modules: HashMap<String, Module> = HashMap::new();

        if let Ok(entries) = fs::read_dir(wasm_dir) {
            let entries: Vec<fs::DirEntry> = entries
                .flatten()
                .filter(|x| x.path().extension().unwrap_or_default() == "wasm") // filer .wasm
                .collect();

            for entry in entries {
                if let Ok(path) = entry.path().canonicalize() {
                    if let Some(file) = path.file_stem() {
                        let name = file.to_string_lossy().to_string();
                        if let Ok(wasm_bytes) = std::fs::read(&path) {
                            if let Ok(module) = Module::new(&store, wasm_bytes) {
                                debug!("load wasm {:?} {:?}", &file, &path);
                                modules.insert(name, module);
                            } else {
                                eprint!("\rfailed load wasm: {:?}\r\n", &file);
                            }
                        } else {
                            eprint!("\rfailed read wasm: {:?}\r\n", &file);
                        }
                    }
                }
            }
        }
        WasmEngine {
            environment,
            modules,
            store,
        }
    }

    pub fn call(&mut self, name: &str, args: &[String]) -> Result<()> {
        if let Some(module) = self.modules.get(name) {
            // TODO pipe stdin/out
            let wasi_env = WasiState::new(name)
                .args(args)
                // .env("KEY", "Value")
                .finalize(&mut self.store)?;

            let import_object = wasi_env.import_object(&mut self.store, module)?;
            let instance = Instance::new(&mut self.store, module, &import_object)?;

            let memory = instance.exports.get_memory("memory")?;
            wasi_env
                .data_mut(&mut self.store)
                .set_memory(memory.clone());

            let start = instance.exports.get_function("_start")?;
            start.call(&mut self.store, &[])?;
        } else {
            eprint!("\runknown wasm command: {}\r\n", name);
        }
        Ok(())
    }
}
