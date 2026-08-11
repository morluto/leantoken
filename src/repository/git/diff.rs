/// Resolve changed paths between a base revision and the working tree.
///
/// Runs `git diff --name-only -z --no-renames <base> -- .` to capture both
/// committed and uncommitted changes relative to the base. The call is
/// bounded by `max` paths and a timeout. If git is unavailable or the
/// revision cannot be resolved, an error is returned so the caller can
/// surface an actionable message.
pub fn git_diff_paths(root: &Path, base_revision: &str, max: usize) -> Result<GitDiffResult> {
    git_diff_paths_with(
        root,
        base_revision,
        max,
        Path::new("git"),
        Duration::from_millis(1_000),
    )
}

/// Resolve changed paths between two immutable Git revisions.
pub fn git_diff_paths_between(
    root: &Path,
    base_revision: &str,
    head_revision: &str,
    max: usize,
) -> Result<GitDiffResult> {
    let timeout = Duration::from_millis(1_000);
    let program = Path::new("git");
    let base_sha =
        resolve_revision_sha_for_field(root, program, base_revision, timeout, "base revision")?;
    let head_sha =
        resolve_revision_sha_for_field(root, program, head_revision, timeout, "head revision")?;
    let changed_paths = diff_name_only(
        root,
        program,
        &base_sha,
        Some(&head_sha),
        max,
        timeout,
        &git_worktree_prefix(root),
    )?;
    Ok(GitDiffResult {
        base_revision: base_sha,
        head_revision: head_sha,
        changed_paths,
    })
}

/// Resolve diff endpoint identities without enumerating changed paths.
pub(crate) fn git_diff_identity(
    root: &Path,
    base_revision: &str,
    head_revision: Option<&str>,
) -> Result<GitDiffResult> {
    let timeout = Duration::from_millis(1_000);
    let program = Path::new("git");
    let base_sha =
        resolve_revision_sha_for_field(root, program, base_revision, timeout, "base revision")?;
    let head_sha = resolve_revision_sha_for_field(
        root,
        program,
        head_revision.unwrap_or("HEAD"),
        timeout,
        "head revision",
    )?;
    Ok(GitDiffResult {
        base_revision: base_sha,
        head_revision: head_sha,
        changed_paths: Vec::new(),
    })
}

/// Resolve the current Git `HEAD` to the same bounded short SHA used by diff receipts.
pub(crate) fn git_head_revision(root: &Path) -> Result<String> {
    resolve_revision_sha_for_field(
        root,
        Path::new("git"),
        "HEAD",
        Duration::from_millis(1_000),
        "head revision",
    )
}

/// Resolve the current branch name with the same bounded Git command policy
/// used for other snapshot metadata. Detached HEAD is reported as unavailable.
pub(crate) fn git_branch_name(root: &Path) -> Result<String> {
    let output = run_git_capture(
        root,
        Path::new("git"),
        &["symbolic-ref".into(), "--short".into(), "HEAD".into()],
        GitCaptureOptions {
            timeout: Duration::from_millis(500),
            field: "git branch",
            timeout_reason: "git branch lookup timed out",
            failure_reason: "git branch lookup failed",
            max_output_bytes: 512,
        },
    )?;
    let branch = String::from_utf8(output)
        .map_err(|_| Error::InvalidInput {
            field: "git branch",
            reason: "git branch name is not valid UTF-8",
        })?
        .trim()
        .to_owned();
    if branch.is_empty() {
        return Err(Error::InvalidInput {
            field: "git branch",
            reason: "git branch name is unavailable",
        });
    }
    Ok(branch)
}

/// Parse bounded target-side hunk ranges between a base revision and the working tree.
pub fn git_diff_hunks(root: &Path, base_revision: &str, max: usize) -> Result<Vec<GitHunkRange>> {
    git_diff_hunks_with_head(root, base_revision, None, &[], max)
}

/// Parse bounded target-side hunk ranges between two immutable Git revisions.
pub fn git_diff_hunks_between(
    root: &Path,
    base_revision: &str,
    head_revision: &str,
    max: usize,
) -> Result<Vec<GitHunkRange>> {
    git_diff_hunks_with_head(root, base_revision, Some(head_revision), &[], max)
}

/// Parse bounded target-side hunk ranges only for explicit repository paths.
pub(crate) fn git_diff_hunks_scoped(
    root: &Path,
    base_revision: &str,
    head_revision: Option<&str>,
    paths: &[String],
    max: usize,
) -> Result<Vec<GitHunkRange>> {
    git_diff_hunks_with_head(root, base_revision, head_revision, paths, max)
}

pub(crate) fn git_diff_hunks_with_head(
    root: &Path,
    base_revision: &str,
    head_revision: Option<&str>,
    paths: &[String],
    max: usize,
) -> Result<Vec<GitHunkRange>> {
    if max == 0 {
        return Ok(Vec::new());
    }
    let timeout = Duration::from_millis(1_000);
    let program = Path::new("git");
    let base_sha =
        resolve_revision_sha_for_field(root, program, base_revision, timeout, "base revision")?;
    let head_sha = head_revision
        .map(|revision| {
            resolve_revision_sha_for_field(root, program, revision, timeout, "head revision")
        })
        .transpose()?;
    let prefix = git_worktree_prefix(root);
    let mut args = vec![
        "-c".to_owned(),
        "core.fsmonitor=false".to_owned(),
        "diff".to_owned(),
        "--no-ext-diff".to_owned(),
        "--no-textconv".to_owned(),
        "--unified=0".to_owned(),
        "--no-renames".to_owned(),
        base_sha,
    ];
    args.extend(head_sha);
    args.push("--".to_owned());
    if paths.is_empty() {
        args.push(".".to_owned());
    } else {
        args.extend(paths.iter().cloned());
    }
    let output = run_git_capture(
        root,
        program,
        &args,
        GitCaptureOptions {
            timeout,
            field: "base revision",
            timeout_reason: "git diff timed out",
            failure_reason: "could not diff revision",
            max_output_bytes: bounded_git_output(max, GIT_HUNK_OUTPUT_BYTES_PER_RESULT),
        },
    )?;
    parse_git_diff_hunks(output.as_slice(), max, &prefix)
}

pub(crate) fn parse_git_diff_hunks<R: BufRead>(
    mut reader: R,
    max: usize,
    prefix: &str,
) -> Result<Vec<GitHunkRange>> {
    let mut ranges = Vec::new();
    let mut target_path = None;
    let mut line = String::new();
    while ranges.len() < max {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            target_path = path
                .strip_prefix("b/")
                .map(|path| path.trim_end_matches(['\r', '\n']))
                .and_then(|path| path.strip_prefix(prefix))
                .map(|path| slash_path(Path::new(path)));
            continue;
        }
        let Some(path) = target_path.as_ref() else {
            continue;
        };
        let Some(header) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some(target) = header.split_whitespace().find(|part| part.starts_with('+')) else {
            continue;
        };
        let target = &target[1..];
        let (start, length) = target
            .split_once(',')
            .map_or((target, "1"), |(start, length)| (start, length));
        let raw_start = start
            .parse::<usize>()
            .map_err(|error| Error::OperationFailure(error.to_string()))?;
        let length = length
            .parse::<usize>()
            .map_err(|error| Error::OperationFailure(error.to_string()))?;
        let (start_line, end_line) = if length == 0 {
            (
                raw_start.checked_add(1).ok_or_else(|| {
                    Error::OperationFailure("git diff hunk range overflow".into())
                })?,
                raw_start,
            )
        } else {
            let start_line = raw_start.max(1);
            let end_line = start_line
                .checked_add(length - 1)
                .ok_or_else(|| Error::OperationFailure("git diff hunk range overflow".into()))?;
            (start_line, end_line)
        };
        ranges.push(GitHunkRange {
            path: path.clone(),
            start_line,
            end_line,
        });
    }
    Ok(ranges)
}

pub(crate) fn git_diff_paths_with(
    root: &Path,
    base_revision: &str,
    max: usize,
    program: &Path,
    timeout: Duration,
) -> Result<GitDiffResult> {
    if base_revision.trim().is_empty() {
        return Err(Error::InvalidInput {
            field: "base revision",
            reason: "must not be empty",
        });
    }
    if max == 0 {
        return Ok(GitDiffResult {
            base_revision: String::new(),
            head_revision: String::new(),
            changed_paths: Vec::new(),
        });
    }
    let prefix = git_worktree_prefix(root);
    let base_sha = resolve_revision_sha(root, program, base_revision, timeout)?;
    let head_sha = resolve_revision_sha(root, program, "HEAD", timeout)?;
    let changed = diff_name_only(root, program, &base_sha, None, max, timeout, &prefix)?;
    Ok(GitDiffResult {
        base_revision: base_sha,
        head_revision: head_sha,
        changed_paths: changed,
    })
}

pub(crate) fn resolve_revision_sha(
    root: &Path,
    program: &Path,
    revision: &str,
    timeout: Duration,
) -> Result<String> {
    resolve_revision_sha_for_field(root, program, revision, timeout, "base revision")
}

pub(crate) fn resolve_revision_sha_for_field(
    root: &Path,
    program: &Path,
    revision: &str,
    timeout: Duration,
    field: &'static str,
) -> Result<String> {
    if revision.trim().is_empty() {
        return Err(Error::InvalidInput {
            field,
            reason: "must not be empty",
        });
    }
    let commit_revision = format!("{revision}^{{commit}}");
    let mut child = match Command::new(program)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args([
            "rev-parse",
            "--verify",
            "--short=12",
            "--end-of-options",
            &commit_revision,
        ])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return Err(Error::InvalidInput {
                field,
                reason: "git is unavailable",
            });
        }
    };
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) | Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::InvalidInput {
                field,
                reason: "git rev-parse timed out",
            });
        }
    };
    if !status.success() {
        return Err(Error::InvalidInput {
            field,
            reason: "could not resolve revision",
        });
    }
    let output = child.stdout.take().map_or(Vec::new(), |mut s| {
        use std::io::Read;
        let mut buf = Vec::new();
        let _ = s.read_to_end(&mut buf);
        buf
    });
    let sha = String::from_utf8_lossy(&output).trim().to_owned();
    if sha.is_empty() {
        return Err(Error::InvalidInput {
            field,
            reason: "resolved to an empty SHA",
        });
    }
    Ok(sha)
}

pub(crate) fn diff_name_only(
    root: &Path,
    program: &Path,
    base_sha: &str,
    head_sha: Option<&str>,
    max: usize,
    timeout: Duration,
    prefix: &str,
) -> Result<Vec<String>> {
    let mut args = vec![
        "-c".to_owned(),
        "core.fsmonitor=false".to_owned(),
        "diff".to_owned(),
        "--no-ext-diff".to_owned(),
        "--no-textconv".to_owned(),
        "--name-only".to_owned(),
        "-z".to_owned(),
        "--no-renames".to_owned(),
        base_sha.to_owned(),
    ];
    args.extend(head_sha.map(str::to_owned));
    args.extend(["--".to_owned(), ".".to_owned()]);
    let Ok(output) = run_git_capture(
        root,
        program,
        &args,
        GitCaptureOptions {
            timeout,
            field: "base revision",
            timeout_reason: "git diff timed out",
            failure_reason: "could not diff revision",
            max_output_bytes: bounded_git_output(max, GIT_PATH_OUTPUT_BYTES_PER_RESULT),
        },
    ) else {
        return Ok(Vec::new());
    };
    parse_diff_names(output.as_slice(), max, prefix)
}

pub(crate) fn parse_diff_names<R: BufRead>(
    mut reader: R,
    max: usize,
    prefix: &str,
) -> Result<Vec<String>> {
    if max == 0 {
        return Ok(Vec::new());
    }
    let mut changed = Vec::new();
    let mut record = Vec::new();
    loop {
        record.clear();
        match reader.read_until(0, &mut record) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        if record.last() == Some(&0) {
            record.pop();
        }
        if record.is_empty() {
            continue;
        }
        let path = std::str::from_utf8(&record).map_err(|_| Error::InvalidInput {
            field: "git diff path",
            reason: "must be valid UTF-8",
        })?;
        let Some(path) = path.strip_prefix(prefix) else {
            continue;
        };
        changed.push(slash_path(Path::new(path)));
        if changed.len() == max {
            break;
        }
    }
    Ok(changed)
}
use super::*;
