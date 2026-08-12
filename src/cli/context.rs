use super::*;

#[derive(Debug, Clone, Parser)]
pub struct ContextArgs {
    /// Task description.
    #[arg(short, long)]
    pub task: String,

    /// Consistency boundary for this retrieval.
    #[command(flatten)]
    pub index_consistency: RetrievalConsistencyArgs,

    /// Evidence workflow; auto selects only on high-confidence task language.
    #[arg(long, value_enum, default_value = "auto")]
    pub workflow: ContextWorkflowArg,

    /// Caller-observed compiler, test, runtime, or log excerpt (repeatable).
    #[arg(long = "failure-trace")]
    pub failure_traces: Vec<String>,

    /// Caller-observed exact or qualified identifier (repeatable).
    #[arg(long = "evidence-symbol")]
    pub evidence_symbols: Vec<String>,

    /// Caller-observed repository-relative path (repeatable).
    #[arg(long = "evidence-path")]
    pub evidence_paths: Vec<String>,

    /// Caller-observed test name, command, or behavioral check (repeatable).
    #[arg(long = "test-intent")]
    pub test_intents: Vec<String>,

    /// Maximum source tokens across returned fragments.
    #[arg(
        short,
        long,
        value_parser = parse_positive_usize,
        default_value_t = DEFAULT_CONTEXT_TOKENS
    )]
    pub budget: usize,

    /// Maximum tokens in the final serialized JSON service response.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_response_tokens: Option<usize>,

    /// Include only paths matching these patterns (repeatable).
    #[arg(long = "include")]
    pub include_paths: Vec<String>,

    /// Require evidence matching each path pattern (repeatable).
    #[arg(long = "must-include")]
    pub must_include_paths: Vec<String>,

    /// Require evidence for each exact symbol (repeatable).
    #[arg(long = "must-include-symbol")]
    pub must_include_symbols: Vec<String>,

    /// Require path-scoped literal evidence as a JSON object (repeatable).
    #[arg(long = "required-evidence", value_name = "JSON", value_parser = parse_required_evidence)]
    pub required_evidence: Vec<ContextRequiredEvidence>,

    /// Maximum number of returned fragments (default: 8).
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_fragments: Option<usize>,

    /// Preview ranked candidates without returning source fragments; cannot be
    /// combined with --handoff.
    #[arg(long)]
    pub plan_only: bool,

    /// Focus on these paths (repeatable).
    #[arg(long = "focus")]
    pub focus_paths: Vec<String>,

    /// Restrict returned fragments to focus paths; requires --focus.
    #[arg(long)]
    pub strict_focus_paths: bool,

    /// Minimum fragments to return for each focus path; requires --focus.
    #[arg(long, value_parser = parse_positive_usize)]
    pub minimum_fragments_per_focus_path: Option<usize>,

    /// Focus on these symbols (repeatable).
    #[arg(long = "focus-symbol")]
    pub focus_symbols: Vec<String>,

    /// Exclude these paths (repeatable).
    #[arg(long = "exclude")]
    pub exclude_paths: Vec<String>,

    /// Content hashes the caller already holds (repeatable).
    #[arg(long = "known-hash")]
    pub known_hashes: Vec<String>,

    /// Prior repository generation for delta context.
    #[arg(long = "prior-generation")]
    pub prior_repository_generation: Option<u64>,

    /// Base revision or immutable range (e.g. "origin/main" or "BASE..HEAD").
    #[arg(long = "base-revision")]
    pub base_revision: Option<String>,

    /// Changed paths for diff-scoped context (repeatable).
    #[arg(long = "changed-path")]
    pub changed_paths: Vec<String>,

    /// Restrict returned fragments to resolved changed paths.
    #[arg(long)]
    pub strict_changed_paths: bool,

    /// Response presentation depth; balanced preserves the historical default.
    #[arg(long, value_enum)]
    pub response_profile: Option<ContextResponseProfileArg>,

    /// Include the full bounded omission and diff diagnostics.
    #[arg(long = "verbose-diagnostics")]
    pub explain_diagnostics: bool,

    /// Attach compact provenance for a host-triggered executor handoff; cannot be
    /// combined with --plan-only.
    #[arg(long)]
    pub handoff: bool,

    /// Override the compact handoff task summary.
    #[arg(long, value_name = "TEXT", requires = "handoff")]
    pub handoff_summary: Option<String>,
}

impl ContextArgs {
    pub(super) fn handoff_request(&self) -> Option<HandoffManifestRequest> {
        self.handoff.then(|| HandoffManifestRequest {
            summary: self.handoff_summary.clone(),
            ..HandoffManifestRequest::default()
        })
    }

    pub(super) fn workflow_evidence(&self) -> WorkflowEvidence {
        WorkflowEvidence::new()
            .with_failure_traces(self.failure_traces.clone())
            .with_symbols(self.evidence_symbols.clone())
            .with_paths(self.evidence_paths.clone())
            .with_test_intents(self.test_intents.clone())
    }
}

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum ContextWorkflowArg {
    #[default]
    Auto,
    Implementation,
    Contribution,
    Review,
    Investigation,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ContextResponseProfileArg {
    Compact,
    Balanced,
    Explain,
}

impl From<ContextResponseProfileArg> for crate::model::ContextResponseProfile {
    fn from(value: ContextResponseProfileArg) -> Self {
        match value {
            ContextResponseProfileArg::Compact => Self::Compact,
            ContextResponseProfileArg::Balanced => Self::Balanced,
            ContextResponseProfileArg::Explain => Self::Explain,
        }
    }
}

impl From<ContextWorkflowArg> for crate::model::ContextWorkflow {
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

impl From<ContextArgs> for ContextRequest {
    fn from(args: ContextArgs) -> Self {
        Self {
            task: args.task,
            token_budget: args.budget,
            include_paths: args.include_paths,
            must_include_paths: args.must_include_paths,
            must_include_symbols: args.must_include_symbols,
            required_evidence: args.required_evidence,
            max_fragments: args.max_fragments,
            plan_only: args.plan_only,
            focus_paths: args.focus_paths,
            strict_focus_paths: args.strict_focus_paths,
            minimum_fragments_per_focus_path: args.minimum_fragments_per_focus_path,
            focus_symbols: args.focus_symbols,
            exclude_paths: args.exclude_paths,
            known_hashes: args.known_hashes,
            receipt_id: None,
            prior_repository_generation: args.prior_repository_generation,
            base_revision: args.base_revision,
            changed_paths: args.changed_paths,
            strict_changed_paths: args.strict_changed_paths,
            explain_diagnostics: args.explain_diagnostics,
        }
    }
}
