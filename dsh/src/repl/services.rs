use super::FileContextCache;
use super::prompt_refresh::PromptRefreshCoordinator;
use crate::ai_features::AiService;
use crate::argument_explainer::ArgumentExplainer;
use crate::command_timing::SharedCommandTiming;
use crate::prompt::Prompt;
use parking_lot::RwLock;
use std::sync::Arc;

pub(crate) struct ReplServices {
    pub ai: Option<Arc<dyn AiService + Send + Sync>>,
    pub command_timing: SharedCommandTiming,
    pub file_context: Arc<RwLock<FileContextCache>>,
    pub argument_explainer: ArgumentExplainer,
    pub prompt_refresh: PromptRefreshCoordinator,
}

impl ReplServices {
    pub fn new(
        ai: Option<Arc<dyn AiService + Send + Sync>>,
        command_timing: SharedCommandTiming,
        prompt: Arc<RwLock<Prompt>>,
    ) -> Self {
        Self {
            ai,
            command_timing,
            file_context: Arc::new(RwLock::new(FileContextCache::new())),
            argument_explainer: ArgumentExplainer::new(),
            prompt_refresh: PromptRefreshCoordinator::new(prompt),
        }
    }
}
