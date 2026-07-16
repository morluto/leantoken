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
    let events = Arc::new(Mutex::new(Vec::new()));
    let sequence = Arc::new(AtomicU64::new(0));

    let input_events = Arc::clone(&events);
    let input_sequence = Arc::clone(&sequence);
    let input = std::thread::spawn(move || -> std::io::Result<()> {
        copy_lines(
            BufReader::new(std::io::stdin().lock()),
            child_stdin,
            Direction::ClientToServer,
            &input_events,
            &input_sequence,
        )
    });
    let output_events = Arc::clone(&events);
    let output_sequence = Arc::clone(&sequence);
    let output = std::thread::spawn(move || -> std::io::Result<()> {
        copy_lines(
            BufReader::new(child_stdout),
            std::io::stdout().lock(),
            Direction::ServerToClient,
            &output_events,
            &output_sequence,
        )
    });

    let status = child.wait()?;
    output.join().map_err(|_| "MCP output proxy panicked")??;
    if input.is_finished() {
        input.join().map_err(|_| "MCP input proxy panicked")??;
    }

    let mut events = events
        .lock()
        .map_err(|_| "wire event recorder was poisoned")?
        .clone();
    events.sort_by_key(|event| event.sequence);
    let generated_at = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let tokenizer = Tokenizer::from_str(&args.tokenizer, false)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let repository = match (args.repository_revision, args.dirty_fingerprint) {
        (Some(revision), Some(dirty_fingerprint)) => Some(RepositoryIdentity {
            revision,
            dirty_fingerprint,
        }),
        (None, None) => None,
        _ => {
            return Err(
                "repository revision and dirty fingerprint must be supplied together".into(),
            );
        }
    };
    let mut trace = Trace {
        schema_version: TRACE_SCHEMA_V2,
        trace_id: Some(format!("stdio-{}", generated_at.as_millis())),
        trace_content_blake3: None,
        host: args.host,
        host_version: args.host_version,
        model: args.model,
        provider: args.provider,
        tokenizer: args.tokenizer,
        token_count_exact: Some(tokenizer.is_exact()),
        generated_at_unix_seconds: Some(generated_at.as_secs()),
        repository,
        final_turn: None,
        provider_usage: None,
        provider_total_input_tokens: None,
        outcome: None,
        events,
    };
    trace
        .seal_content_hash()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if let Some(parent) = args
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, serde_json::to_vec_pretty(&trace)?)?;
    if !status.success() {
        return Err(format!("MCP server exited with {status}").into());
    }
    Ok(())
}

fn copy_lines(
    mut reader: impl BufRead,
    mut writer: impl Write,
    direction: Direction,
    events: &Mutex<Vec<Event>>,
    sequence: &AtomicU64,
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
        let sequence = sequence.fetch_add(1, Ordering::Relaxed);
        let event = Event {
            sequence: Some(sequence),
            direction,
            turn: None,
            timestamp_unix_millis: Some(
                u64::try_from(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(std::io::Error::other)?
                        .as_millis(),
                )
                .unwrap_or(u64::MAX),
            ),
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
        events
            .lock()
            .map_err(|_| std::io::Error::other("wire event recorder was poisoned"))?
            .push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_preserves_bytes_and_records_exact_json() {
        let input = b"{ \"jsonrpc\": \"2.0\" }\r\n";
        let mut output = Vec::new();
        let events = Mutex::new(Vec::new());
        let sequence = AtomicU64::new(0);

        copy_lines(
            BufReader::new(&input[..]),
            &mut output,
            Direction::ClientToServer,
            &events,
            &sequence,
        )
        .expect("copy trace");

        assert_eq!(output, input);
        let events = events.into_inner().expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].raw_json.as_deref(),
            Some("{ \"jsonrpc\": \"2.0\" }")
        );
        assert_eq!(events[0].sequence, Some(0));
        assert!(events[0].timestamp_unix_millis.is_some());
    }
}
