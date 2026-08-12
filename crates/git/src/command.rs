pub struct GitCaptureOptions {
    pub timeout: Duration,
    pub field: &'static str,
    pub timeout_reason: &'static str,
    pub failure_reason: &'static str,
    pub max_output_bytes: usize,
}

pub fn run_git_capture(
    root: &Path,
    program: &Path,
    args: &[String],
    options: GitCaptureOptions,
) -> Result<Vec<u8>> {
    run_git_capture_bounded(root, program, args, None, options)
}

pub fn run_git_capture_with_input(
    root: &Path,
    program: &Path,
    args: &[String],
    input: &[u8],
    options: GitCaptureOptions,
) -> Result<Vec<u8>> {
    run_git_capture_bounded(root, program, args, Some(input), options)
}

pub fn run_git_capture_bounded(
    root: &Path,
    program: &Path,
    args: &[String],
    input: Option<&[u8]>,
    options: GitCaptureOptions,
) -> Result<Vec<u8>> {
    use std::io::Read;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread;
    use std::time::Instant;

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
        .current_dir(root)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.group_spawn().map_err(|_| Error::InvalidInput {
        field: options.field,
        reason: "git is unavailable",
    })?;

    let output_limit_exceeded = Arc::new(AtomicBool::new(false));
    let reader_exceeded = Arc::clone(&output_limit_exceeded);
    let (release_reader, reader_release) = std::sync::mpsc::channel();
    let max_output_bytes = options.max_output_bytes;
    let mut stdout = child
        .inner()
        .stdout
        .take()
        .ok_or_else(|| Error::OperationFailure("git stdout unavailable".into()))?;
    let reader = thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut output = Vec::with_capacity(max_output_bytes.min(64 * 1024));
        let mut chunk = [0u8; 8 * 1024];
        loop {
            let read = stdout.read(&mut chunk)?;
            if read == 0 {
                return Ok(output);
            }
            if output.len().saturating_add(read) > max_output_bytes {
                reader_exceeded.store(true, Ordering::Release);
                // Keep the pipe open without draining it until the parent has
                // killed the producer. Otherwise a fast producer can observe
                // SIGPIPE, let a shell wrapper continue to its next command,
                // and perform work after the limit was crossed.
                let _ = reader_release.recv();
                return Ok(output);
            }
            output.extend_from_slice(&chunk[..read]);
        }
    });
    let writer = input.map(|input| {
        let input = input.to_vec();
        let mut stdin = child.inner().stdin.take();
        thread::spawn(move || -> std::io::Result<()> {
            stdin
                .as_mut()
                .ok_or_else(|| std::io::Error::other("git stdin unavailable"))?
                .write_all(&input)
        })
    });

    enum ChildOutcome {
        Exited(std::process::ExitStatus),
        OutputLimit,
        Timeout,
        WaitError(std::io::Error),
    }

    let deadline = Instant::now() + options.timeout;
    let outcome = loop {
        if output_limit_exceeded.load(Ordering::Acquire) {
            break ChildOutcome::OutputLimit;
        }
        match child.try_wait() {
            Ok(Some(status)) => break ChildOutcome::Exited(status),
            Ok(None) => {}
            Err(error) => break ChildOutcome::WaitError(error),
        }
        if Instant::now() >= deadline {
            break ChildOutcome::Timeout;
        }
        thread::sleep(Duration::from_millis(5));
    };

    // Terminate the whole process group even after the direct child exits:
    // external helpers can otherwise retain stdout and block the reader join.
    let _ = child.kill();
    let _ = child.wait();
    // If the reader crossed the bound just as the child exited, it may be
    // waiting for this signal even though the polling loop observed normal
    // exit first.
    let _ = release_reader.send(());

    let status = match outcome {
        ChildOutcome::Exited(status) => status,
        ChildOutcome::OutputLimit => {
            let _ = reader.join();
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            return Err(Error::RequestLimitExceeded {
                field: "git output bytes",
                requested: options.max_output_bytes.saturating_add(1),
                limit: options.max_output_bytes,
            });
        }
        ChildOutcome::Timeout => {
            let _ = reader.join();
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            return Err(Error::InvalidInput {
                field: options.field,
                reason: options.timeout_reason,
            });
        }
        ChildOutcome::WaitError(error) => {
            let _ = reader.join();
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            return Err(error.into());
        }
    };
    if let Some(writer) = writer {
        writer
            .join()
            .map_err(|_| Error::OperationFailure("git stdin task panicked".into()))??;
    }
    let output = reader
        .join()
        .map_err(|_| Error::OperationFailure("git stdout task panicked".into()))??;
    if output_limit_exceeded.load(Ordering::Acquire) {
        return Err(Error::RequestLimitExceeded {
            field: "git output bytes",
            requested: options.max_output_bytes.saturating_add(1),
            limit: options.max_output_bytes,
        });
    }
    if !status.success() {
        return Err(Error::InvalidInput {
            field: options.field,
            reason: options.failure_reason,
        });
    }
    Ok(output)
}
use super::*;
