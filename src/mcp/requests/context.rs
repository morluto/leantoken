use super::*;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    description = "Context cross-field relationships remain runtime-validated: strict focus constraints require focus_paths, plan_only cannot combine with receipt_id or handoff, and handoff cannot be combined with plan_only."
)]
pub(in crate::mcp) struct ContextMcpRequest {
    /// Optional name of an approved repository context.
    #[serde(default)]
    #[schemars(schema_with = "repository_context_schema")]
    pub(in crate::mcp) repository_context: Option<String>,
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(length(max = 128))]
    pub(in crate::mcp) expected_repository_id: Option<String>,
    /// Evidence workflow; `auto` selects only on high-confidence task language.
    #[serde(default)]
    pub(in crate::mcp) workflow: ContextWorkflow,
    /// Typed caller-observed failure traces, symbols, paths, and test intent.
    #[serde(default)]
    pub(in crate::mcp) workflow_evidence: WorkflowEvidence,
    /// Natural-language coding task; include known identifiers and constraints.
    #[schemars(length(min = 3, max = 65536))]
    pub(in crate::mcp) task: NonEmptyText,
    /// Maximum source tokens across selected fragments (default 3000, maximum 32000).
    #[serde(default)]
    #[schemars(schema_with = "context_token_limit_schema")]
    pub(in crate::mcp) token_budget: Option<usize>,
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    pub(in crate::mcp) max_response_tokens: Option<usize>,
    /// Require every returned source fragment to match one of these path patterns.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    pub(in crate::mcp) include_paths: Vec<String>,
    /// Require evidence matching every indexed path pattern.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    pub(in crate::mcp) must_include_paths: Vec<String>,
    /// Require evidence for every exact indexed symbol.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    pub(in crate::mcp) must_include_symbols: Vec<String>,
    /// Require matching query evidence within each path-scoped contract.
    #[serde(default)]
    #[schemars(length(max = 32))]
    pub(in crate::mcp) required_evidence: Vec<ContextRequiredEvidence>,
    /// Maximum returned fragments (default 8, maximum 100).
    #[serde(default)]
    #[schemars(
        schema_with = "context_fragment_limit_schema",
        default = "default_context_fragment_option"
    )]
    pub(in crate::mcp) max_fragments: Option<usize>,
    /// Preview ranked candidates without source or receipt mutation; omit `receipt_id`
    /// and `handoff`.
    #[serde(default)]
    pub(in crate::mcp) plan_only: bool,
    /// Boost matching paths without filtering other candidates.
    #[serde(default)]
    #[schemars(length(max = 32), inner(length(max = 4096)))]
    pub(in crate::mcp) focus_paths: Vec<String>,
    /// Require every returned fragment to match at least one focus path; requires
    /// non-empty `focus_paths`.
    #[serde(default)]
    pub(in crate::mcp) strict_focus_paths: bool,
    /// Minimum returned fragments required per focus path (maximum 8); requires
    /// non-empty `focus_paths`.
    #[serde(default)]
    #[schemars(schema_with = "context_focus_fragment_limit_schema")]
    pub(in crate::mcp) minimum_fragments_per_focus_path: Option<usize>,
    /// Boost candidates for these exact symbol names.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    pub(in crate::mcp) focus_symbols: Vec<String>,
    /// Exclude matching repository paths.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    pub(in crate::mcp) exclude_paths: Vec<String>,
    /// Fragment hashes already held by the caller and not to resend.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 128)))]
    pub(in crate::mcp) known_hashes: Vec<String>,
    /// Suppress evidence already returned under this server-managed receipt; omit
    /// when `plan_only` is true.
    #[serde(default)]
    #[schemars(length(max = 128))]
    pub(in crate::mcp) receipt_id: Option<String>,
    /// Earlier generation used to boost files indexed since that response.
    #[serde(default)]
    pub(in crate::mcp) prior_repository_generation: Option<u64>,
    /// Base revision or `BASE..HEAD` range for diff-scoped context.
    #[serde(default)]
    #[schemars(length(max = 256))]
    pub(in crate::mcp) base_revision: Option<NonEmptyText>,
    /// Changed paths for diff-scoped context.
    #[serde(default)]
    #[schemars(length(max = 512), inner(length(max = 4096)))]
    pub(in crate::mcp) changed_paths: Vec<String>,
    /// Require every returned fragment to belong to the resolved changed paths.
    #[serde(default)]
    pub(in crate::mcp) strict_changed_paths: bool,
    /// Response presentation depth; defaults to `balanced`. `compact` removes
    /// optional diff and omission detail, while `explain` includes bounded detail.
    #[serde(default)]
    pub(in crate::mcp) response_profile: Option<ContextResponseProfile>,
    /// Attach a compact provenance manifest for a host-triggered executor handoff;
    /// cannot be combined with `plan_only`.
    #[serde(default)]
    pub(in crate::mcp) handoff: Option<HandoffManifestRequest>,
    /// Use `reconcile_working_tree` after edits; otherwise `indexed_generation`.
    #[serde(default)]
    #[schemars(schema_with = "index_consistency_schema")]
    pub(in crate::mcp) consistency: IndexConsistency,
}

impl ContextMcpRequest {
    pub(in crate::mcp) fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit(
            "token_budget",
            self.token_budget,
            limits.max_output_tokens,
        )?;
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )?;
        validate_optional_positive_limit("max_fragments", self.max_fragments, limits.max_results)?;
        validate_optional_positive_limit(
            "minimum_fragments_per_focus_path",
            self.minimum_fragments_per_focus_path,
            limits.max_results,
        )?;
        if (self.strict_focus_paths || self.minimum_fragments_per_focus_path.is_some())
            && self.focus_paths.is_empty()
        {
            return Err(crate::Error::InvalidInput {
                field: "focus paths",
                reason: "must not be empty when focus path constraints are enabled",
            });
        }
        Ok(())
    }

    pub(in crate::mcp) fn into_parts(
        self,
        default_token_budget: usize,
    ) -> (
        ContextRequest,
        ContextWorkflow,
        WorkflowEvidence,
        IndexConsistency,
        ServiceCallOptions,
        Option<String>,
        Option<HandoffManifestRequest>,
    ) {
        let options = if self.plan_only {
            service_call_options(self.max_response_tokens)
        } else {
            service_call_options_with_receipt(self.max_response_tokens)
        };
        let options = self.response_profile.map_or(options, |profile| {
            options.with_context_response_profile(profile)
        });
        (
            ContextRequest {
                task: self.task.into_string(),
                token_budget: self.token_budget.unwrap_or(default_token_budget),
                include_paths: self.include_paths,
                must_include_paths: self.must_include_paths,
                must_include_symbols: self.must_include_symbols,
                required_evidence: self.required_evidence,
                max_fragments: self.max_fragments,
                plan_only: self.plan_only,
                focus_paths: self.focus_paths,
                strict_focus_paths: self.strict_focus_paths,
                minimum_fragments_per_focus_path: self.minimum_fragments_per_focus_path,
                focus_symbols: self.focus_symbols,
                exclude_paths: self.exclude_paths,
                known_hashes: self.known_hashes,
                receipt_id: self.receipt_id,
                prior_repository_generation: self.prior_repository_generation,
                base_revision: self.base_revision.map(NonEmptyText::into_string),
                changed_paths: self.changed_paths,
                strict_changed_paths: self.strict_changed_paths,
                explain_diagnostics: false,
            },
            self.workflow,
            self.workflow_evidence,
            self.consistency,
            options,
            self.expected_repository_id,
            self.handoff,
        )
    }
}

pub(in crate::mcp) const fn default_context_fragment_option() -> Option<usize> {
    Some(DEFAULT_CONTEXT_FRAGMENTS)
}

pub(in crate::mcp) fn context_fragment_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": ["integer", "null"],
        "minimum": 1,
        "maximum": MAX_RESULTS,
        "default": DEFAULT_CONTEXT_FRAGMENTS
    })
}

pub(in crate::mcp) fn context_focus_fragment_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": ["integer", "null"],
        "minimum": 1,
        "maximum": MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN,
        "default": null
    })
}
