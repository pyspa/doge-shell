pub mod clear_screen;
pub mod lisp_action;
pub mod reload_config;

pub use lisp_action::*;

use super::ActionRegistry;
use std::sync::Arc;

pub fn register_all(registry: &mut ActionRegistry) {
    registry.register(Arc::new(clear_screen::ClearScreenAction));
    registry.register(Arc::new(reload_config::ReloadConfigAction));
}
