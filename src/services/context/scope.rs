use crate::repository::GitWorkingTreeStatus;

pub(super) struct DiffScopeResolution {
    pub(super) diff_scope: Option<DiffScopeReceipt>,
    pub(super) working_tree: GitWorkingTreeStatus,
}

impl Services {
    pub(super) fn resolve_diff_scope(
        &self,
        request: &ContextRequest,
        revision: Option<&ContextRevision>,
    ) -> Result<DiffScopeResolution> {
        let has_base = revision.is_some();
        let has_paths = !request.changed_paths.is_empty();
        let explicit_hard_scope = has_paths && request.strict_changed_paths;
        let git_result = match revision {
            Some(ContextRevision::Range { base, head }) if explicit_hard_scope => {
                Some(git_diff_identity(&self.config.root, base, Some(head))?)
            }
            Some(ContextRevision::Single(revision)) if explicit_hard_scope => {
                Some(git_diff_identity(&self.config.root, revision, None)?)
            }
            Some(ContextRevision::Range { base, head }) => Some(git_diff_paths_between(
                &self.config.root,
                base,
                head,
                MAX_DIFF_CHANGED_PATHS,
            )?),
            Some(ContextRevision::Single(revision)) => Some(git_diff_paths(
                &self.config.root,
                revision,
                MAX_DIFF_CHANGED_PATHS,
            )?),
            None => None,
        };
        let working_tree_status = git_working_tree_status(&self.config.root, GIT_CHANGED_PATHS_MAX);
        if !working_tree_status.is_available() {
            tracing::debug!("working-tree signal unavailable");
        }
        if !has_base && !has_paths && !request.strict_changed_paths {
            return Ok(DiffScopeResolution {
                diff_scope: None,
                working_tree: working_tree_status,
            });
        }
        if let Some(git_result) = git_result {
            let mut changed_paths = request.changed_paths.clone();
            if !explicit_hard_scope {
                let mut resolved_paths = git_result.changed_paths;
                if !revision.is_some_and(ContextRevision::is_range) {
                    resolved_paths.extend(working_tree_status.changed_paths.iter().cloned());
                }
                resolved_paths.sort();
                resolved_paths.dedup();
                for path in resolved_paths {
                    if changed_paths.len() == MAX_DIFF_CHANGED_PATHS {
                        tracing::warn!(
                            changed_paths = MAX_DIFF_CHANGED_PATHS,
                            "context diff scope truncated at {} entries;                             the change set has more paths than the bound",
                            MAX_DIFF_CHANGED_PATHS,
                        );
                        break;
                    }
                    if !changed_paths.contains(&path) {
                        changed_paths.push(path);
                    }
                }
            }
            changed_paths.sort();
            changed_paths.dedup();
            return Ok(DiffScopeResolution {
                diff_scope: Some(DiffScopeReceipt {
                    base_revision: Some(git_result.base_revision),
                    head_revision: Some(git_result.head_revision),
                    changed_paths,
                    indexed_changed_paths: 0,
                    evidence: None,
                }),
                working_tree: working_tree_status,
            });
        }
        let mut resolved_paths = if has_paths {
            request.changed_paths.clone()
        } else {
            working_tree_status
                .changed_paths
                .iter()
                .cloned()
                .collect::<Vec<_>>()
        };
        resolved_paths.sort();
        resolved_paths.dedup();
        Ok(DiffScopeResolution {
            diff_scope: Some(DiffScopeReceipt {
                base_revision: None,
                head_revision: None,
                changed_paths: resolved_paths,
                indexed_changed_paths: 0,
                evidence: None,
            }),
            working_tree: working_tree_status,
        })
    }
}
use super::*;
