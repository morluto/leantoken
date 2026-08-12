#[path = "support/wire_trace.rs"]
mod wire_trace;

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, ValueEnum};
use leantoken::tokens::Tokenizer;
use wire_trace::{Direction, Event, RepositoryIdentity, TRACE_SCHEMA_V2, Trace};

#[derive(Debug, Parser)]
#[command(about = "Proxy an MCP stdio server and capture exact JSON-RPC messages")]
struct Args {
    /// Trace path. Never written to stdout, which remains protocol-only.
    #[arg(long)]
    output: PathBuf,
    /// MCP host name recorded with the trace.
    #[arg(long)]
    host: String,
    /// Exact MCP host version recorded with the trace.
    #[arg(long)]
    host_version: String,
    /// Provider name when known.
    #[arg(long)]
    provider: Option<String>,
    /// Model identifier when known.
    #[arg(long)]
    model: Option<String>,
    /// Pinned repository revision when known.
    #[arg(long)]
    repository_revision: Option<String>,
    /// Hash or stable description of the dirty state.
    #[arg(long)]
    dirty_fingerprint: Option<String>,
    /// Tokenizer the analyzer should use.
    #[arg(long, default_value = "cl100k_base")]
    tokenizer: String,
    /// Server command and arguments, following `--`.
    #[arg(required = true, last = true)]
    command: Vec<OsString>,
}

#[derive(Debug)]
struct Recorder {
    output: PathBuf,
    trace_id: String,
    host: String,
    host_version: String,
    model: Option<String>,
    provider: Option<String>,
    tokenizer: String,
    token_count_exact: bool,
    generated_at_unix_seconds: u64,
    repository: Option<RepositoryIdentity>,
    events: Mutex<Vec<Event>>,
    sequence: AtomicU64,
}

impl Recorder {
    fn new(args: &Args) -> Result<Self, Box<dyn Error>> {
        let tokenizer = Tokenizer::from_str(&args.tokenizer, false)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        let repository = match (&args.repository_revision, &args.dirty_fingerprint) {
            (Some(revision), Some(dirty_fingerprint)) => Some(RepositoryIdentity {
                revision: revision.clone(),
                dirty_fingerprint: dirty_fingerprint.clone(),
            }),
            (None, None) => None,
            _ => {
                return Err(
                    "repository revision and dirty fingerprint must be supplied together".into(),
                );
            }
        };
        let generated_at = SystemTime::now().duration_since(UNIX_EPOCH)?;
        if let Some(parent) = args
            .output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let recorder = Self {
            output: args.output.clone(),
            trace_id: format!("stdio-{}", generated_at.as_millis()),
            host: args.host.clone(),
            host_version: args.host_version.clone(),
            model: args.model.clone(),
            provider: args.provider.clone(),
            tokenizer: args.tokenizer.clone(),
            token_count_exact: tokenizer.is_exact(),
            generated_at_unix_seconds: generated_at.as_secs(),
            repository,
            events: Mutex::new(Vec::new()),
            sequence: AtomicU64::new(0),
        };
        recorder.persist()?;
        Ok(recorder)
    }

    fn record(&self, direction: Direction, raw_json: String) -> std::io::Result<()> {
        let timestamp_unix_millis = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(std::io::Error::other)?
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        let mut events = self
            .events
            .lock()
            .map_err(|_| std::io::Error::other("wire event recorder was poisoned"))?;
        let event = Event {
            sequence: Some(self.sequence.fetch_add(1, Ordering::Relaxed)),
            direction,
            turn: None,
            timestamp_unix_millis: Some(timestamp_unix_millis),
            latency_ms: None,
            category: None,
            message: None,
            raw_json: Some(raw_json),
            provider_visible_payload: None,
            tool_name: None,
            call_id: None,
            result_id: None,
            ranges: Vec::new(),
            visible_through_turn: None,
            stable_prefix: None,
            cache_eligible: None,
            compaction: None,
            provider_usage: None,
            provider_input_tokens: None,
        };
        events.push(event);
        drop(events);
        self.persist()
    }

    fn persist(&self) -> std::io::Result<()> {
        let mut events = self
            .events
            .lock()
            .map_err(|_| std::io::Error::other("wire event recorder was poisoned"))?;
        events.sort_by_key(|event| event.sequence);
        let mut trace = Trace {
            schema_version: TRACE_SCHEMA_V2,
            trace_id: Some(self.trace_id.clone()),
            trace_content_blake3: None,
            host: self.host.clone(),
            host_version: self.host_version.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            tokenizer: self.tokenizer.clone(),
            token_count_exact: Some(self.token_count_exact),
            generated_at_unix_seconds: Some(self.generated_at_unix_seconds),
            repository: self.repository.clone(),
            final_turn: None,
            provider_usage: None,
            provider_total_input_tokens: None,
            outcome: None,
            events: events.clone(),
        };
        trace.seal_content_hash().map_err(std::io::Error::other)?;
        let bytes = serde_json::to_vec_pretty(&trace).map_err(std::io::Error::other)?;
        let temporary = self.output.with_extension("wire-capture.tmp");
        fs::write(&temporary, bytes)?;
        if cfg!(windows) && self.output.exists() {
            fs::remove_file(&self.output)?;
        }
        fs::rename(temporary, &self.output)
    }
}
fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let (program, command_args) = args
        .command
        .split_first()
        .ok_or("server command is empty")?;
    let recorder = Arc::new(Recorder::new(&args)?);
    let mut child = Command::new(program)
        .args(command_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let child_stdin = child.stdin.take().ok_or("server stdin unavailable")?;
    let child_stdout = child.stdout.take().ok_or("server stdout unavailable")?;

    let input_recorder = Arc::clone(&recorder);
    let input = std::thread::spawn(move || -> std::io::Result<()> {
        copy_lines(
            BufReader::new(std::io::stdin().lock()),
            child_stdin,
            Direction::ClientToServer,
            &input_recorder,
        )
    });
    let output_recorder = Arc::clone(&recorder);
    let output = std::thread::spawn(move || -> std::io::Result<()> {
        copy_lines(
            BufReader::new(child_stdout),
            std::io::stdout().lock(),
            Direction::ServerToClient,
            &output_recorder,
        )
    });

    let status = child.wait()?;
    output.join().map_err(|_| "MCP output proxy panicked")??;
    if input.is_finished() {
        input.join().map_err(|_| "MCP input proxy panicked")??;
    }

    recorder.persist()?;
    if !status.success() {
        return Err(format!("MCP server exited with {status}").into());
    }
    Ok(())
}

fn copy_lines(
    mut reader: impl BufRead,
    mut writer: impl Write,
    direction: Direction,
    recorder: &Recorder,
) -> std::io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        writer.write_all(&line)?;
        writer.flush()?;
        let raw_json = String::from_utf8_lossy(&line)
            .trim_end_matches(['\r', '\n'])
            .to_owned();
        if raw_json.trim().is_empty() {
            continue;
        }
        recorder.record(direction, raw_json)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_preserves_bytes_and_persists_each_message() {
        let input = b"{ \"jsonrpc\": \"2.0\" }\r\n";
        let mut output = Vec::new();
        let directory = tempfile::tempdir().expect("trace directory");
        let args = Args {
            output: directory.path().join("trace.json"),
            host: "test".into(),
            host_version: "1".into(),
            provider: Some("test-provider".into()),
            model: Some("test-model".into()),
            repository_revision: Some("test-revision".into()),
            dirty_fingerprint: Some("clean".into()),
            tokenizer: "cl100k_base".into(),
            command: vec!["unused".into()],
        };
        let recorder = Recorder::new(&args).expect("recorder");

        copy_lines(
            BufReader::new(&input[..]),
            &mut output,
            Direction::ClientToServer,
            &recorder,
        )
        .expect("copy trace");

        assert_eq!(output, input);
        let events = recorder.events.into_inner().expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].raw_json.as_deref(),
            Some("{ \"jsonrpc\": \"2.0\" }")
        );
        assert_eq!(events[0].sequence, Some(0));
        assert!(events[0].timestamp_unix_millis.is_some());
        let trace: Trace =
            serde_json::from_slice(&fs::read(&args.output).expect("persisted trace"))
                .expect("valid trace");
        trace.validate_version().expect("validated v2 trace");
        assert_eq!(trace.events.len(), 1);
        assert_eq!(trace.model.as_deref(), Some("test-model"));
        assert_eq!(trace.provider.as_deref(), Some("test-provider"));
        assert_eq!(
            trace
                .repository
                .as_ref()
                .map(|repository| repository.revision.as_str()),
            Some("test-revision")
        );
    }

    #[test]
    fn recorder_requires_complete_repository_identity() {
        let directory = tempfile::tempdir().expect("trace directory");
        let args = Args {
            output: directory.path().join("trace.json"),
            host: "test".into(),
            host_version: "1".into(),
            provider: None,
            model: None,
            repository_revision: Some("test-revision".into()),
            dirty_fingerprint: None,
            tokenizer: "cl100k_base".into(),
            command: vec!["unused".into()],
        };

        let error = Recorder::new(&args).expect_err("incomplete identity must fail");
        assert!(error.to_string().contains("must be supplied together"));
    }

    #[test]
    fn concurrent_directions_persist_a_contiguous_valid_trace() {
        let directory = tempfile::tempdir().expect("trace directory");
        let output = directory.path().join("trace.json");
        let recorder = Arc::new(
            Recorder::new(&Args {
                output: output.clone(),
                host: "test".into(),
                host_version: "1".into(),
                provider: None,
                model: None,
                repository_revision: None,
                dirty_fingerprint: None,
                tokenizer: "cl100k_base".into(),
                command: vec!["unused".into()],
            })
            .expect("recorder"),
        );
        let threads = [Direction::ClientToServer, Direction::ServerToClient]
            .into_iter()
            .map(|direction| {
                let recorder = Arc::clone(&recorder);
                std::thread::spawn(move || {
                    for id in 0..4 {
                        recorder
                            .record(direction, format!(r#"{{"jsonrpc":"2.0","id":{id}}}"#))
                            .expect("record event");
                    }
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().expect("recording thread");
        }

        let trace: Trace = serde_json::from_slice(&fs::read(output).expect("persisted trace"))
            .expect("valid trace json");
        trace.validate_version().expect("validated v2 trace");
        assert_eq!(trace.events.len(), 8);
    }
}
