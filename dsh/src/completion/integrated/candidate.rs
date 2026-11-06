//! Completion candidate types shared across the whole completion subsystem:
//! `EnhancedCandidate` (what a provider produces) and `CandidateType` (its
//! sort order, icon, and mapping to the display-facing `Candidate` enum).
use crate::completion::display::{Candidate, ShellSyntaxKind};

/// Enhanced completion candidate
#[derive(Debug, Clone)]
pub struct EnhancedCandidate {
    /// Candidate text
    pub text: String,
    /// Description
    pub description: Option<String>,
    /// Candidate type
    pub candidate_type: CandidateType,
    /// Priority
    pub priority: u32,
}

impl EnhancedCandidate {
    /// Convert to Candidate for skim display
    pub fn to_candidate(&self) -> Candidate {
        match self.candidate_type {
            CandidateType::VariableReference | CandidateType::HomeReference => {
                Candidate::ShellSyntax {
                    kind: if self.candidate_type == CandidateType::VariableReference {
                        ShellSyntaxKind::Variable
                    } else {
                        ShellSyntaxKind::Home
                    },
                    text: self.text.clone(),
                    description: self.description.clone(),
                }
            }
            CandidateType::SubCommand => Candidate::Command {
                name: self.text.clone(),
                description: self.description.clone().unwrap_or_default(),
            },
            CandidateType::LongOption | CandidateType::ShortOption => Candidate::Option {
                name: self.text.clone(),
                description: self.description.clone().unwrap_or_default(),
            },
            CandidateType::HomeFile | CandidateType::HomeDirectory => {
                Candidate::HomePath(Box::new(Candidate::File {
                    path: self.text.clone(),
                    is_dir: self.candidate_type == CandidateType::HomeDirectory,
                }))
            }
            CandidateType::File => Candidate::File {
                path: self.text.clone(),
                is_dir: false,
            },
            CandidateType::Directory => Candidate::File {
                path: self.text.clone(),
                is_dir: true,
            },
            CandidateType::Argument | CandidateType::Generic => {
                if let Some(ref desc) = self.description {
                    Candidate::Item(self.text.clone(), desc.clone())
                } else {
                    Candidate::Basic(self.text.clone())
                }
            }
            CandidateType::Process => Candidate::Process {
                pid: self.text.clone(),
                command: self.description.clone().unwrap_or_default(),
            },
        }
    }
}

/// Candidate type
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CandidateType {
    /// Subcommand
    SubCommand,
    /// Short option
    ShortOption,
    /// Long option
    LongOption,
    /// Argument
    Argument,
    /// File
    File,
    /// Directory
    Directory,
    /// Process
    Process,
    /// Generic
    Generic,
    /// Validated variable or home expansion emitted by a built-in producer.
    VariableReference,
    HomeReference,
    HomeFile,
    HomeDirectory,
}

impl CandidateType {
    /// Get sort order
    pub fn sort_order(&self) -> u8 {
        match self {
            CandidateType::SubCommand => 1,
            CandidateType::LongOption => 2,
            CandidateType::ShortOption => 3,
            CandidateType::Argument => 4,
            CandidateType::Directory | CandidateType::HomeDirectory => 5,
            CandidateType::File | CandidateType::HomeFile => 6,
            CandidateType::Process => 7,
            CandidateType::Generic
            | CandidateType::VariableReference
            | CandidateType::HomeReference => 8,
        }
    }

    /// Get display icon
    pub fn icon(&self) -> &'static str {
        match self {
            CandidateType::SubCommand => "⚡",
            CandidateType::LongOption => "🔧",
            CandidateType::ShortOption => "🔧",
            CandidateType::Argument => "📝",
            CandidateType::File | CandidateType::HomeFile => "📄",
            CandidateType::Directory | CandidateType::HomeDirectory => "📁",
            CandidateType::Process => "🔧",
            CandidateType::Generic
            | CandidateType::VariableReference
            | CandidateType::HomeReference => "💡",
        }
    }
}
