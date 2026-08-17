use std::{
    io::{self, Read, Write},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use command_group::CommandGroup;

#[derive(Debug, Clone, Copy)]
pub(crate) struct CaptureOptions {
    pub(crate) timeout: Duration,
    pub(crate) max_stdout_bytes: usize,
}

#[derive(Debug)]
pub(crate) struct CapturedOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
}

#[derive(Debug)]
pub(crate) enum CaptureError {
    Spawn,
    MissingPipe(&'static str),
    StdoutLimit { limit: usize },
    Timeout,
    Io(io::Error),
    WorkerPanicked(&'static str),
}

pub(crate) fn capture_stdout_bounded(
    command: &mut Command,
    input: Option<&[u8]>,
    options: CaptureOptions,
) -> std::result::Result<CapturedOutput, CaptureError> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.group_spawn().map_err(|_| CaptureError::Spawn)?;

    let stdin = if input.is_some() {
        match child.inner().stdin.take() {
            Some(stdin) => Some(stdin),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CaptureError::MissingPipe("stdin"));
            }
        }
    } else {
        None
    };
    let mut stdout = match child.inner().stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CaptureError::MissingPipe("stdout"));
        }
    };

    let output_limit_exceeded = Arc::new(AtomicBool::new(false));
    let reader_exceeded = Arc::clone(&output_limit_exceeded);
    let (release_reader, reader_release) = std::sync::mpsc::channel();
    let max_stdout_bytes = options.max_stdout_bytes;
    let reader = thread::spawn(move || -> io::Result<Vec<u8>> {
        let mut output = Vec::with_capacity(max_stdout_bytes.min(64 * 1024));
        let mut chunk = [0u8; 8 * 1024];
        loop {
            let read = stdout.read(&mut chunk)?;
            if read == 0 {
                return Ok(output);
            }
            if output.len().saturating_add(read) > max_stdout_bytes {
                reader_exceeded.store(true, Ordering::Release);
                // Keep the pipe open without draining it until the parent has
                // killed the producer. Otherwise a fast producer can observe
                // SIGPIPE and continue work after crossing the output bound.
                let _ = reader_release.recv();
                return Ok(output);
            }
            output.extend_from_slice(&chunk[..read]);
        }
    });
    let writer = input.map(|input| {
        let input = input.to_vec();
        let mut stdin = stdin;
        thread::spawn(move || -> io::Result<()> {
            stdin
                .as_mut()
                .ok_or_else(|| io::Error::other("subprocess stdin unavailable"))?
                .write_all(&input)
        })
    });

    enum ChildOutcome {
        Exited(ExitStatus),
        OutputLimit,
        Timeout,
        WaitError(io::Error),
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
    let _ = release_reader.send(());

    match outcome {
        ChildOutcome::OutputLimit => {
            let _ = reader.join();
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            Err(CaptureError::StdoutLimit {
                limit: options.max_stdout_bytes,
            })
        }
        ChildOutcome::Timeout => {
            let _ = reader.join();
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            Err(CaptureError::Timeout)
        }
        ChildOutcome::WaitError(error) => {
            let _ = reader.join();
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            Err(CaptureError::Io(error))
        }
        ChildOutcome::Exited(status) => {
            if let Some(writer) = writer {
                writer
                    .join()
                    .map_err(|_| CaptureError::WorkerPanicked("stdin"))?
                    .map_err(CaptureError::Io)?;
            }
            let stdout = reader
                .join()
                .map_err(|_| CaptureError::WorkerPanicked("stdout"))?
                .map_err(CaptureError::Io)?;
            if output_limit_exceeded.load(Ordering::Acquire) {
                return Err(CaptureError::StdoutLimit {
                    limit: options.max_stdout_bytes,
                });
            }
            Ok(CapturedOutput { status, stdout })
        }
    }
}
