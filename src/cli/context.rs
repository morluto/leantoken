use super::*;
use crate::model::{ContextResponseProfile, ContextWorkflow};

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum ContextWorkflowArg {
    #[default]
    Auto,
    Implementation,
    Contribution,
    Review,
    Investigation,
}

impl From<ContextWorkflowArg> for ContextWorkflow {
    fn from(value: ContextWorkflowArg) -> Self {
        match value {
            ContextWorkflowArg::Auto => Self::Auto,
            ContextWorkflowArg::Implementation => Self::Implementation,
            ContextWorkflowArg::Contribution => Self::Contribution,
            ContextWorkflowArg::Review => Self::Review,
            ContextWorkflowArg::Investigation => Self::Investigation,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ContextResponseProfileArg {
    Compact,
    Balanced,
    Explain,
}

impl From<ContextResponseProfileArg> for ContextResponseProfile {
    fn from(value: ContextResponseProfileArg) -> Self {
        match value {
            ContextResponseProfileArg::Compact => Self::Compact,
            ContextResponseProfileArg::Balanced => Self::Balanced,
            ContextResponseProfileArg::Explain => Self::Explain,
        }
    }
}

#[derive(Debug, Clone, Parser)]
pub struct ContextArgs {
    #[arg(short, long)]
    pub task: String,
    #[arg(long, value_enum, default_value = "auto")]
    pub workflow: ContextWorkflowArg,
    #[arg(short, long, value_parser = parse_positive_usize, default_value_t = DEFAULT_CONTEXT_TOKENS)]
    pub budget: usize,
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_response_tokens: Option<usize>,
    #[arg(long = "include")]
    pub include_paths: Vec<String>,
    #[arg(long = "focus")]
    pub focus_paths: Vec<String>,
    #[arg(long = "exclude")]
    pub exclude_paths: Vec<String>,
    #[arg(long = "known-hash")]
    pub known_hashes: Vec<String>,
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_fragments: Option<usize>,
    #[arg(long, value_enum)]
    pub response_profile: Option<ContextResponseProfileArg>,
}

impl ContextArgs {
    pub(super) fn request(&self) -> ContextRequest {
        ContextRequest {
            task: self.task.clone(),
            token_budget: self.budget,
            include_paths: self.include_paths.clone(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            required_evidence: Vec::new(),
            max_fragments: self.max_fragments,
            plan_only: false,
            focus_paths: self.focus_paths.clone(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: Vec::new(),
            exclude_paths: self.exclude_paths.clone(),
            known_hashes: self.known_hashes.clone(),
            receipt_id: None,
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
            strict_changed_paths: false,
            explain_diagnostics: false,
        }
    }

    pub(super) fn workflow_evidence(&self) -> WorkflowEvidence {
        WorkflowEvidence::default()
    }
}
