use crate::repository::{GitDiffResult, GitWorkingTreeStatus};

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
        let strict_automatic_scope = request.strict_changed_paths && !has_paths;
        let uses_working_tree_paths = revision.is_none_or(|revision| !revision.is_range());
        if strict_automatic_scope && uses_working_tree_paths {
            working_tree_status.require_complete()?;
        }
        if !has_base && !has_paths && !request.strict_changed_paths {
            return Ok(DiffScopeResolution {
                diff_scope: None,
                working_tree: working_tree_status,
            });
        }
        if let Some(git_result) = git_result {
            if strict_automatic_scope && !git_result.changed_paths_complete {
                return Err(changed_path_limit_error(git_result.changed_paths_limit));
            }
            let GitDiffResult {
                base_revision,
                head_revision,
                changed_paths: git_changed_paths,
                changed_paths_complete: mut scope_complete,
                changed_paths_limit: mut scope_limit,
            } = git_result;
            let mut changed_paths = request.changed_paths.clone();
            if !explicit_hard_scope {
                let mut resolved_paths = git_changed_paths;
                if !revision.is_some_and(ContextRevision::is_range) {
                    resolved_paths.extend(working_tree_status.changed_paths.iter().cloned());
                    if !working_tree_status.changed_paths_complete() {
                        scope_complete = false;
                        scope_limit = scope_limit.or(working_tree_status.changed_paths_limit());
                    }
                }
                resolved_paths.sort();
                resolved_paths.dedup();
                for path in resolved_paths {
                    if changed_paths.contains(&path) {
                        continue;
                    }
                    if changed_paths.len() == MAX_DIFF_CHANGED_PATHS {
                        scope_complete = false;
                        scope_limit = Some(MAX_DIFF_CHANGED_PATHS);
                        break;
                    }
                    changed_paths.push(path);
                }
            }
            changed_paths.sort();
            changed_paths.dedup();
            if strict_automatic_scope && !scope_complete {
                return Err(changed_path_limit_error(scope_limit));
            }
            return Ok(DiffScopeResolution {
                diff_scope: Some(DiffScopeReceipt {
                    base_revision: Some(base_revision),
                    head_revision: Some(head_revision),
                    changed_paths,
                    changed_paths_complete: scope_complete,
                    changed_paths_limit: (!scope_complete).then_some(scope_limit).flatten(),
                    indexed_changed_paths: 0,
                    evidence: None,
                }),
                working_tree: working_tree_status,
            });
        }
        let (mut resolved_paths, changed_paths_complete, changed_paths_limit) = if has_paths {
            (request.changed_paths.clone(), true, None)
        } else {
            (
                working_tree_status
                    .changed_paths
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>(),
                working_tree_status.changed_paths_complete(),
                working_tree_status.changed_paths_limit(),
            )
        };
        resolved_paths.sort();
        resolved_paths.dedup();
        Ok(DiffScopeResolution {
            diff_scope: Some(DiffScopeReceipt {
                base_revision: None,
                head_revision: None,
                changed_paths: resolved_paths,
                changed_paths_complete,
                changed_paths_limit,
                indexed_changed_paths: 0,
                evidence: None,
            }),
            working_tree: working_tree_status,
        })
    }
}

fn changed_path_limit_error(limit: Option<usize>) -> Error {
    let limit = limit.unwrap_or(MAX_DIFF_CHANGED_PATHS);
    Error::RequestLimitExceeded {
        field: "git changed paths",
        requested: limit.saturating_add(1),
        limit,
    }
}
use super::*;
