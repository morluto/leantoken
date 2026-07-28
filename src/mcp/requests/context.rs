use super::*;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = add_context_option_constraints)]
pub(in crate::mcp) struct ContextMcpRequest {
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
    pub(in crate::mcp) task: String,
    /// Maximum source tokens across selected fragments (default 3000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(
        schema_with = "context_token_limit_schema",
        default = "default_context_token_option"
    )]
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
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
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
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
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
    pub(in crate::mcp) base_revision: Option<String>,
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
    /// Legacy alias for `response_profile=explain`; conflicts with an explicit
    /// `compact` or `balanced` profile.
    #[serde(default)]
    pub(in crate::mcp) verbose_diagnostics: bool,
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
        )
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
        let options = self
            .max_response_tokens
            .map_or_else(ServiceCallOptions::new, |limit| {
                ServiceCallOptions::new().with_max_response_tokens(limit)
            });
        let options = self.response_profile.map_or(options, |profile| {
            options.with_context_response_profile(profile)
        });
        (
            ContextRequest {
                task: self.task,
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
                base_revision: self.base_revision,
                changed_paths: self.changed_paths,
                strict_changed_paths: self.strict_changed_paths,
                verbose_diagnostics: self.verbose_diagnostics,
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

pub(in crate::mcp) const fn default_context_token_option() -> Option<usize> {
    Some(DEFAULT_CONTEXT_TOKENS)
}

pub(in crate::mcp) const fn default_context_fragment_option() -> Option<usize> {
    Some(DEFAULT_CONTEXT_FRAGMENTS)
}

pub(in crate::mcp) fn context_fragment_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "minimum": 1,
        "maximum": MAX_RESULTS,
        "default": DEFAULT_CONTEXT_FRAGMENTS
    })
}

pub(in crate::mcp) fn context_focus_fragment_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "minimum": 1,
        "maximum": MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN,
        "default": null
    })
}

pub(in crate::mcp) fn add_context_option_constraints(schema: &mut Schema) {
    schema.insert(
        "allOf".into(),
        serde_json::json!([
            {
                "if": {
                    "properties": {"strict_focus_paths": {"const": true}},
                    "required": ["strict_focus_paths"]
                },
                "then": {
                    "properties": {"focus_paths": {"minItems": 1}},
                    "required": ["focus_paths"]
                }
            },
            {
                "if": {
                    "properties": {
                        "minimum_fragments_per_focus_path": {"not": {"type": "null"}}
                    },
                    "required": ["minimum_fragments_per_focus_path"]
                },
                "then": {
                    "properties": {"focus_paths": {"minItems": 1}},
                    "required": ["focus_paths"]
                }
            },
            {
                "if": {
                    "properties": {"plan_only": {"const": true}},
                    "required": ["plan_only"]
                },
                "then": {
                    "properties": {
                        "receipt_id": {"type": "null"},
                        "handoff": {"type": "null"}
                    }
                }
            }
        ]),
    );
}
