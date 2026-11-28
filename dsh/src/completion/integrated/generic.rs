//! Eligibility for generic candidate sources after resolving the cursor context.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GenericCompletion {
    Files,
    Directory,
    FileExtensions(Vec<String>),
    Command,
    ExternalCommand,
    DeclaredValues,
    OptionName,
}

impl GenericCompletion {
    pub(crate) fn owns_candidates(&self) -> bool {
        matches!(
            self,
            Self::Directory
                | Self::FileExtensions(_)
                | Self::DeclaredValues
                | Self::ExternalCommand
        )
    }
}

impl IntegratedCompletionEngine {
    pub(super) fn generic_completion(&self, parsed: &ParsedCommandLine) -> GenericCompletion {
        use parser::CompletionContext;

        match self.argument_type_for_completion_context(parsed) {
            Some(ArgumentType::Directory) => GenericCompletion::Directory,
            Some(ArgumentType::File {
                extensions: Some(values),
            }) => GenericCompletion::FileExtensions(values),
            Some(ArgumentType::Choice(_) | ArgumentType::Number) => {
                GenericCompletion::DeclaredValues
            }
            Some(ArgumentType::Command | ArgumentType::CommandWithArgs) => {
                GenericCompletion::ExternalCommand
            }
            _ => match parsed.completion_context {
                CompletionContext::Command => GenericCompletion::Command,
                CompletionContext::ShortOption | CompletionContext::LongOption => {
                    GenericCompletion::OptionName
                }
                _ => GenericCompletion::Files,
            },
        }
    }
}
