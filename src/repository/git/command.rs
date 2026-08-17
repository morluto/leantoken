use crate::subprocess::{CaptureError, CaptureOptions, capture_stdout_bounded};

pub(crate) struct GitCaptureOptions {
    pub(crate) timeout: Duration,
    pub(crate) field: &'static str,
    pub(crate) timeout_reason: &'static str,
    pub(crate) failure_reason: &'static str,
    pub(crate) max_output_bytes: usize,
}

pub(crate) fn run_git_capture(
    root: &Path,
    program: &Path,
    args: &[String],
    options: GitCaptureOptions,
) -> Result<Vec<u8>> {
    run_git_capture_bounded(root, program, args, None, options)
}

pub(crate) fn run_git_capture_with_input(
    root: &Path,
    program: &Path,
    args: &[String],
    input: &[u8],
    options: GitCaptureOptions,
) -> Result<Vec<u8>> {
    run_git_capture_bounded(root, program, args, Some(input), options)
}

pub(crate) fn run_git_capture_bounded(
    root: &Path,
    program: &Path,
    args: &[String],
    input: Option<&[u8]>,
    options: GitCaptureOptions,
) -> Result<Vec<u8>> {
    let mut command = Command::new(program);
    // Force Git to treat all pathspec arguments as literal filesystem
    // paths, preventing pathspec magic (e.g. `:(literal)`, `:(exclude)`)
    // in repository filenames from being interpreted as Git selectors.
    // See issue #546: repository filenames can legitimately begin with
    // pathspec magic syntax, but Git should never treat them as patterns.
    command
        .env_remove("GIT_GLOB_PATHSPECS")
        .env_remove("GIT_ICASE_PATHSPECS")
        .env_remove("GIT_NOGLOB_PATHSPECS")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args(args)
        .current_dir(root);
    let output = capture_stdout_bounded(
        &mut command,
        input,
        CaptureOptions {
            timeout: options.timeout,
            max_stdout_bytes: options.max_output_bytes,
        },
    )
    .map_err(|error| match error {
        CaptureError::Spawn => Error::InvalidInput {
            field: options.field,
            reason: "git is unavailable",
        },
        CaptureError::MissingPipe(pipe) => {
            Error::OperationFailure(format!("git {pipe} unavailable"))
        }
        CaptureError::StdoutLimit { limit } => Error::RequestLimitExceeded {
            field: "git output bytes",
            requested: limit.saturating_add(1),
            limit,
        },
        CaptureError::Timeout => Error::InvalidInput {
            field: options.field,
            reason: options.timeout_reason,
        },
        CaptureError::Io(error) => error.into(),
        CaptureError::WorkerPanicked(worker) => {
            Error::OperationFailure(format!("git {worker} task panicked"))
        }
    })?;
    if !output.status.success() {
        return Err(Error::InvalidInput {
            field: options.field,
            reason: options.failure_reason,
        });
    }
    Ok(output.stdout)
}
use super::*;
