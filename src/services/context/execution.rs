impl Services {
    #[allow(clippy::cognitive_complexity, clippy::too_many_arguments)]
    fn context_sync(
        &self,
        mut request: ContextRequest,
        workflow: ContextWorkflow,
        handoff: Option<HandoffManifestRequest>,
        options: ServiceCallOptions,
        workflow_evidence: WorkflowEvidence,
        cancellation: &CancellationToken,
        diagnostics: CandidateDiagnostics,
        signals: ContextSignals,
    ) -> Result<(ContextEvaluation, Option<usize>)> {
        check_cancelled(cancellation)?;
        self.validate_call_options(options)?;
        let response_profile = response::effective_context_response_profile(&request, options)?;
        request.verbose_diagnostics = response_profile == ContextResponseProfile::Explain;
        self.validate_context_request(&request, handoff.as_ref())?;
        self.validate_workflow_evidence(&workflow_evidence)?;
        request.changed_paths = request
            .changed_paths
            .iter()
            .map(|path| normalize_relative(path))
            .collect::<Result<Vec<_>>>()?;
        let (diff_scope, mut changed_paths, working_tree_state_available) =
            self.resolve_diff_scope(&request)?;
        let working_tree_state = if !working_tree_state_available {
            HandoffWorkingTreeState::Unknown
        } else if changed_paths.is_empty() {
            HandoffWorkingTreeState::Clean
        } else {
            HandoffWorkingTreeState::Dirty
        };
        let working_tree_paths = changed_paths.iter().cloned().collect::<Vec<_>>();
        let mut scoped_request = request.clone();
        if let Some(scope) = &diff_scope {
            scoped_request.changed_paths = scope.changed_paths.clone();
        }
        if let Some(ref scope) = diff_scope {
            changed_paths.extend(scope.changed_paths.iter().cloned());
        }
        let path_filter = PathFilter::new(&request.include_paths, &request.exclude_paths)?;
        let strict_changed_paths = request.strict_changed_paths.then(|| {
            scoped_request
                .changed_paths
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>()
        });
        self.consistent(|session, generation| {
            let facet_plan = facets::plan_with_workflow_evidence(
                &request.task,
                &workflow_evidence,
                MAX_CONTEXT_QUERIES,
            );
            let queries = facet_plan.queries;
            let mut phases = ContextPhaseTracker::new(diagnostics, generation);
            let candidate_generation_started = phases.timer();
            phases.counters.queries_planned = queries.len();
            phases.counters.queries_executed = queries
                .iter()
                .filter(|query| !query.is_generic_test_path_prior())
                .count();
            let terms = queries
                .iter()
                .map(|query| query.value.clone())
                .collect::<Vec<_>>();
            let path_scorer = ContextPathScorer::new(&terms, &request.task);
            let mut batch = CandidateBatch::default();
            let constraint_expansion = self.append_constraint_candidates(
                ConstraintCandidateExpansion {
                    session,
                    request: &scoped_request,
                    queries: &queries,
                    path_scorer: &path_scorer,
                    cancellation,
                },
                &mut batch.candidates,
                &mut phases,
            )?;
            batch.coverage = constraint_expansion.coverage;
            batch.warnings = self.append_focus_candidates(
                FocusExpansion {
                    session,
                    request: &scoped_request,
                    queries: &queries,
                    path_scorer: &path_scorer,
                    resolutions: &constraint_expansion.focus_paths,
                    cancellation,
                },
                &mut batch.candidates,
                &mut phases,
            )?;

            // Workflow words such as `test` are useful path priors but terrible
            // retrieval queries: nearly every test function becomes a high-
            // scoring symbol candidate. Keep them out of candidate generation.
            for query in queries
                .iter()
                .filter(|query| !query.is_generic_test_path_prior())
            {
                self.append_query_candidates(
                    QueryCandidateExpansion {
                        session,
                        request: &request,
                        query,
                        path_filter: &path_filter,
                        strict_changed_paths: strict_changed_paths.as_ref(),
                        changed_paths: &changed_paths,
                        path_scorer: &path_scorer,
                        cancellation,
                        signals,
                    },
                    &mut batch,
                    &mut phases,
                )?;
            }

            apply_query_fusion(&mut batch.candidates, &batch.query_fusion);
            let resolved_workflow = resolve_context_workflow(workflow, &request.task);
            let workflow_started = phases.timer();
            let (workflow_receipt, workflow_path_excluded) = self.append_workflow_candidates(
                session,
                &scoped_request,
                resolved_workflow,
                cancellation,
                &mut batch.candidates,
            )?;
            batch.workflow_receipt = workflow_receipt;
            batch
                .path_excluded_candidates
                .extend(workflow_path_excluded);

            signals
                .import_neighbor
                .then(|| {
                    self.append_import_symbol_candidates(
                        ImportExpansion {
                            session,
                            request: &request,
                            queries: &queries,
                            terms: &terms,
                            changed_paths: &changed_paths,
                            cancellation,
                        },
                        &mut batch.candidates,
                    )
                })
                .transpose()?;
            signals
                .reverse_dependency
                .then(|| {
                    self.apply_reverse_dependency_boost(session, &queries, &mut batch.candidates)
                })
                .transpose()?;
            if let Some(paths) = &strict_changed_paths {
                batch
                    .candidates
                    .retain(|candidate| paths.contains(candidate.path.as_str()));
                batch
                    .path_excluded_candidates
                    .retain(|path| paths.contains(path.as_str()));
            }
            if let Some(started) = workflow_started {
                phases.timings.workflow_generation_ms = started.elapsed().as_secs_f64() * 1_000.0;
            }
            if let Some(started) = candidate_generation_started {
                phases.timings.candidate_generation_ms = started.elapsed().as_secs_f64() * 1_000.0;
            }

            self.finalize_context_pipeline(
                ContextFinalization {
                    session,
                    request: &request,
                    scoped_request: &scoped_request,
                    handoff: handoff.as_ref(),
                    options,
                    response_profile,
                    cancellation,
                    diagnostics,
                    generation,
                    diff_scope: diff_scope.as_ref(),
                    working_tree_state,
                    working_tree_paths: &working_tree_paths,
                    resolved_workflow,
                },
                batch,
                phases,
            )
        })
    }
}
