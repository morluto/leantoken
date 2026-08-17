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
    fn unavailable(changed_paths: HashSet<String>, modified: bool, untracked: bool) -> Self {
        Self {
            changed_paths,
            state: GitWorkingTreeState::Unavailable(GitWorkingTreeChanges::from_flags(
                modified, untracked,
            )),
        }
    }

    fn available(changed_paths: HashSet<String>, modified: bool, untracked: bool) -> Self {
        Self {
            changed_paths,
            state: GitWorkingTreeState::Available(GitWorkingTreeChanges::from_flags(
                modified, untracked,
            )),
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
        return GitWorkingTreeStatus::available(HashSet::new(), false, false);
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
    let Ok(output) = run_git_capture(
        root,
        program,
        &args,
        GitCaptureOptions {
            timeout,
            field: "git status",
            timeout_reason: "git status timed out",
            failure_reason: "git status failed",
            max_output_bytes: bounded_git_output(max, GIT_PATH_OUTPUT_BYTES_PER_RESULT),
        },
    ) else {
        return GitWorkingTreeStatus::unavailable(HashSet::new(), false, false);
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
        return GitWorkingTreeStatus::available(HashSet::new(), false, false);
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
            Err(_) => return GitWorkingTreeStatus::unavailable(changed, modified, untracked),
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
            return GitWorkingTreeStatus::unavailable(HashSet::new(), modified, untracked);
        };

        let Some(path) = path.strip_prefix(prefix) else {
            continue;
        };
        changed.insert(slash_path(Path::new(path)));
        if changed.len() == max {
            tracing::warn!(
                changed_paths = max,
                "git status changed-path set truncated at {} entries;                 the working tree has more changed paths than the bound",
                max,
            );
            break;
        }
    }
    GitWorkingTreeStatus::available(changed, modified, untracked)
}
use super::*;
