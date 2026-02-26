use skim::Skim;
use skim::prelude::*;

/// Run skim in a Tokio blocking section to avoid nested-runtime panics.
pub fn run_skim_with(options: SkimOptions, rx: Option<SkimItemReceiver>) -> Option<SkimOutput> {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| Skim::run_with(options, rx).ok())
    } else {
        Skim::run_with(options, rx).ok()
    }
}
