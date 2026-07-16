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
    let events = Arc::new(Mutex::new(Vec::new()));
    let sequence = Arc::new(AtomicU64::new(0));

    let input_events = Arc::clone(&events);
    let input_sequence = Arc::clone(&sequence);
    let input = std::thread::spawn(move || -> std::io::Result<()> {
        copy_lines(
            BufReader::new(std::io::stdin().lock()),
            child_stdin,
            "client_to_server",
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
            "server_to_client",
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
    let trace = Trace {
        schema_version: 1,
        host: args.host,
        host_version: args.host_version,
        tokenizer: args.tokenizer,
        generated_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        provider_total_input_tokens: None,
        events,
    };
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
    direction: &'static str,
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
        let event = Event {
            sequence: sequence.fetch_add(1, Ordering::Relaxed),
            direction,
            raw_json,
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
            "client_to_server",
            &events,
            &sequence,
        )
        .expect("copy trace");

        assert_eq!(output, input);
        let events = events.into_inner().expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].raw_json, "{ \"jsonrpc\": \"2.0\" }");
    }
}
