impl Services {
    fn context_response_with_receipt_reserve(
        &self,
        response: &ContextResponse,
        request: &ContextRequest,
    ) -> Result<ContextResponse> {
        let mut sized = response.clone();
        if !request.plan_only {
            let receipt_id = request
                .receipt_id
                .clone()
                .unwrap_or_else(|| "rffffffffffffffff".into());
            let selected = sized.fragments.len();
            sized.meta.receipt_id = Some(receipt_id.clone());
            sized.meta.receipt_suppressed_exact = selected;
            sized.meta.receipt_suppressed_overlap = selected;
            sized.meta.receipt_near_duplicates = selected;
            sized.warnings.push(format!(
                "{selected} returned fragments are semantic near-duplicates of prior receipt evidence"
            ));
            sized
                .warnings
                .push("all selected evidence was already covered by the receipt".into());
            if let Some(manifest) = &mut sized.handoff_manifest {
                manifest.receipt_id = Some(receipt_id);
            }
        }
        set_routing_consistency(&mut sized, IndexConsistency::ReconcileWorkingTree);
        self.finalize_response(&mut sized)?;
        Ok(sized)
    }

    fn context_response_tokens_with_receipt_reserve(
        &self,
        response: &ContextResponse,
        request: &ContextRequest,
    ) -> Result<usize> {
        let sized = self.context_response_with_receipt_reserve(response, request)?;
        let budget = ResponseBudget::new(&self.config.tokenizer, usize::MAX);
        let serialized_tokens = budget.serialized_tokens(&sized)?;
        debug_assert_eq!(serialized_tokens, sized.meta.total_response_tokens);
        Ok(serialized_tokens)
    }

    fn context_response_fits(
        &self,
        response: &ContextResponse,
        request: &ContextRequest,
        max_response_tokens: usize,
    ) -> Result<bool> {
        let sized = self.context_response_with_receipt_reserve(response, request)?;
        ResponseBudget::new(&self.config.tokenizer, max_response_tokens)
            .fits(&sized)
            .map_err(Into::into)
    }

    fn refresh_context_omission_warning(response: &mut ContextResponse) {
        response.warnings.retain(|warning| {
            warning
                .strip_suffix(" omitted")
                .is_none_or(|count| count.parse::<usize>().is_err())
        });
        let omitted = response
            .omission_summary
            .path_excluded
            .saturating_add(response.omission_summary.known_hash)
            .saturating_add(response.omission_summary.budget_or_result_limit);
        if omitted > 0 {
            response.warnings.insert(0, format!("{omitted} omitted"));
        }
    }

    fn trim_context_selection(response: &mut ContextResponse, keep: usize) {
        let (removed, removed_tokens) = if let Some(plan) = &mut response.plan {
            let removed = plan.candidates.len().saturating_sub(keep);
            let removed_tokens = plan
                .candidates
                .iter()
                .skip(keep)
                .map(|candidate| candidate.estimated_tokens)
                .sum::<usize>();
            plan.candidates.truncate(keep);
            plan.estimated_source_tokens =
                plan.estimated_source_tokens.saturating_sub(removed_tokens);
            plan.result_complete &= removed == 0;
            (removed, removed_tokens)
        } else {
            let removed = response.fragments.len().saturating_sub(keep);
            let removed_tokens = response
                .fragments
                .iter()
                .skip(keep)
                .map(|fragment| fragment.token_count)
                .sum::<usize>();
            response.fragments.truncate(keep);
            response.receipt.fragment_hashes.truncate(keep);
            (removed, removed_tokens)
        };
        response.meta.source_tokens = response.meta.source_tokens.saturating_sub(removed_tokens);
        response.meta.emitted_tokens = response.meta.source_tokens;
        response.omission_summary.budget_or_result_limit = response
            .omission_summary
            .budget_or_result_limit
            .saturating_add(removed);
        Self::refresh_context_omission_warning(response);
    }

    fn fit_context_response(
        &self,
        response: &mut ContextResponse,
        request: &ContextRequest,
        max_response_tokens: usize,
    ) -> Result<()> {
        self.finalize_response(response)?;
        if self.context_response_fits(response, request, max_response_tokens)? {
            return Ok(());
        }

        response.omitted.clear();
        response.omission_summary.by_path.clear();
        response.omission_summary.by_language_or_file_type.clear();
        response.omission_summary.by_reason.clear();
        response.omission_summary.by_score_band.clear();
        if self.context_response_fits(response, request, max_response_tokens)? {
            self.finalize_response(response)?;
            return Ok(());
        }

        if let Some(scope) = &mut response.diff_scope {
            scope.evidence = None;
        }
        if self.context_response_fits(response, request, max_response_tokens)? {
            self.finalize_response(response)?;
            return Ok(());
        }

        response.routing = None;
        if self.context_response_fits(response, request, max_response_tokens)? {
            self.finalize_response(response)?;
            return Ok(());
        }

        if let Some(plan) = &mut response.plan {
            for candidate in &mut plan.candidates {
                candidate.reasons.clear();
            }
        }
        for fragment in &mut response.fragments {
            fragment.reason.clear();
        }
        if self.context_response_fits(response, request, max_response_tokens)? {
            self.finalize_response(response)?;
            return Ok(());
        }

        let can_reduce_selected = request.include_paths.is_empty()
            && request.must_include_paths.is_empty()
            && request.must_include_symbols.is_empty()
            && request.required_evidence.is_empty()
            && request.focus_paths.is_empty()
            && request.focus_symbols.is_empty()
            && !request.strict_focus_paths
            && request.minimum_fragments_per_focus_path.is_none()
            && request.base_revision.is_none()
            && request.changed_paths.is_empty()
            && !request.strict_changed_paths
            && response.handoff_manifest.is_none();
        if can_reduce_selected {
            let selected = response
                .plan
                .as_ref()
                .map_or(response.fragments.len(), |plan| plan.candidates.len());
            let omission_reserve = response
                .omission_summary
                .budget_or_result_limit
                .saturating_add(selected);
            let budget = ResponseBudget::new(&self.config.tokenizer, max_response_tokens);
            let keep = budget.largest_fitting_prefix(selected, |keep| {
                let mut candidate = response.clone();
                Self::trim_context_selection(&mut candidate, keep);
                candidate.omission_summary.budget_or_result_limit = omission_reserve;
                Self::refresh_context_omission_warning(&mut candidate);
                self.context_response_tokens_with_receipt_reserve(&candidate, request)
            })?;
            if let Some(keep) = keep {
                Self::trim_context_selection(response, keep);
                self.finalize_response(response)?;
                if self.context_response_fits(response, request, max_response_tokens)? {
                    return Ok(());
                }
                return Err(Error::InternalFailure(
                    "context prefix fitting violated its monotonic sizing reserve".into(),
                ));
            }
            Self::trim_context_selection(response, 0);
        }

        let minimum = self.context_response_tokens_with_receipt_reserve(response, request)?;
        Err(Error::RequestLimitExceeded {
            field: "max_response_tokens",
            requested: minimum,
            limit: max_response_tokens,
        })
    }

    fn finalize_context_pipeline(
        &self,
        finalization: ContextFinalization<'_>,
        batch: CandidateBatch,
        mut phases: ContextPhaseTracker,
    ) -> Result<(ContextEvaluation, Option<usize>)> {
        let ContextFinalization {
            session,
            request,
            scoped_request,
            handoff,
            options,
            cancellation,
            diagnostics,
            generation,
            diff_scope,
            working_tree_state,
            working_tree_paths,
            resolved_workflow,
        } = finalization;
        let CandidateBatch {
            candidates,
            path_excluded_candidates,
            coverage,
            warnings: focus_generation_warnings,
            workflow_receipt,
            ..
        } = batch;
        let ranking_started = phases.timer();
        let candidate_path_count = candidates
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        let generated_candidate_paths = if diagnostics == CandidateDiagnostics::Collect {
            candidates
                .iter()
                .map(|candidate| candidate.path.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        let generated_candidates = if diagnostics == CandidateDiagnostics::Collect {
            candidates
                .iter()
                .map(|candidate| {
                    let token_count = candidate.token_count_with(self.config.tokenizer).max(1);
                    ContextCandidateEvaluation {
                        path: candidate.path.clone(),
                        start_line: candidate.start_line,
                        end_line: candidate.end_line,
                        representation: candidate.representation.clone(),
                        match_kinds: candidate.match_kinds.clone(),
                        concepts: candidate.concepts.clone(),
                        concept_weight: candidate.concept_weight,
                        score: candidate.score(&ranking::Weights::default(), token_count),
                        token_count,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut response = ranking::select_with_tokenizer_and_context_exclusions(
            candidates,
            scoped_request,
            generation,
            self.config.tokenizer,
            &self.config.context_exclude_paths,
            &path_excluded_candidates,
        );
        let mut coverage = response::merge_selected_coverage(coverage, &mut response);
        let selected_paths = response::selected_paths(&response);
        self.finalize_strict_scope_coverage(
            session,
            scoped_request,
            &selected_paths,
            &mut coverage,
        )?;
        response.coverage = coverage;
        response.warnings.extend(focus_generation_warnings);
        response::append_coverage_warnings(&mut response);
        response.workflow = resolved_workflow;
        response.workflow_receipt = workflow_receipt;
        response.meta.freshness = self.freshness();
        response.meta.repository_id = self.repository_id();
        if let Some(scope) = diff_scope {
            let mut scope = scope.clone();
            let mut indexed = 0usize;
            for path in &scope.changed_paths {
                if session.find_file(path)?.is_some() {
                    indexed += 1;
                }
            }
            scope.indexed_changed_paths = indexed;
            scope.evidence = (!request.plan_only || request.verbose_diagnostics)
                .then(|| {
                    self.build_diff_evidence(
                        session,
                        scoped_request,
                        &scope,
                        resolved_workflow,
                        cancellation,
                    )
                })
                .transpose()?;
            response.routing =
                build_context_routing(request, &scope, candidate_path_count, &selected_paths);
            if let Some(routing) = &response.routing {
                let concentration = if routing.weakly_concentrated {
                    "; selected evidence is weakly concentrated"
                } else {
                    ""
                };
                response.warnings.push(format!(
                    "context spans {} changed paths across {} path groups{concentration}",
                    routing.changed_paths, routing.path_groups_total
                ));
            }
            response.diff_scope = Some(scope);
        }
        if let Some(handoff) = &handoff {
            let evidence = response
                .fragments
                .iter()
                .map(|fragment| HandoffEvidence {
                    path: fragment.path.clone(),
                    start_line: fragment.start_line,
                    end_line: fragment.end_line,
                    content_hash: fragment.content_hash.clone(),
                })
                .collect::<Vec<_>>();
            let resolved_head = response
                .diff_scope
                .as_ref()
                .and_then(|scope| scope.head_revision.clone());
            let (commit_revision, commit_revision_available) = if let Some(head) = resolved_head {
                (Some(head), true)
            } else {
                match git_head_revision(&self.config.root) {
                    Ok(head) => (Some(head), true),
                    Err(error) => {
                        tracing::debug!(%error, "handoff Git identity unavailable");
                        (None, false)
                    }
                }
            };
            response.handoff_manifest = Some(handoff::build(
                request,
                handoff,
                &response,
                evidence,
                HandoffProvenance {
                    commit_revision,
                    commit_revision_available,
                    working_tree_state: if commit_revision_available {
                        working_tree_state
                    } else {
                        HandoffWorkingTreeState::Unknown
                    },
                    working_tree_paths: working_tree_paths.to_vec(),
                },
            ));
        }
        let baseline_source_tokens = self.finalize_context_delivery(
            &mut response,
            response::ContextResponseFinalization {
                session,
                request,
                options,
                generation,
            },
        )?;
        if let Some(started) = ranking_started {
            phases.timings.ranking_finalize_ms = started.elapsed().as_secs_f64() * 1_000.0;
        }
        let (phases, timings, primitive_keys) = phases.finish(generated_candidates.len());
        Ok((
            ContextEvaluation {
                response,
                generated_candidate_paths,
                generated_candidates,
                phases,
                timings,
                primitive_keys,
            },
            baseline_source_tokens,
        ))
    }
}
