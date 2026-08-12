impl Services {
    pub(in crate::services::context) fn file_change_boost(
        file_generation: Option<u64>,
        path: &str,
        changed_paths: &HashSet<String>,
        prior_generation: Option<u64>,
    ) -> f64 {
        let mut boost = 0.0;

        if let Some(prior) = prior_generation
            && file_generation.is_some_and(|generation| generation > prior)
        {
            boost += 1.0;
        }

        if changed_paths.contains(path) {
            boost += 1.0;
        }

        boost
    }

    pub(in crate::services::context) fn append_import_symbol_candidates(
        &self,
        expansion: ImportExpansion<'_>,
        candidates: &mut Vec<Candidate>,
    ) -> Result<()> {
        let seed_paths = import_seed_paths(candidates, expansion.queries, self.config.tokenizer);
        let requested_paths = seed_paths
            .iter()
            .take(24)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let targets =
            expansion
                .session
                .import_symbol_targets(&requested_paths, 32, MAX_IMPORT_SYMBOLS)?;
        let path_filter = PathFilter::new(
            &expansion.request.include_paths,
            &expansion.request.exclude_paths,
        )?;
        let mut pending = Vec::new();
        for target in targets {
            check_cancelled(expansion.cancellation)?;
            let Some((_, seed_concepts)) = seed_paths.get(target.seed_index) else {
                continue;
            };
            let target_path = &target.target_file.path;
            if !path_filter.allows(target_path) {
                continue;
            }
            let Some((symbol, query, exact)) =
                corroborated_import_symbol(target.symbols, expansion.queries, seed_concepts)
            else {
                continue;
            };
            pending.push((target.target_file, symbol, query.clone(), exact));
        }
        let excerpt_requests = pending
            .iter()
            .map(|(target_file, symbol, _, _)| AdaptiveExcerptRequest {
                file_id: target_file.id,
                declaration_start: symbol.start_line,
                declaration_end: symbol.end_line,
                matched_line: symbol.start_line,
                token_budget: excerpt_budget(
                    expansion.request.token_budget,
                    ContextExcerptKind::ImportSymbol,
                ),
            })
            .collect::<Vec<_>>();
        let excerpts = self.adaptive_context_excerpts(expansion.session, &excerpt_requests)?;
        let mut neighbor_count = 0usize;
        let mut neighbor_ranges = BTreeSet::new();
        for ((target_file, symbol, query, exact), excerpt) in pending.into_iter().zip(excerpts) {
            check_cancelled(expansion.cancellation)?;
            let Some(excerpt) = excerpt else { continue };
            let target_path = target_file.path;
            if !neighbor_ranges.insert((target_path.clone(), excerpt.start_line, excerpt.end_line))
            {
                continue;
            }
            let change_boost = Self::file_change_boost(
                Some(target_file.generation),
                &target_path,
                expansion.changed_paths,
                expansion.request.prior_repository_generation,
            );
            let candidate = Candidate::new(
                &target_path,
                excerpt.start_line,
                excerpt.end_line,
                excerpt.content,
            )
            .match_kind("import")
            .match_kind("symbol")
            .concept(&query.fusion_key, query.concept_weight)
            .representation("import_symbol")
            .symbol_name(symbol.name)
            .exact(exact)
            .symbol(1.0)
            .path_score(context_path_score(
                &target_path,
                expansion.terms,
                &expansion.request.task,
            ))
            .import_boost(1.0)
            .change_boost(change_boost);
            candidates.push(annotate_candidate(
                candidate,
                &query,
                "import_symbol",
                neighbor_count,
            ));
            neighbor_count += 1;
            if neighbor_count >= 24 {
                break;
            }
        }
        Ok(())
    }

    pub(in crate::services::context) fn apply_reverse_dependency_boost(
        &self,
        session: &RepositoryGeneration,
        queries: &[ContextQuery],
        candidates: &mut [Candidate],
    ) -> Result<()> {
        let seed_paths = import_seed_paths(candidates, queries, self.config.tokenizer)
            .into_iter()
            .take(24)
            .map(|(path, _)| path)
            .collect::<Vec<_>>();
        let importers = session
            .affected_importers(&seed_paths)?
            .into_iter()
            .collect::<HashSet<_>>();
        for candidate in candidates {
            if importers.contains(&candidate.path) {
                if !candidate
                    .match_kinds
                    .iter()
                    .any(|kind| kind == "reverse-import")
                {
                    candidate.match_kinds.push("reverse-import".into());
                }
                candidate.import_boost = candidate.import_boost.max(1.0);
            }
        }
        Ok(())
    }
}
use super::*;
