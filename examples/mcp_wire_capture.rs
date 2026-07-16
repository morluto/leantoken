use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use serde::Serialize;

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
    /// Tokenizer the analyzer should use.
    #[arg(long, default_value = "cl100k_base")]
    tokenizer: String,
    /// Server command and arguments, following `--`.
    #[arg(required = true, last = true)]
    command: Vec<OsString>,
}

#[derive(Debug, Serialize)]
struct Trace {
    schema_version: u32,
    host: String,
    host_version: String,
    tokenizer: String,
    generated_at_unix_seconds: u64,
    provider_total_input_tokens: Option<u64>,
    events: Vec<Event>,
}

struct Recorder {
    output: PathBuf,
    host: String,
    host_version: String,
    tokenizer: String,
    generated_at_unix_seconds: u64,
    events: Mutex<Vec<Event>>,
    sequence: AtomicU64,
}

impl Recorder {
    fn new(args: &Args) -> Result<Self, Box<dyn Error>> {
        if let Some(parent) = args
            .output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let recorder = Self {
            output: args.output.clone(),
            host: args.host.clone(),
            host_version: args.host_version.clone(),
            tokenizer: args.tokenizer.clone(),
            generated_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
            events: Mutex::new(Vec::new()),
            sequence: AtomicU64::new(0),
        };
        recorder.persist()?;
        Ok(recorder)
    }

    fn record(&self, direction: &'static str, raw_json: String) -> std::io::Result<()> {
        let event = Event {
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            direction,
            raw_json,
            provider_input_tokens: None,
        };
        {
            self.events
                .lock()
                .map_err(|_| std::io::Error::other("wire event recorder was poisoned"))?
                .push(event);
        }
        self.persist()
    }

    fn persist(&self) -> std::io::Result<()> {
        let mut events = self
            .events
            .lock()
            .map_err(|_| std::io::Error::other("wire event recorder was poisoned"))?;
        events.sort_by_key(|event| event.sequence);
        let trace = Trace {
            schema_version: 1,
            host: self.host.clone(),
            host_version: self.host_version.clone(),
            tokenizer: self.tokenizer.clone(),
            generated_at_unix_seconds: self.generated_at_unix_seconds,
            provider_total_input_tokens: None,
            events: events.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&trace).map_err(std::io::Error::other)?;
        let temporary = self.output.with_extension("wire-capture.tmp");
        fs::write(&temporary, bytes)?;
        if cfg!(windows) && self.output.exists() {
            fs::remove_file(&self.output)?;
        }
        fs::rename(temporary, &self.output)
    }
}

#[derive(Debug, Clone, Serialize)]
struct Event {
    sequence: u64,
    direction: &'static str,
    raw_json: String,
    provider_input_tokens: Option<u64>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let (program, command_args) = args
        .command
        .split_first()
        .ok_or("server command is empty")?;
    let mut child = Command::new(program)
        .args(command_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let child_stdin = child.stdin.take().ok_or("server stdin unavailable")?;
    let child_stdout = child.stdout.take().ok_or("server stdout unavailable")?;
    let recorder = Arc::new(Recorder::new(&args)?);

    let input_recorder = Arc::clone(&recorder);
    let input = std::thread::spawn(move || -> std::io::Result<()> {
        copy_lines(
            BufReader::new(std::io::stdin().lock()),
            child_stdin,
            "client_to_server",
            &input_recorder,
        )
    });
    let output_recorder = Arc::clone(&recorder);
    let output = std::thread::spawn(move || -> std::io::Result<()> {
        copy_lines(
            BufReader::new(child_stdout),
            std::io::stdout().lock(),
            "server_to_client",
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
    direction: &'static str,
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
    fn proxy_preserves_bytes_and_records_exact_json() {
        let input = b"{ \"jsonrpc\": \"2.0\" }\r\n";
        let mut output = Vec::new();
        let directory = tempfile::tempdir().expect("trace directory");
        let args = Args {
            output: directory.path().join("trace.json"),
            host: "test".into(),
            host_version: "1".into(),
            tokenizer: "cl100k_base".into(),
            command: vec!["unused".into()],
        };
        let recorder = Recorder::new(&args).expect("recorder");

        copy_lines(
            BufReader::new(&input[..]),
            &mut output,
            "client_to_server",
            &recorder,
        )
        .expect("copy trace");

        assert_eq!(output, input);
        let events = recorder.events.into_inner().expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].raw_json, "{ \"jsonrpc\": \"2.0\" }");
        let trace: serde_json::Value =
            serde_json::from_slice(&fs::read(&args.output).expect("persisted trace"))
                .expect("valid trace");
        assert_eq!(trace["events"].as_array().map(Vec::len), Some(1));
    }
}
