use super::{RuntimeError, Symbol, Value};
use crate::environment::Environment;
use parking_lot::RwLock;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::{collections::HashMap, fmt::Debug};

/// An environment of symbol bindings. Used for the base environment, for
/// closures, for `let` statements, for function arguments, etc.
#[derive(Debug)]
pub struct Env {
    parent: Option<Rc<RefCell<Env>>>,
    entries: HashMap<Symbol, Value>,
    pub shell_env: Arc<RwLock<Environment>>,
}

impl Env {
    /// Create a new, empty environment
    pub fn new(shell_env: Arc<RwLock<Environment>>) -> Self {
        Self {
            parent: None,
            entries: HashMap::new(),
            shell_env,
        }
    }

    /// Create a new environment extending the given environment
    pub fn extend(parent: Rc<RefCell<Env>>) -> Self {
        let shell_env = Arc::clone(&parent.borrow_mut().shell_env);
        Self {
            parent: Some(parent),
            entries: HashMap::new(),
            shell_env,
        }
    }

    /// Walks up the environment hierarchy until it finds the symbol's value or
    /// runs out of environments.
    pub fn get(&self, key: &Symbol) -> Option<Value> {
        if let Some(val) = self.entries.get(key) {
            Some(val.clone()) // clone the Rc
        } else if let Some(parent) = &self.parent {
            parent.borrow().get(key)
        } else {
            None
        }
    }

    /// Define a new key in the current environment
    pub fn define(&mut self, key: Symbol, value: Value) {
        self.entries.insert(key, value);
    }

    /// Find the environment where this key is defined, and update its value.
    /// Returns an Err if the symbol has not been defined anywhere in the hierarchy.
    pub fn set(&mut self, key: Symbol, value: Value) -> Result<(), RuntimeError> {
        use std::collections::hash_map::Entry;
        match self.entries.entry(key.clone()) {
            Entry::Occupied(mut entry) => {
                entry.insert(value);
                Ok(())
            }
            Entry::Vacant(_) => {
                if let Some(parent) = &self.parent {
                    parent.borrow_mut().set(key, value)
                } else {
                    Err(RuntimeError {
                        msg: format!("Tried to set value of undefined symbol \"{key}\""),
                    })
                }
            }
        }
    }

    /// Delete the nearest (going upwards) definition of this key
    pub fn undefine(&mut self, key: &Symbol) {
        if self.entries.contains_key(key) {
            self.entries.remove(key);
        } else if let Some(parent) = &self.parent {
            parent.borrow_mut().undefine(key);
        }
    }

    fn display_recursive(&self, output: &mut String, depth: i32) {
        let indent = &(0..depth).map(|_| "  ").collect::<String>();

        output.push_str(indent);
        output.push_str("{ ");

        for (symbol, value) in &self.entries {
            output.push_str(format!("\n{indent}  {symbol}: {value}").as_str());
        }

        if let Some(parent) = &self.parent {
            output.push_str("\n\n");
            parent
                .as_ref()
                .borrow()
                .display_recursive(output, depth + 1);
        }

        output.push('\n');
        output.push_str(indent);
        output.push('}');
    }
}

impl std::fmt::Display for Env {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        let mut output = String::new();

        output.push_str("Env: ");
        self.display_recursive(&mut output, 0);

        write!(formatter, "{}", &output)
    }
}
