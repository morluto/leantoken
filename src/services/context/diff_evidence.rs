#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DiffEvidenceMode {
    WorkingTree,
    ImmutableRange,
}

pub(super) struct DiffEvidenceInput<'a> {
    pub(super) session: &'a IndexReadSnapshot,
    pub(super) request: &'a ContextRequest,
    pub(super) scope: &'a DiffScopeReceipt,
    pub(super) workflow: ContextWorkflow,
    pub(super) policy: &'a ContextPolicy,
    pub(super) mode: DiffEvidenceMode,
    pub(super) cancellation: &'a CancellationToken,
}

impl Services {
    pub(super) fn build_diff_evidence(
        &self,
        input: DiffEvidenceInput<'_>,
    ) -> Result<DiffEvidenceReceipt> {
        let DiffEvidenceInput {
            session,
            request,
            scope,
            workflow,
            policy,
            mode,
            cancellation,
        } = input;
        let mut changed_symbols = Vec::new();
        let mut relationships = BTreeSet::new();
        let mut gaps = Vec::new();
        let changed_hunks = if let Some(base_revision) = &scope.base_revision {
            let head_revision = match mode {
                DiffEvidenceMode::ImmutableRange => {
                    Some(scope.head_revision.as_deref().ok_or_else(|| {
                        Error::OperationFailure("immutable diff scope has no head revision".into())
                    })?)
                }
                DiffEvidenceMode::WorkingTree => None,
            };
            let mut hunks = git_diff_hunks_scoped(
                &self.config.root,
                base_revision,
                head_revision,
                &scope.changed_paths,
                MAX_DIFF_EVIDENCE_SYMBOLS + 1,
            )?;
            if hunks.len() > MAX_DIFF_EVIDENCE_SYMBOLS {
                gaps.push("changed_hunk_evidence_truncated".into());
                hunks.truncate(MAX_DIFF_EVIDENCE_SYMBOLS);
            }
            hunks
                .into_iter()
                .map(|hunk| DiffHunkEvidence {
                    path: hunk.path,
                    start_line: hunk.start_line,
                    end_line: hunk.end_line,
                })
                .collect::<Vec<_>>()
        } else {
            gaps.push("hunk_ranges_unavailable_for_explicit_paths".into());
            Vec::new()
        };
        let scoped_paths = scope
            .changed_paths
            .iter()
            .take(MAX_DIFF_EVIDENCE_PATHS)
            .collect::<Vec<_>>();
        if scope.changed_paths.len() > scoped_paths.len() {
            gaps.push("changed_path_evidence_truncated".into());
        }

        for path in &scoped_paths {
            check_cancelled(cancellation)?;
            let Some(file) = session.find_file(path)? else {
                gaps.push(format!("{path}:not_indexed_or_deleted"));
                continue;
            };
            if !file.structurally_complete {
                gaps.push(format!("{path}:structural_coverage_incomplete"));
            }
            let symbols = session.get_symbols_for_file(file.id, MAX_DIFF_EVIDENCE_SYMBOLS)?;
            let path_hunks = changed_hunks
                .iter()
                .filter(|hunk| hunk.path == **path)
                .collect::<Vec<_>>();
            let symbols = symbols
                .into_iter()
                .filter(|symbol| {
                    path_hunks.is_empty()
                        || path_hunks.iter().any(|hunk| {
                            hunk.start_line <= hunk.end_line
                                && symbol.start_line <= hunk.end_line
                                && symbol.end_line >= hunk.start_line
                        })
                })
                .collect::<Vec<_>>();
            if symbols.is_empty() && file.structurally_complete {
                gaps.push(format!("{path}:no_indexed_definitions"));
            }
            for symbol in symbols {
                if changed_symbols.len() == MAX_DIFF_EVIDENCE_SYMBOLS {
                    gaps.push("changed_symbol_evidence_truncated".into());
                    break;
                }
                let mut references = session.search_references(
                    &symbol.name,
                    true,
                    MAX_REFERENCES_PER_CHANGED_SYMBOL + 1,
                )?;
                if references.len() > MAX_REFERENCES_PER_CHANGED_SYMBOL {
                    gaps.push(format!(
                        "{path}:{}:reference_evidence_truncated",
                        symbol.name
                    ));
                    references.truncate(MAX_REFERENCES_PER_CHANGED_SYMBOL);
                }
                for reference in references {
                    if reference.path != **path {
                        relationships.insert((
                            (**path).clone(),
                            reference.path,
                            "reference".to_owned(),
                        ));
                    }
                }
                changed_symbols.push(DiffSymbolEvidence {
                    path: (**path).clone(),
                    name: symbol.name,
                    kind: symbol.kind,
                    start_line: symbol.start_line,
                    end_line: symbol.end_line,
                });
            }
            for importer in session.affected_importers(&[(**path).clone()])? {
                if importer != **path {
                    relationships.insert(((**path).clone(), importer, "importer".to_owned()));
                }
            }
        }

        let mut cursor = None;
        let mut scanned_owner_test_files = 0;
        let mut owner_test_scan_truncated = false;
        loop {
            check_cancelled(cancellation)?;
            let page = session.list_files(512, cursor)?;
            let Some(last) = page.last() else {
                break;
            };
            cursor = Some(last.id);
            for file in page {
                if scanned_owner_test_files == MAX_OWNER_TEST_SCAN_FILES {
                    owner_test_scan_truncated = true;
                    break;
                }
                scanned_owner_test_files += 1;
                if let Some(changed_path) = owner_test_changed_path(&file.path, request) {
                    relationships.insert((changed_path, file.path, "test_name_match".to_owned()));
                }
            }
            if owner_test_scan_truncated {
                break;
            }
        }
        if owner_test_scan_truncated {
            gaps.push("owner_test_scan_truncated".into());
        }

        let semantic_change = if workflow == ContextWorkflow::Review && !policy.is_plan() {
            let semantic_paths = scoped_paths
                .iter()
                .map(|path| (*path).clone())
                .collect::<Vec<_>>();
            let mut semantic = if mode == DiffEvidenceMode::ImmutableRange {
                let base_revision = scope.base_revision.as_deref().ok_or_else(|| {
                    Error::OperationFailure("immutable diff scope has no base revision".into())
                })?;
                let head_revision = scope.head_revision.as_deref().ok_or_else(|| {
                    Error::OperationFailure("immutable diff scope has no head revision".into())
                })?;
                classify_revision_changes(
                    &self.config.root,
                    base_revision,
                    head_revision,
                    &semantic_paths,
                    usize::try_from(self.config.max_file_bytes).unwrap_or(usize::MAX),
                    MAX_DIFF_EVIDENCE_SYMBOLS,
                )
            } else {
                DiffSemanticChangeReceipt {
                    symbol_changes: Vec::new(),
                    configuration_changes: Vec::new(),
                    owner_tests: Vec::new(),
                    gaps: vec!["semantic_change_requires_immutable_range".into()],
                }
            };
            if scope.changed_paths.len() > semantic_paths.len() {
                semantic
                    .gaps
                    .push("semantic_changed_paths_truncated".into());
            }
            if owner_test_scan_truncated {
                semantic.gaps.push("owner_test_scan_truncated".into());
            }
            semantic.owner_tests = owner_test_coverage(
                &scoped_paths,
                &relationships,
                owner_test_scan_truncated,
                &mut semantic.gaps,
            );
            semantic.gaps.sort();
            semantic.gaps.dedup();
            Some(semantic)
        } else {
            None
        };
        let relationship_count = relationships.len();
        let related_paths = relationships
            .into_iter()
            .take(MAX_DIFF_EVIDENCE_RELATIONSHIPS)
            .map(|(changed_path, related_path, signal)| DiffRelatedPath {
                changed_path,
                related_path,
                signal,
            })
            .collect();
        if relationship_count > MAX_DIFF_EVIDENCE_RELATIONSHIPS {
            gaps.push("related_path_evidence_truncated".into());
        }
        gaps.sort();
        gaps.dedup();

        Ok(DiffEvidenceReceipt {
            changed_hunks,
            changed_symbols,
            related_paths,
            semantic_change,
            gaps,
        })
    }
}
use super::*;
