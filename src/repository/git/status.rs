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
    pub(crate) available: bool,
}

/// Observe bounded working-tree state without changing the public empty-set fallback.
pub(crate) fn git_working_tree_status(root: &Path, max: usize) -> GitWorkingTreeStatus {
    git_working_tree_status_with(root, max, Path::new("git"), Duration::from_millis(500))
}

fn git_changed_paths_with(
    root: &Path,
    max: usize,
    program: &Path,
    timeout: Duration,
) -> Result<HashSet<String>> {
    Ok(git_working_tree_status_with(root, max, program, timeout).changed_paths)
}

fn git_working_tree_status_with(
    root: &Path,
    max: usize,
    program: &Path,
    timeout: Duration,
) -> GitWorkingTreeStatus {
    if max == 0 {
        return GitWorkingTreeStatus {
            changed_paths: HashSet::new(),
            available: true,
        };
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
        return GitWorkingTreeStatus {
            changed_paths: HashSet::new(),
            available: false,
        };
    };
    let (changed_paths, available) = parse_git_status_observation(output.as_slice(), max, &prefix);
    GitWorkingTreeStatus {
        changed_paths,
        available,
    }
}

fn bounded_git_output(max_results: usize, bytes_per_result: usize) -> usize {
    max_results
        .saturating_mul(bytes_per_result)
        .max(bytes_per_result)
        .min(MAX_GIT_DISCOVERY_OUTPUT_BYTES)
}

fn git_worktree_prefix(root: &Path) -> String {
    root.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .and_then(|worktree| root.strip_prefix(worktree).ok())
        .map(slash_path)
        .filter(|prefix| !prefix.is_empty())
        .map(|prefix| format!("{prefix}/"))
        .unwrap_or_default()
}

#[cfg(test)]
fn parse_git_status<R: BufRead>(mut reader: R, max: usize, prefix: &str) -> HashSet<String> {
    parse_git_status_observation(&mut reader, max, prefix).0
}

fn parse_git_status_observation<R: BufRead>(
    mut reader: R,
    max: usize,
    prefix: &str,
) -> (HashSet<String>, bool) {
    if max == 0 {
        return (HashSet::new(), true);
    }
    let mut changed = HashSet::new();
    let mut record = Vec::new();

    loop {
        record.clear();
        match reader.read_until(0, &mut record) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => return (changed, false),
        }

        if record.last() == Some(&0) {
            record.pop();
        }
        if record.len() < 4 || record.get(2) != Some(&b' ') {
            continue;
        }

        let status = &record[..2];
        let path = String::from_utf8_lossy(&record[3..]).into_owned();

        // Ignore ignored files; keep modified, added, deleted, and untracked.
        if status == b"!!" {
            continue;
        }

        let Some(path) = path.strip_prefix(prefix) else {
            continue;
        };
        changed.insert(slash_path(Path::new(path)));
        if changed.len() == max {
            break;
        }
    }
    (changed, true)
}
