use crate::{Deadline, Sandbox};
use std::fmt;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const MAX_CAPTURE_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub struct ProcessHarness<'a> {
    binary: PathBuf,
    sandbox: &'a Sandbox,
    timeout: Duration,
}

#[derive(Debug)]
pub struct ProcessOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub environment_names: Vec<String>,
}

impl ProcessOutput {
    /// Bounded diagnostics suitable for a test failure message or CI log.
    pub fn diagnostics(&self) -> String {
        format!(
            "status={:?}, timed_out={}, env={:?}\nstdout:\n{}\nstderr:\n{}",
            self.status, self.timed_out, self.environment_names, self.stdout, self.stderr
        )
    }
}

#[derive(Debug)]
pub enum ProcessError {
    Io(std::io::Error),
    Timeout(ProcessOutput),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "process I/O error: {error}"),
            Self::Timeout(output) => write!(f, "process timed out: {output:?}"),
        }
    }
}
impl std::error::Error for ProcessError {}
impl From<std::io::Error> for ProcessError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl<'a> ProcessHarness<'a> {
    pub fn new(binary: impl Into<PathBuf>, sandbox: &'a Sandbox) -> Self {
        Self {
            binary: binary.into(),
            sandbox,
            timeout: Duration::from_secs(30),
        }
    }
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn run(&self, args: &[&str], input: &[u8]) -> Result<ProcessOutput, ProcessError> {
        let mut command = Command::new(&self.binary);
        command
            .args(args)
            .current_dir(self.sandbox.repo())
            .env_clear();
        let environment = self.sandbox.environment();
        for (key, value) in &environment {
            command.env(key, value);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stdout_thread = thread::spawn(move || read_all(stdout));
        let stderr_thread = thread::spawn(move || read_all(stderr));
        let input = input.to_owned();
        let stdin_thread = thread::spawn(move || stdin.write_all(&input));
        let deadline = Deadline::new(self.timeout);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status.code();
            }
            if deadline.expired() {
                child.kill()?;
                let _ = child.wait();
                break None;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let input_result = stdin_thread
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("stdin writer thread panicked")));
        if status.is_some() {
            input_result?;
        }
        let output = ProcessOutput {
            status,
            stdout: String::from_utf8_lossy(&stdout_thread.join().unwrap_or_default()).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_thread.join().unwrap_or_default()).into_owned(),
            timed_out: status.is_none(),
            environment_names: environment.keys().cloned().collect(),
        };
        std::fs::write(self.sandbox.logs().join("stdout.log"), &output.stdout)?;
        std::fs::write(self.sandbox.logs().join("stderr.log"), &output.stderr)?;
        if output.timed_out {
            Err(ProcessError::Timeout(output))
        } else {
            Ok(output)
        }
    }
}

fn read_all<R: Read>(mut reader: R) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(MAX_CAPTURE_BYTES);
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        if bytes.len() < MAX_CAPTURE_BYTES {
            let remaining = MAX_CAPTURE_BYTES - bytes.len();
            bytes.extend_from_slice(&buffer[..read.min(remaining)]);
            truncated |= read > remaining;
        } else {
            truncated = true;
        }
    }
    if truncated {
        bytes.extend_from_slice(
            format!("\n<output truncated at {MAX_CAPTURE_BYTES} bytes>\n").as_bytes(),
        );
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::{MAX_CAPTURE_BYTES, read_all};
    use std::io::Cursor;

    #[test]
    fn process_capture_is_bounded_and_marks_truncation() {
        let bytes = read_all(Cursor::new(vec![b'x'; MAX_CAPTURE_BYTES + 1024]));
        assert!(bytes.len() < MAX_CAPTURE_BYTES + 100);
        assert!(bytes.ends_with(b"<output truncated at 65536 bytes>\n"));
    }
}
