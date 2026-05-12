use crate::shell::Shell;
use anyhow::Result;
use parking_lot::RwLock;
use skim::prelude::*;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

pub mod actions;

use async_trait::async_trait;

/// Trait for executable actions in the command palette
#[async_trait(?Send)]
pub trait Action: Send + Sync {
    /// Display name of the action
    fn name(&self) -> &str;
    /// Description of the action
    fn description(&self) -> &str;
    /// Optional command usage hint shown in the palette.
    fn usage(&self) -> Option<&str> {
        None
    }
    /// Optional keyboard shortcut shown in the palette.
    fn shortcut(&self) -> Option<&str> {
        None
    }
    /// Optional related commands shown in the palette.
    fn related(&self) -> Option<&str> {
        None
    }
    /// Icon for the action (emoji)
    fn icon(&self) -> &str {
        "🔹"
    }
    /// Category of the action (for grouping in UI)
    fn category(&self) -> &str {
        "General"
    }
    /// Execute the action
    async fn execute(&self, shell: &mut Shell, input: &str) -> Result<()>;
}

/// Registry for managing available actions
pub struct ActionRegistry {
    actions: HashMap<String, Arc<dyn Action>>,
}

impl Default for ActionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self {
            actions: HashMap::new(),
        }
    }

    pub fn register(&mut self, action: Arc<dyn Action>) {
        self.actions.insert(action.name().to_string(), action);
    }

    pub fn get_all(&self) -> Vec<Arc<dyn Action>> {
        let mut actions: Vec<_> = self.actions.values().cloned().collect();
        // Sort by category, then by name
        actions.sort_by(|a, b| {
            let cat_cmp = a.category().cmp(b.category());
            if cat_cmp == std::cmp::Ordering::Equal {
                a.name().cmp(b.name())
            } else {
                cat_cmp
            }
        });
        actions
    }

    /// Snapshot current registry state for transactional operations.
    pub fn snapshot_actions(&self) -> HashMap<String, Arc<dyn Action>> {
        self.actions.clone()
    }

    /// Restore registry state from a snapshot.
    pub fn restore_actions(&mut self, actions: HashMap<String, Arc<dyn Action>>) {
        self.actions = actions;
    }
}

pub static REGISTRY: LazyLock<RwLock<ActionRegistry>> =
    LazyLock::new(|| RwLock::new(ActionRegistry::new()));

static REGISTER_ONCE: std::sync::Once = std::sync::Once::new();

/// Helper to register built-in actions
pub fn register_builtin_actions() {
    REGISTER_ONCE.call_once(|| {
        let mut registry = REGISTRY.write();
        actions::register_all(&mut registry);
    });
}

// --- UI Wrapper Item ---

struct PaletteItem {
    action: Arc<dyn Action>,
}

impl SkimItem for PaletteItem {
    fn text(&self) -> Cow<'_, str> {
        let cat = self.action.category();
        let icon = self.action.icon();
        let name = self.action.name();
        let mut details = self.action.description().to_string();
        if let Some(usage) = self.action.usage() {
            details.push_str(" | use: ");
            details.push_str(usage);
        }
        if let Some(shortcut) = self.action.shortcut() {
            details.push_str(" | key: ");
            details.push_str(shortcut);
        }
        if let Some(related) = self.action.related() {
            details.push_str(" | related: ");
            details.push_str(related);
        }
        Cow::Owned(format!("[{:<8}] {} {:<20} {}", cat, icon, name, details))
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.action.name())
    }
}

/// A simple wrapper for String that implements SkimItem
#[derive(Debug, Clone)]
pub struct StringItem(pub String);

impl SkimItem for StringItem {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.0)
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.0)
    }
}

/// Command Palette UI
pub struct CommandPalette;

impl CommandPalette {
    pub async fn run(shell: &mut Shell, input: &str) -> Result<Option<String>> {
        let actions = {
            let registry = REGISTRY.read();
            registry.get_all()
        };

        // Prepare items for Skim
        let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
        for action in actions {
            tx_item.send(vec![Arc::new(PaletteItem { action })]).ok();
        }
        drop(tx_item); // Close sender to signal end of items

        // Skim options
        let options = SkimOptionsBuilder::default()
            // .height("40%".to_string()) // Remove height to use full screen / alternate screen
            .multi(false)
            .prompt("Cmd> ".to_string())
            .bind(vec!["Enter:accept".to_string(), "Esc:abort".to_string()])
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        // Run Skim synchronously (blocking the worker thread is acceptable here as it's a modal UI)
        let selected_items = crate::utils::skim::run_skim_with(options, Some(rx_item))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected_items.first() {
            // Downcast or retrieve Action
            // SkimItem is a trait object, effectively Arc<dyn SkimItem>
            // We can't easily downcast back to PaletteItem directly from SkimItem without Any.
            // But we know text() matches the name.

            let output = item.output().to_string(); // this is the name
            let action_name = output.as_str();

            if action_name == "Search History" {
                return actions::search_history::select_history_command(shell).await;
            }

            // Re-acquire lock to get the action (we dropped it before running Skim)
            let action = {
                let registry = REGISTRY.read();
                registry.actions.get(action_name).cloned()
            };

            if let Some(action) = action {
                action.execute(shell, input).await?;
            }
        };
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockAction {
        name: &'static str,
        desc: &'static str,
    }

    #[async_trait(?Send)]
    impl Action for MockAction {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            self.desc
        }
        async fn execute(&self, _shell: &mut Shell, _input: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_registry_registration() {
        let mut registry = ActionRegistry::new();
        let action = Arc::new(MockAction {
            name: "Test Action",
            desc: "Test Description",
        });

        registry.register(action);
        let actions = registry.get_all();

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name(), "Test Action");
        assert_eq!(actions[0].description(), "Test Description");
    }

    #[test]
    fn test_builtin_actions_properties() {
        // Test ClearScreenAction properties
        let clear_screen = actions::clear_screen::ClearScreenAction;
        assert_eq!(clear_screen.name(), "Clear Screen");
        assert_eq!(clear_screen.description(), "Clear the terminal screen");

        // Test ReloadConfigAction properties
        let reload_config = actions::reload_config::ReloadConfigAction;
        assert_eq!(reload_config.name(), "Reload Config");
        assert_eq!(reload_config.description(), "Reload config.lisp");
    }

    #[test]
    fn test_string_item() {
        let item = StringItem("test value".to_string());
        assert_eq!(item.text(), "test value");
        assert_eq!(item.output(), "test value");

        let item_clone = item.clone();
        assert_eq!(item_clone.text(), "test value");
    }
}
