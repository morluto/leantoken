impl Services {
    fn append_workflow_candidates(
        &self,
        session: &ReadSession,
        request: &ContextRequest,
        workflow: ContextWorkflow,
        cancellation: &CancellationToken,
        candidates: &mut Vec<Candidate>,
    ) -> Result<(Option<WorkflowReceipt>, Vec<String>)> {
        if !matches!(
            workflow,
            ContextWorkflow::Contribution | ContextWorkflow::Review
        ) {
            return Ok((None, Vec::new()));
        }

        let mut matches = Vec::new();
        let path_filter = PathFilter::new(&request.include_paths, &request.exclude_paths)?;
        let mut path_excluded = Vec::new();
        let mut cursor = None;
        let mut scanned_files = 0;
        let mut scan_truncated = false;
        loop {
            check_cancelled(cancellation)?;
            let page = session.list_files(512, cursor)?;
            let Some(last) = page.last() else {
                break;
            };
            cursor = Some(last.id);
            for file in page {
                if scanned_files == MAX_WORKFLOW_SCAN_FILES {
                    scan_truncated = true;
                    break;
                }
                scanned_files += 1;
                if let Some((score, family)) = workflow_path_role(&file.path, request) {
                    if path_filter.allows(&file.path) {
                        matches.push((file, score, family));
                    } else {
                        path_excluded.push(file.path);
                    }
                }
            }
            if scan_truncated {
                break;
            }
        }
        let count = |family| {
            matches
                .iter()
                .filter(|(_, _, candidate_family)| *candidate_family == family)
                .count()
        };
        let guidance_candidates = count("guidance");
        let template_candidates = count("template");
        let validation_candidates = count("validation");
        let owner_test_candidates = count("owner_test");
        let mut missing_families = Vec::new();
        for (family, candidates) in [
            ("guidance", guidance_candidates),
            ("template", template_candidates),
            ("validation", validation_candidates),
            ("owner_test", owner_test_candidates),
        ] {
            if candidates == 0 {
                missing_families.push(family.to_owned());
            }
        }
        if scan_truncated {
            missing_families.push("repository_scan_truncated".to_owned());
        }
        matches.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.path.cmp(&right.0.path))
        });
        matches.truncate(24);

        let excerpt_requests = matches
            .iter()
            .map(|(file, _, _)| StoredExcerptRequest {
                file_id: file.id,
                desired_start_line: 1,
                desired_end_line: 80,
                required_start_line: 1,
                required_end_line: 1,
                max_lines: 80,
            })
            .collect::<Vec<_>>();
        for ((file, score, family), excerpt) in matches
            .into_iter()
            .zip(self.stored_excerpts(session, &excerpt_requests)?)
        {
            let Some(excerpt) = excerpt else { continue };
            candidates.push(
                Candidate::new(
                    file.path,
                    excerpt.start_line,
                    excerpt.end_line,
                    excerpt.content,
                )
                .match_kind(format!("workflow_{family}"))
                .representation("workflow")
                .path_score(score)
                .focus_boost(score),
            );
        }
        Ok((
            Some(WorkflowReceipt {
                guidance_candidates,
                template_candidates,
                validation_candidates,
                owner_test_candidates,
                missing_families,
            }),
            path_excluded,
        ))
    }
}
