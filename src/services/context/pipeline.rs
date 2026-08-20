#[derive(Clone, Copy)]
pub(super) struct ContextSignals {
    pub(super) import_neighbor: bool,
    pub(super) reverse_dependency: bool,
    pub(super) caller: bool,
}

pub(super) struct ContextPolicy {
    delivery: ContextDelivery,
    focus: ContextFocusPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkingTreeObservation {
    Unknown,
    Clean,
    Modified,
    Untracked,
    ModifiedAndUntracked,
    DirtyUnclassified,
}

impl WorkingTreeObservation {
    pub(super) fn from_status(status: &GitWorkingTreeStatus) -> Self {
        if !status.is_available() {
            return Self::Unknown;
        }
        if status.changed_paths.is_empty() {
            return Self::Clean;
        }
        match (status.has_modified(), status.has_untracked()) {
            (true, true) => Self::ModifiedAndUntracked,
            (true, false) => Self::Modified,
            (false, true) => Self::Untracked,
            (false, false) => Self::DirtyUnclassified,
        }
    }

    pub(super) const fn handoff_state(self) -> HandoffWorkingTreeState {
        match self {
            Self::Unknown => HandoffWorkingTreeState::Unknown,
            Self::Clean => HandoffWorkingTreeState::Clean,
            Self::Modified
            | Self::Untracked
            | Self::ModifiedAndUntracked
            | Self::DirtyUnclassified => HandoffWorkingTreeState::Dirty,
        }
    }

    pub(super) const fn provenance_state(self) -> RepositoryWorkingTreeState {
        match self {
            Self::Unknown => RepositoryWorkingTreeState::Unknown,
            Self::Modified | Self::ModifiedAndUntracked => RepositoryWorkingTreeState::Modified,
            Self::Untracked => RepositoryWorkingTreeState::Untracked,
            Self::Clean | Self::DirtyUnclassified => RepositoryWorkingTreeState::Clean,
        }
    }

    pub(super) const fn is_available(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

enum ContextDelivery {
    Plan,
    Fragments {
        receipt_id: Option<String>,
        handoff: Option<HandoffManifestRequest>,
    },
}

impl ContextPolicy {
    pub(super) fn parse(
        request: &ContextRequest,
        handoff: Option<HandoffManifestRequest>,
    ) -> Result<Self> {
        let mut violations = Vec::with_capacity(3);
        if (request.strict_focus_paths || request.minimum_fragments_per_focus_path.is_some())
            && request.focus_paths.is_empty()
        {
            violations.push(crate::InputViolation::new(
                "focus paths",
                "must not be empty when focus path constraints are enabled",
            ));
        }
        if request.plan_only && request.receipt_id.is_some() {
            violations.push(crate::InputViolation::new(
                "receipt_id",
                "must be omitted when plan_only is true",
            ));
        }
        if request.plan_only && handoff.is_some() {
            violations.push(crate::InputViolation::new(
                "plan_only",
                "cannot be combined with a handoff manifest",
            ));
        }
        match violations.len() {
            0 => {}
            1 => {
                let violation = violations[0];
                return Err(Error::InvalidInput {
                    field: violation.field,
                    reason: violation.reason,
                });
            }
            _ => {
                return Err(Error::InvalidInputConstraints(crate::InputViolations::new(
                    violations,
                )));
            }
        }

        let handoff = handoff.map(handoff::parse_request).transpose()?;
        let delivery = if request.plan_only {
            ContextDelivery::Plan
        } else {
            ContextDelivery::Fragments {
                receipt_id: request.receipt_id.clone(),
                handoff,
            }
        };
        let focus = ContextFocusPolicy::parse(request);
        Ok(Self { delivery, focus })
    }

    pub(super) const fn is_plan(&self) -> bool {
        matches!(self.delivery, ContextDelivery::Plan)
    }

    pub(super) const fn focus(&self) -> ContextFocusPolicy {
        self.focus
    }

    pub(super) const fn focus_minimum(&self) -> Option<usize> {
        self.focus.minimum_fragments()
    }

    pub(super) fn receipt_id(&self) -> Option<&str> {
        match &self.delivery {
            ContextDelivery::Plan => None,
            ContextDelivery::Fragments { receipt_id, .. } => receipt_id.as_deref(),
        }
    }

    pub(super) fn handoff(&self) -> Option<&HandoffManifestRequest> {
        match &self.delivery {
            ContextDelivery::Plan => None,
            ContextDelivery::Fragments { handoff, .. } => handoff.as_ref(),
        }
    }
}

#[derive(Default)]
pub(super) struct CandidateBatch {
    pub(super) candidates: Vec<Candidate>,
    pub(super) path_excluded_candidates: Vec<String>,
    pub(super) query_fusion: HashMap<String, HashMap<String, f64>>,
    pub(super) coverage: ContextCoverageReceipt,
    pub(super) warnings: Vec<String>,
    pub(super) workflow_receipt: Option<WorkflowReceipt>,
}

#[derive(Clone, Copy)]
pub(super) struct QueryCandidateExpansion<'a> {
    pub(super) session: &'a IndexReadSnapshot,
    pub(super) request: &'a ContextRequest,
    pub(super) query: &'a ContextQuery,
    pub(super) path_filter: &'a PathFilter,
    pub(super) strict_changed_paths: Option<&'a HashSet<&'a str>>,
    pub(super) changed_paths: &'a HashSet<String>,
    pub(super) path_scorer: &'a ContextPathScorer,
    pub(super) cancellation: &'a CancellationToken,
    pub(super) signals: ContextSignals,
}

pub(super) struct ContextFinalization<'a> {
    pub(super) session: &'a IndexReadSnapshot,
    pub(super) request: &'a ContextRequest,
    pub(super) scoped_request: &'a ContextRequest,
    pub(super) policy: &'a ContextPolicy,
    pub(super) options: ServiceCallOptions,
    pub(super) response_profile: ContextResponseProfile,
    pub(super) diff_evidence_mode: DiffEvidenceMode,
    pub(super) cancellation: &'a CancellationToken,
    pub(super) diagnostics: CandidateDiagnostics,
    pub(super) generation: u64,
    pub(super) diff_scope: Option<&'a DiffScopeReceipt>,
    pub(super) working_tree: WorkingTreeObservation,
    pub(super) working_tree_paths: &'a [String],
    pub(super) working_tree_paths_complete: bool,
    pub(super) working_tree_paths_limit: Option<usize>,
    pub(super) commit_revision: Option<&'a str>,
    pub(super) branch: Option<&'a str>,
    pub(super) resolved_workflow: ContextWorkflow,
}

pub(super) struct AccountedContextResponse {
    pub(super) response: ContextResponse,
    pub(super) baseline_source_tokens: Option<usize>,
    pub(super) operation: TokenAccountingOperation,
}

pub(super) struct ContextExecution {
    pub(super) handoff: Option<HandoffManifestRequest>,
    pub(super) workflow: ContextWorkflow,
    pub(super) workflow_evidence: WorkflowEvidence,
}

/// A workflow-aware context request and its retrieval boundary.
///
/// This is the complete command used by adapters that provide both observed
/// workflow evidence and an explicit index-consistency policy.
pub struct ContextWorkflowOptions {
    /// Task request driving candidate generation.
    pub request: ContextRequest,
    /// Optional host-triggered handoff manifest.
    pub handoff: Option<HandoffManifestRequest>,
    /// Requested or auto-detected workflow.
    pub workflow: ContextWorkflow,
    /// Caller-observed compiler, test, runtime, or log evidence.
    pub workflow_evidence: WorkflowEvidence,
    /// Index consistency boundary for this retrieval.
    pub consistency: IndexConsistency,
    /// Serialized-response and deadline controls.
    pub options: ServiceCallOptions,
    /// Cancellation token observed by the retrieval.
    pub cancellation: CancellationToken,
}

impl ContextExecution {
    pub(super) fn new(workflow: ContextWorkflow) -> Self {
        Self {
            handoff: None,
            workflow,
            workflow_evidence: WorkflowEvidence::default(),
        }
    }

    pub(super) fn with_handoff(mut self, handoff: HandoffManifestRequest) -> Self {
        self.handoff = Some(handoff);
        self
    }

    pub(super) fn with_workflow_evidence(mut self, workflow_evidence: WorkflowEvidence) -> Self {
        self.workflow_evidence = workflow_evidence;
        self
    }
}

impl ContextSignals {
    pub(super) const PRODUCTION: Self = Self {
        import_neighbor: true,
        reverse_dependency: false,
        caller: true,
    };

    pub(super) const fn evaluation(policy: ContextSignalPolicy) -> Self {
        match policy {
            ContextSignalPolicy::LexicalSyntax => Self {
                import_neighbor: false,
                reverse_dependency: false,
                caller: false,
            },
            ContextSignalPolicy::ImportNeighbor => Self {
                import_neighbor: true,
                reverse_dependency: false,
                caller: false,
            },
            ContextSignalPolicy::ReverseDependency => Self {
                import_neighbor: false,
                reverse_dependency: true,
                caller: false,
            },
            ContextSignalPolicy::HighConfidenceCaller => Self {
                import_neighbor: false,
                reverse_dependency: false,
                caller: true,
            },
        }
    }
}

pub(super) fn qualified_symbol_match(
    concept: &str,
    name: &str,
    parent: Option<&str>,
    signature: Option<&str>,
) -> f64 {
    if !concept.contains(['.', ':']) {
        return 0.0;
    }
    let parts = concept
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .flat_map(identifier_words)
        .map(|part| part.to_ascii_lowercase())
        .filter(|part| part.chars().count() >= 2)
        .collect::<HashSet<_>>();
    if parts.len() < 2 {
        return 0.0;
    }
    let haystack = format!(
        "{} {} {}",
        name,
        parent.unwrap_or_default(),
        signature.unwrap_or_default()
    )
    .to_ascii_lowercase();
    f64::from(parts.iter().all(|part| haystack.contains(part)))
}

pub(super) fn record_query_hit(
    fusion: &mut HashMap<String, HashMap<String, f64>>,
    path: &str,
    fusion_key: &str,
    weight: f64,
    rank: usize,
) {
    if weight < MIN_CORROBORATED_QUERY_WEIGHT {
        return;
    }
    pub(super) const RRF_K: f64 = 60.0;
    let rank = f64::from(u32::try_from(rank).unwrap_or(u32::MAX));
    let score = weight * RRF_K / (RRF_K + rank + 1.0);
    fusion
        .entry(path.to_owned())
        .or_default()
        .entry(fusion_key.to_owned())
        .and_modify(|current| *current = current.max(score))
        .or_insert(score);
}

pub(super) fn apply_query_fusion(
    candidates: &mut [Candidate],
    fusion: &HashMap<String, HashMap<String, f64>>,
) {
    for candidate in candidates {
        let Some(matches) = fusion.get(&candidate.path) else {
            continue;
        };
        if matches.len() > 1 {
            let total = matches.values().sum::<f64>();
            let strongest = matches.values().copied().fold(0.0, f64::max);
            candidate.path_score += (total - strongest).min(0.2);
            if !candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "multi-query")
            {
                candidate.match_kinds.push("multi-query".into());
            }
        }
    }
}

pub(super) fn annotate_candidate(
    mut candidate: Candidate,
    query: &ContextQuery,
    channel: &str,
    rank: usize,
) -> Candidate {
    for facet in query.facet_names() {
        candidate = candidate.facet(facet, &query.fusion_key);
    }
    candidate.channel(channel, rank)
}

pub(super) fn low_cardinality_exact_query(queries: &[ContextQuery]) -> bool {
    queries
        .iter()
        .filter(|query| query.has_facet(FacetKind::ExactAtom))
        .map(|query| query.fusion_key.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        == 1
}

pub(super) fn corroborated_import_symbol<'a>(
    symbols: Vec<SymbolRecord>,
    queries: &'a [ContextQuery],
    seed_concepts: &BTreeSet<String>,
) -> Option<(SymbolRecord, &'a ContextQuery, f64)> {
    let mut best: Option<(usize, usize, usize, SymbolRecord, &ContextQuery, f64)> = None;
    for (query_rank, query) in queries.iter().enumerate() {
        if query.concept_weight < MIN_CORROBORATED_QUERY_WEIGHT
            || !seed_concepts.contains(&query.fusion_key)
            || !(query.has_facet(FacetKind::ExactAtom)
                || query.has_facet(FacetKind::Symbol)
                || query.has_facet(FacetKind::Configuration))
        {
            continue;
        }
        for symbol in &symbols {
            let exact = symbol.name.eq_ignore_ascii_case(&query.value);
            let qualified = qualified_symbol_match(
                &query.fusion_key,
                &symbol.name,
                symbol.parent.as_deref(),
                symbol.signature.as_deref(),
            ) > 0.0;
            if !exact && !qualified {
                continue;
            }
            let class = usize::from(qualified) * 2 + usize::from(exact);
            let evidence = f64::from(exact) + f64::from(qualified) * 1.5;
            let candidate = (
                class,
                usize::MAX - query_rank,
                usize::MAX - symbol.start_line,
                symbol.clone(),
                query,
                evidence,
            );
            if best.as_ref().is_none_or(|current| {
                (candidate.0, candidate.1, candidate.2) > (current.0, current.1, current.2)
            }) {
                best = Some(candidate);
            }
        }
    }
    best.map(|(_, _, _, symbol, query, evidence)| (symbol, query, evidence))
}

pub(super) fn import_seed_paths(
    candidates: &[Candidate],
    queries: &[ContextQuery],
    tokenizer: crate::tokens::Tokenizer,
) -> Vec<(String, BTreeSet<String>)> {
    if low_cardinality_exact_query(queries) {
        return Vec::new();
    }
    let mut paths = BTreeMap::<String, (f64, BTreeSet<String>)>::new();
    for candidate in candidates {
        if candidate.concept_weight < MIN_CORROBORATED_QUERY_WEIGHT || candidate.concepts.is_empty()
        {
            continue;
        }
        let token_count = candidate.token_count_with(tokenizer).max(1);
        let score = candidate.score(&ranking::Weights::default(), token_count);
        let entry = paths
            .entry(candidate.path.clone())
            .or_insert_with(|| (score, BTreeSet::new()));
        entry.0 = entry.0.max(score);
        entry.1.extend(candidate.concepts.iter().cloned());
    }
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort_by(|left, right| {
        right
            .1
            .0
            .total_cmp(&left.1.0)
            .then_with(|| left.0.cmp(&right.0))
    });
    paths
        .into_iter()
        .map(|(path, (_, concepts))| (path, concepts))
        .collect()
}

pub(super) struct ImportExpansion<'a> {
    pub(super) session: &'a IndexReadSnapshot,
    pub(super) request: &'a ContextRequest,
    pub(super) queries: &'a [ContextQuery],
    pub(super) terms: &'a [String],
    pub(super) changed_paths: &'a HashSet<String>,
    pub(super) cancellation: &'a CancellationToken,
}

pub(super) fn task_mentions_language(task: &str, language: &str) -> bool {
    task.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .any(|word| {
            if language == "go" {
                word == "Go" || word.eq_ignore_ascii_case("golang")
            } else {
                word.eq_ignore_ascii_case(language)
            }
        })
}
use super::*;
