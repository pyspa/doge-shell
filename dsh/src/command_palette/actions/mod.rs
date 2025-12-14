pub mod clear_screen;
pub mod reload_config;

use super::ActionRegistry;
use std::sync::Arc;

pub fn register_all(registry: &mut ActionRegistry) {
    registry.register(Arc::new(clear_screen::ClearScreenAction));
    registry.register(Arc::new(reload_config::ReloadConfigAction));
}
