/// Return paths reported by `git status` as working-tree changes.
///
/// The result is capped at `max` entries to keep the call bounded. If the
/// root is not a Git repository, `git` is unavailable, or `git status` fails,
/// an empty set is returned so callers can safely proceed without a diff
/// signal.
pub fn git_changed_paths(root: &Path, max: usize) -> Result<HashSet<String>> {
    git_changed_paths_with(root, max, Path::new("git"), Duration::from_millis(500))
}

/// Bounded working-tree paths plus whether `git status` completed successfully.
pub(crate) struct GitWorkingTreeStatus {
    pub(crate) changed_paths: HashSet<String>,
    state: GitWorkingTreeState,
    changed_paths_complete: bool,
    changed_paths_limit: Option<usize>,
    failure: Option<GitWorkingTreeFailure>,
}

#[derive(Clone, Copy)]
enum GitWorkingTreeFailure {
    Unavailable,
    InvalidPathEncoding,
    Read,
    OutputBytes { requested: usize, limit: usize },
}

#[derive(Clone, Copy)]
enum GitWorkingTreeState {
    Unavailable(GitWorkingTreeChanges),
    Available(GitWorkingTreeChanges),
}

#[derive(Clone, Copy)]
enum GitWorkingTreeChanges {
    Clean,
    Modified,
    Untracked,
    ModifiedAndUntracked,
}

impl GitWorkingTreeStatus {
    fn unavailable(
        changed_paths: HashSet<String>,
        modified: bool,
        untracked: bool,
        failure: GitWorkingTreeFailure,
    ) -> Self {
        Self {
            changed_paths,
            state: GitWorkingTreeState::Unavailable(GitWorkingTreeChanges::from_flags(
                modified, untracked,
            )),
            changed_paths_complete: false,
            changed_paths_limit: None,
            failure: Some(failure),
        }
    }

    fn available(
        changed_paths: HashSet<String>,
        modified: bool,
        untracked: bool,
        changed_paths_complete: bool,
        changed_paths_limit: Option<usize>,
    ) -> Self {
        Self {
            changed_paths,
            state: GitWorkingTreeState::Available(GitWorkingTreeChanges::from_flags(
                modified, untracked,
            )),
            changed_paths_complete,
            changed_paths_limit,
            failure: None,
        }
    }

    pub(crate) const fn is_available(&self) -> bool {
        matches!(self.state, GitWorkingTreeState::Available(_))
    }

    pub(crate) const fn has_modified(&self) -> bool {
        matches!(
            self.state.changes(),
            GitWorkingTreeChanges::Modified | GitWorkingTreeChanges::ModifiedAndUntracked
        )
    }

    pub(crate) const fn has_untracked(&self) -> bool {
        matches!(
            self.state.changes(),
            GitWorkingTreeChanges::Untracked | GitWorkingTreeChanges::ModifiedAndUntracked
        )
    }

    pub(crate) const fn changed_paths_complete(&self) -> bool {
        self.changed_paths_complete
    }

    pub(crate) const fn changed_paths_limit(&self) -> Option<usize> {
        self.changed_paths_limit
    }

    pub(crate) fn require_complete(&self) -> Result<()> {
        match self.failure {
            Some(GitWorkingTreeFailure::OutputBytes { requested, limit }) => {
                Err(Error::RequestLimitExceeded {
                    field: "git output bytes",
                    requested,
                    limit,
                })
            }
            Some(GitWorkingTreeFailure::InvalidPathEncoding) => Err(Error::InvalidInput {
                field: "git status path",
                reason: "must be valid UTF-8",
            }),
            Some(GitWorkingTreeFailure::Read) => Err(Error::OperationFailure(
                "could not read git status paths".into(),
            )),
            Some(GitWorkingTreeFailure::Unavailable) => Err(Error::InvalidInput {
                field: "changed paths",
                reason: "working-tree changed-path discovery is unavailable",
            }),
            None if !self.changed_paths_complete => {
                let limit = self.changed_paths_limit.unwrap_or(self.changed_paths.len());
                Err(Error::RequestLimitExceeded {
                    field: "git changed paths",
                    requested: limit.saturating_add(1),
                    limit,
                })
            }
            None => Ok(()),
        }
    }
}

impl GitWorkingTreeState {
    const fn changes(self) -> GitWorkingTreeChanges {
        match self {
            Self::Unavailable(changes) | Self::Available(changes) => changes,
        }
    }
}

impl GitWorkingTreeChanges {
    const fn from_flags(modified: bool, untracked: bool) -> Self {
        match (modified, untracked) {
            (false, false) => Self::Clean,
            (true, false) => Self::Modified,
            (false, true) => Self::Untracked,
            (true, true) => Self::ModifiedAndUntracked,
        }
    }
}

/// Observe bounded working-tree state without changing the public empty-set fallback.
pub(crate) fn git_working_tree_status(root: &Path, max: usize) -> GitWorkingTreeStatus {
    git_working_tree_status_with(root, max, Path::new("git"), Duration::from_millis(500))
}

pub(crate) fn git_changed_paths_with(
    root: &Path,
    max: usize,
    program: &Path,
    timeout: Duration,
) -> Result<HashSet<String>> {
    Ok(git_working_tree_status_with(root, max, program, timeout).changed_paths)
}

pub(crate) fn git_working_tree_status_with(
    root: &Path,
    max: usize,
    program: &Path,
    timeout: Duration,
) -> GitWorkingTreeStatus {
    if max == 0 {
        return GitWorkingTreeStatus::available(HashSet::new(), false, false, false, Some(0));
    }
    let prefix = git_worktree_prefix(root);
    let args = [
        "-c",
        "core.fsmonitor=false",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--no-renames",
        "--",
        ".",
    ]
    .map(str::to_owned);
    let output = match run_git_capture(
        root,
        program,
        &args,
        GitCaptureOptions {
            timeout,
            field: "git status",
            timeout_reason: "git status timed out",
            failure_reason: "git status failed",
            max_output_bytes: bounded_git_output(
                max.saturating_add(1),
                GIT_PATH_OUTPUT_BYTES_PER_RESULT,
            ),
        },
    ) {
        Ok(output) => output,
        Err(Error::RequestLimitExceeded {
            field: "git output bytes",
            requested,
            limit,
        }) => {
            return GitWorkingTreeStatus::unavailable(
                HashSet::new(),
                false,
                false,
                GitWorkingTreeFailure::OutputBytes { requested, limit },
            );
        }
        Err(_) => {
            return GitWorkingTreeStatus::unavailable(
                HashSet::new(),
                false,
                false,
                GitWorkingTreeFailure::Unavailable,
            );
        }
    };
    parse_git_status_observation(output.as_slice(), max, &prefix)
}

pub(crate) fn bounded_git_output(max_results: usize, bytes_per_result: usize) -> usize {
    max_results
        .saturating_mul(bytes_per_result)
        .max(bytes_per_result)
        .min(MAX_GIT_DISCOVERY_OUTPUT_BYTES)
}

pub(crate) fn git_worktree_prefix(root: &Path) -> String {
    root.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .and_then(|worktree| root.strip_prefix(worktree).ok())
        .map(slash_path)
        .filter(|prefix| !prefix.is_empty())
        .map(|prefix| format!("{prefix}/"))
        .unwrap_or_default()
}

pub(crate) fn parse_git_status_observation<R: BufRead>(
    mut reader: R,
    max: usize,
    prefix: &str,
) -> GitWorkingTreeStatus {
    if max == 0 {
        return GitWorkingTreeStatus::available(HashSet::new(), false, false, false, Some(0));
    }
    let mut changed = HashSet::new();
    let mut modified = false;
    let mut untracked = false;
    let mut record = Vec::new();

    loop {
        record.clear();
        match reader.read_until(0, &mut record) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => {
                return GitWorkingTreeStatus::unavailable(
                    changed,
                    modified,
                    untracked,
                    GitWorkingTreeFailure::Read,
                );
            }
        }

        if record.last() == Some(&0) {
            record.pop();
        }
        if record.len() < 4 || record.get(2) != Some(&b' ') {
            continue;
        }

        let status = &record[..2];

        // Ignore ignored files; keep modified, added, deleted, and untracked.
        if status == b"!!" {
            continue;
        }

        if status == b"??" {
            untracked = true;
        } else {
            modified = true;
        }

        let Ok(path) = std::str::from_utf8(&record[3..]) else {
            return GitWorkingTreeStatus::unavailable(
                HashSet::new(),
                modified,
                untracked,
                GitWorkingTreeFailure::InvalidPathEncoding,
            );
        };

        let Some(path) = path.strip_prefix(prefix) else {
            continue;
        };
        let path = slash_path(Path::new(path));
        if changed.len() == max && !changed.contains(&path) {
            return GitWorkingTreeStatus::available(changed, modified, untracked, false, Some(max));
        }
        changed.insert(path);
    }
    GitWorkingTreeStatus::available(changed, modified, untracked, true, None)
}
use super::*;
