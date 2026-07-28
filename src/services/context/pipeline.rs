#[derive(Clone, Copy)]
struct ContextSignals {
    import_neighbor: bool,
    reverse_dependency: bool,
    caller: bool,
}

#[derive(Default)]
struct CandidateBatch {
    candidates: Vec<Candidate>,
    path_excluded_candidates: Vec<String>,
    query_fusion: HashMap<String, HashMap<String, f64>>,
    coverage: ContextCoverageReceipt,
    warnings: Vec<String>,
    workflow_receipt: Option<WorkflowReceipt>,
}

#[derive(Clone, Copy)]
struct QueryCandidateExpansion<'a> {
    session: &'a ReadSession,
    request: &'a ContextRequest,
    query: &'a ContextQuery,
    path_filter: &'a PathFilter,
    strict_changed_paths: Option<&'a HashSet<&'a str>>,
    changed_paths: &'a HashSet<String>,
    path_scorer: &'a ContextPathScorer,
    cancellation: &'a CancellationToken,
    signals: ContextSignals,
}

struct ContextFinalization<'a> {
    session: &'a ReadSession,
    request: &'a ContextRequest,
    scoped_request: &'a ContextRequest,
    handoff: Option<&'a HandoffManifestRequest>,
    options: ServiceCallOptions,
    cancellation: &'a CancellationToken,
    diagnostics: CandidateDiagnostics,
    generation: u64,
    diff_scope: Option<&'a DiffScopeReceipt>,
    working_tree_state: HandoffWorkingTreeState,
    working_tree_paths: &'a [String],
    resolved_workflow: ContextWorkflow,
}

struct AccountedContextResponse {
    response: ContextResponse,
    baseline_source_tokens: Option<usize>,
    operation: TokenAccountingOperation,
}

struct ContextExecution {
    handoff: Option<HandoffManifestRequest>,
    workflow: ContextWorkflow,
    workflow_evidence: WorkflowEvidence,
}

impl ContextExecution {
    fn new(workflow: ContextWorkflow) -> Self {
        Self {
            handoff: None,
            workflow,
            workflow_evidence: WorkflowEvidence::default(),
        }
    }

    fn with_handoff(mut self, handoff: HandoffManifestRequest) -> Self {
        self.handoff = Some(handoff);
        self
    }

    fn with_workflow_evidence(mut self, workflow_evidence: WorkflowEvidence) -> Self {
        self.workflow_evidence = workflow_evidence;
        self
    }
}

impl ContextSignals {
    const PRODUCTION: Self = Self {
        import_neighbor: true,
        reverse_dependency: false,
        caller: true,
    };

    const fn evaluation(policy: ContextSignalPolicy) -> Self {
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

fn qualified_symbol_match(
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

fn record_query_hit(
    fusion: &mut HashMap<String, HashMap<String, f64>>,
    path: &str,
    fusion_key: &str,
    weight: f64,
    rank: usize,
) {
    if weight < MIN_CORROBORATED_QUERY_WEIGHT {
        return;
    }
    const RRF_K: f64 = 60.0;
    #[allow(clippy::cast_precision_loss)]
    let score = weight * RRF_K / (RRF_K + rank as f64 + 1.0);
    fusion
        .entry(path.to_owned())
        .or_default()
        .entry(fusion_key.to_owned())
        .and_modify(|current| *current = current.max(score))
        .or_insert(score);
}

fn apply_query_fusion(
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

fn annotate_candidate(
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

fn low_cardinality_exact_query(queries: &[ContextQuery]) -> bool {
    queries
        .iter()
        .filter(|query| query.has_facet(FacetKind::ExactAtom))
        .map(|query| query.fusion_key.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        == 1
}

fn corroborated_import_symbol<'a>(
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

fn import_seed_paths(
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

struct ImportExpansion<'a> {
    session: &'a ReadSession,
    request: &'a ContextRequest,
    queries: &'a [ContextQuery],
    terms: &'a [String],
    changed_paths: &'a HashSet<String>,
    cancellation: &'a CancellationToken,
}

fn task_mentions_language(task: &str, language: &str) -> bool {
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
