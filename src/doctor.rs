//! Executable MCP readiness diagnostics for the current repository.

use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{Config, Error, Result};

const EXPECTED_TOOLS: [&str; 5] = [
    "leantoken_context",
    "leantoken_files",
    "leantoken_outline",
    "leantoken_read",
    "leantoken_search",
];
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Successful MCP self-diagnostic report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    /// Overall diagnostic status.
    pub status: &'static str,
    /// Canonical repository checked by the diagnostic.
    pub repository_root: std::path::PathBuf,
    /// MCP implementation name returned during initialization.
    pub server_name: String,
    /// MCP implementation version returned during initialization.
    pub server_version: String,
    /// Whether server-wide agent workflow guidance was present.
    pub instructions_loaded: bool,
    /// Exact MCP tool names exposed by the server.
    pub tools: Vec<String>,
    /// First-retrieval readiness result.
    pub first_call: FirstCallReport,
}

/// First retrieval outcome recorded by [`DoctorReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FirstCallReport {
    /// Final retrieval state.
    pub status: &'static str,
    /// Whether the first attempt observed asynchronous index warmup.
    pub warmed_index: bool,
    /// Number of attempts required to obtain a ready response.
    pub attempts: u64,
    /// Committed repository generation used by the ready response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_generation: Option<u64>,
}

/// Print a concise progress message before a potentially cold repository
/// index is checked. Progress goes to stderr so JSON stdout remains clean.
pub fn print_progress() -> Result<()> {
    let stderr = std::io::stderr();
    let mut output = stderr.lock();
    writeln!(
        output,
        "◇ Context Distillery is checking the MCP handshake and first retrieval..."
    )?;
    Ok(())
}

/// Launch the current executable as an MCP server and verify its public
/// first-run contract against the configured repository.
pub fn run(config: &Config) -> Result<DoctorReport> {
    let mut transport = DoctorTransport::spawn(config)?;
    transport.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {
                "name": "leantoken-doctor",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    }))?;
    let initialize = transport.response(1, RESPONSE_TIMEOUT)?;
    let result = result_object(&initialize, "initialize")?;
    let server_name = required_string(result, "/serverInfo/name", "server name")?;
    let server_version = required_string(result, "/serverInfo/version", "server version")?;
    if server_name != "leantoken" {
        return Err(doctor_error(format!(
            "MCP identified itself as {server_name:?}, expected \"leantoken\""
        )));
    }
    if server_version != env!("CARGO_PKG_VERSION") {
        return Err(doctor_error(format!(
            "MCP reported version {server_version}, expected {}",
            env!("CARGO_PKG_VERSION")
        )));
    }
    let instructions_loaded = result
        .get("instructions")
        .and_then(Value::as_str)
        .is_some_and(|instructions| {
            instructions.contains("call leantoken_context first")
                && instructions.contains("leantoken_search over grep or rg")
        });
    if !instructions_loaded {
        return Err(doctor_error(
            "MCP initialization omitted required agent workflow guidance",
        ));
    }

    transport.send(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }))?;
    transport.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }))?;
    let catalog = transport.response(2, RESPONSE_TIMEOUT)?;
    let catalog_result = result_object(&catalog, "tools/list")?;
    let tools = catalog_result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| doctor_error("tools/list did not return a tool array"))?
        .iter()
        .map(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| doctor_error("tools/list returned a tool without a name"))
        })
        .collect::<Result<Vec<_>>>()?;
    let actual = tools.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = EXPECTED_TOOLS.into_iter().collect::<BTreeSet<_>>();
    if actual != expected || tools.len() != EXPECTED_TOOLS.len() {
        return Err(doctor_error(format!(
            "unexpected MCP tool catalog: {}",
            tools.join(", ")
        )));
    }

    let deadline = Instant::now() + READY_TIMEOUT;
    let mut id = 3_u64;
    let mut attempts = 0_u64;
    let mut warmed_index = false;
    let repository_generation = loop {
        attempts += 1;
        transport.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "leantoken_context",
                "arguments": {
                    "task": "Verify LeanToken Context Distillery first-run readiness",
                    "token_budget": 200
                }
            }
        }))?;
        let response =
            transport.response(id, deadline.saturating_duration_since(Instant::now()))?;
        let call = result_object(&response, "leantoken_context")?;
        if call.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(doctor_error(format!(
                "first retrieval failed: {}",
                tool_message(call)
            )));
        }
        let structured = call
            .get("structuredContent")
            .ok_or_else(|| doctor_error("first retrieval omitted structuredContent"))?;
        if structured.get("status").and_then(Value::as_str) == Some("retryable") {
            warmed_index = true;
            if Instant::now() >= deadline {
                return Err(doctor_error(format!(
                    "repository index did not become ready within {} seconds",
                    READY_TIMEOUT.as_secs()
                )));
            }
            let retry_after = structured
                .get("retry_after_ms")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .clamp(10, 1_000);
            std::thread::sleep(Duration::from_millis(retry_after));
            id += 1;
            continue;
        }
        break structured
            .pointer("/meta/repository_generation")
            .and_then(Value::as_u64);
    };

    transport.close();
    Ok(DoctorReport {
        status: "ready",
        repository_root: config.root.clone(),
        server_name,
        server_version,
        instructions_loaded,
        tools,
        first_call: FirstCallReport {
            status: "ready",
            warmed_index,
            attempts,
            repository_generation,
        },
    })
}

/// Print a doctor report as JSON or Context Distillery terminal output.
pub fn print_report(report: &DoctorReport, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }

    writeln!(output, "◆ LeanToken // Context Distillery")?;
    writeln!(output, "  Repository: {}", report.repository_root.display())?;
    writeln!(
        output,
        "  ✓ MCP identity: {} {}",
        report.server_name, report.server_version
    )?;
    writeln!(output, "  ✓ Agent guidance loaded")?;
    writeln!(
        output,
        "  ✓ Tool catalog: {} retrieval tools",
        report.tools.len()
    )?;
    if report.first_call.warmed_index {
        writeln!(
            output,
            "  ✓ First retrieval: ready after index warmup ({} attempts)",
            report.first_call.attempts
        )?;
    } else {
        writeln!(output, "  ✓ First retrieval: ready")?;
    }
    writeln!(output)?;
    writeln!(
        output,
        "Ready. Distill broad tasks with leantoken_context first."
    )?;
    Ok(())
}

fn result_object<'a>(
    message: &'a Value,
    operation: &str,
) -> Result<&'a serde_json::Map<String, Value>> {
    if let Some(error) = message.get("error") {
        return Err(doctor_error(format!(
            "{operation} returned an MCP error: {error}"
        )));
    }
    message
        .get("result")
        .and_then(Value::as_object)
        .ok_or_else(|| doctor_error(format!("{operation} returned no result object")))
}

fn required_string(
    result: &serde_json::Map<String, Value>,
    pointer: &str,
    label: &str,
) -> Result<String> {
    Value::Object(result.clone())
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| doctor_error(format!("initialize result omitted {label}")))
}

fn tool_message(result: &serde_json::Map<String, Value>) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("unknown MCP tool error")
        .to_owned()
}

fn doctor_error(message: impl Into<String>) -> Error {
    Error::InvalidRequest(format!("doctor failed: {}", message.into()))
}

struct DoctorTransport {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
}

impl DoctorTransport {
    fn spawn(config: &Config) -> Result<Self> {
        let executable = std::env::current_exe()?.canonicalize()?;
        let mut child = std::process::Command::new(executable)
            .arg("--root")
            .arg(&config.root)
            .arg("--database")
            .arg(&config.database_path)
            .arg("--tokenizer")
            .arg(config.tokenizer.name())
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| doctor_error("could not open MCP stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| doctor_error("could not open MCP stdout"))?;
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            lines,
        })
    }

    fn send(&mut self, message: Value) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| doctor_error("MCP process stdin is closed"))?;
        serde_json::to_writer(&mut *stdin, &message)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    fn response(&self, id: u64, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(doctor_error(format!(
                    "timed out waiting for MCP response {id}"
                )));
            }
            let line = self.lines.recv_timeout(remaining).map_err(|error| {
                doctor_error(format!("MCP response {id} was unavailable: {error}"))
            })?;
            let message: Value = serde_json::from_str(&line)?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(message);
            }
        }
    }

    fn close(&mut self) {
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DoctorTransport {
    fn drop(&mut self) {
        self.stdin.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
