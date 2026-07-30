impl Services {
    pub(super) fn resolve_diff_scope(
        &self,
        request: &ContextRequest,
    ) -> Result<(Option<DiffScopeReceipt>, HashSet<String>, bool)> {
        let has_base = request
            .base_revision
            .as_deref()
            .is_some_and(|rev| !rev.trim().is_empty());
        let has_paths = !request.changed_paths.is_empty();
        let revision = request
            .base_revision
            .as_deref()
            .filter(|revision| !revision.trim().is_empty());
        let immutable_range = revision.map(parse_revision_range).transpose()?.flatten();
        let explicit_hard_scope = has_paths && request.strict_changed_paths;
        let git_result = match (revision, immutable_range) {
            (Some(_), Some((base, head))) if explicit_hard_scope => {
                Some(git_diff_identity(&self.config.root, base, Some(head))?)
            }
            (Some(revision), None) if explicit_hard_scope => {
                Some(git_diff_identity(&self.config.root, revision, None)?)
            }
            (Some(_), Some((base, head))) => Some(git_diff_paths_between(
                &self.config.root,
                base,
                head,
                MAX_DIFF_CHANGED_PATHS,
            )?),
            (Some(revision), None) => Some(git_diff_paths(
                &self.config.root,
                revision,
                MAX_DIFF_CHANGED_PATHS,
            )?),
            (None, None) => None,
            (None, Some(_)) => unreachable!("a range comes from a revision"),
        };
        let working_tree_status = git_working_tree_status(&self.config.root, GIT_CHANGED_PATHS_MAX);
        if !working_tree_status.available {
            tracing::debug!("working-tree signal unavailable");
        }
        let working_tree_paths = working_tree_status.changed_paths;
        let working_tree_state_available = working_tree_status.available;
        if !has_base && !has_paths && !request.strict_changed_paths {
            return Ok((None, working_tree_paths, working_tree_state_available));
        }
        if let Some(git_result) = git_result {
            let mut changed_paths = request.changed_paths.clone();
            if !explicit_hard_scope {
                let mut resolved_paths = git_result.changed_paths;
                if immutable_range.is_none() {
                    resolved_paths.extend(working_tree_paths.iter().cloned());
                }
                resolved_paths.sort();
                resolved_paths.dedup();
                for path in resolved_paths {
                    if changed_paths.len() == MAX_DIFF_CHANGED_PATHS {
                        break;
                    }
                    if !changed_paths.contains(&path) {
                        changed_paths.push(path);
                    }
                }
            }
            changed_paths.sort();
            changed_paths.dedup();
            return Ok((
                Some(DiffScopeReceipt {
                    base_revision: Some(git_result.base_revision),
                    head_revision: Some(git_result.head_revision),
                    changed_paths,
                    indexed_changed_paths: 0,
                    evidence: None,
                }),
                working_tree_paths,
                working_tree_state_available,
            ));
        }
        let mut resolved_paths = if has_paths {
            request.changed_paths.clone()
        } else {
            working_tree_paths.iter().cloned().collect::<Vec<_>>()
        };
        resolved_paths.sort();
        resolved_paths.dedup();
        Ok((
            Some(DiffScopeReceipt {
                base_revision: None,
                head_revision: None,
                changed_paths: resolved_paths,
                indexed_changed_paths: 0,
                evidence: None,
            }),
            working_tree_paths,
            working_tree_state_available,
        ))
    }
}
use super::*;
