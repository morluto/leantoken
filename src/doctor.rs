//! Executable MCP readiness diagnostics for the current repository.

use std::{
    collections::{BTreeSet, VecDeque},
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::{Value, json};

use crate::setup::{self, SetupClient};
use crate::{Config, Error, Result};

const EXPECTED_TOOLS: [&str; 6] = [
    "leantoken_context",
    "leantoken_files",
    "leantoken_outline",
    "leantoken_read",
    "leantoken_savings",
    "leantoken_search",
];
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_DIAGNOSTIC_LINES: usize = 8;
const MAX_DIAGNOSTIC_LINE_CHARS: usize = 512;
const MAX_DIAGNOSTIC_LINE_BYTES: usize = MAX_DIAGNOSTIC_LINE_CHARS * 4;

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
    /// Host registration and pre-session discovery state.
    pub integration: IntegrationReport,
    /// First-retrieval readiness result.
    pub first_call: FirstCallReport,
}

/// Structured host-integration status independent of repository readiness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationReport {
    /// `registered`, `not_registered`, or `unknown`.
    pub registration_status: &'static str,
    /// Clients with an existing LeanToken MCP registration.
    pub configured_clients: Vec<SetupClient>,
    /// `installed`, `partial`, `missing`, or `unknown`.
    pub discovery_status: &'static str,
    /// LeanToken-owned skill descriptors found on disk.
    pub discovery_paths: Vec<std::path::PathBuf>,
    /// Native child process launch state.
    pub launcher_status: &'static str,
    /// MCP initialize exchange state.
    pub handshake_status: &'static str,
    /// Static MCP tool catalog state.
    pub catalog_status: &'static str,
    /// Actionable exact-version verification command.
    pub repair_command: String,
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
    transport.send(
        json!({
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
        }),
        "handshake",
    )?;
    let initialize = transport.response(1, RESPONSE_TIMEOUT, "handshake")?;
    let result = result_object(&initialize, "initialize", "handshake")?;
    let server_name = required_string(result, "/serverInfo/name", "server name", "handshake")?;
    let server_version =
        required_string(result, "/serverInfo/version", "server version", "handshake")?;
    if server_name != "leantoken" {
        return Err(doctor_error(
            "handshake",
            format!("MCP identified itself as {server_name:?}, expected \"leantoken\""),
        ));
    }
    if server_version != env!("CARGO_PKG_VERSION") {
        return Err(doctor_error(
            "handshake",
            format!(
                "MCP reported version {server_version}, expected {}",
                env!("CARGO_PKG_VERSION")
            ),
        ));
    }
    let instructions_loaded = result
        .get("instructions")
        .and_then(Value::as_str)
        .is_some_and(|instructions| {
            instructions.contains("call leantoken_savings directly")
                && instructions.contains("call leantoken_context first")
                && instructions.contains("leantoken_search over grep or rg")
        });
    if !instructions_loaded {
        return Err(doctor_error(
            "handshake",
            "MCP initialization omitted required agent workflow guidance",
        ));
    }

    transport.send(
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
        "handshake",
    )?;
    transport.send(
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
        "catalog",
    )?;
    let catalog = transport.response(2, RESPONSE_TIMEOUT, "catalog")?;
    let catalog_result = result_object(&catalog, "tools/list", "catalog")?;
    let tools = catalog_result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| doctor_error("catalog", "tools/list did not return a tool array"))?
        .iter()
        .map(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| doctor_error("catalog", "tools/list returned a tool without a name"))
        })
        .collect::<Result<Vec<_>>>()?;
    let actual = tools.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = EXPECTED_TOOLS.into_iter().collect::<BTreeSet<_>>();
    if actual != expected || tools.len() != EXPECTED_TOOLS.len() {
        return Err(doctor_error(
            "catalog",
            format!("unexpected MCP tool catalog: {}", tools.join(", ")),
        ));
    }

    let deadline = Instant::now() + READY_TIMEOUT;
    let mut id = 3_u64;
    let mut attempts = 0_u64;
    let mut warmed_index = false;
    let repository_generation = loop {
        attempts += 1;
        transport.send(
            json!({
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
            }),
            "first_retrieval",
        )?;
        let response = transport.response(
            id,
            deadline.saturating_duration_since(Instant::now()),
            "first_retrieval",
        )?;
        let call = result_object(&response, "leantoken_context", "first_retrieval")?;
        if call.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(doctor_error(
                "first_retrieval",
                format!(
                    "first retrieval failed: {}{}",
                    tool_message(call),
                    transport.diagnostic_context()
                ),
            ));
        }
        let structured = call.get("structuredContent").ok_or_else(|| {
            doctor_error(
                "first_retrieval",
                "first retrieval omitted structuredContent",
            )
        })?;
        if structured.get("status").and_then(Value::as_str) == Some("retryable") {
            warmed_index = true;
            if Instant::now() >= deadline {
                return Err(doctor_error(
                    "first_retrieval",
                    format!(
                        "repository index did not become ready within {} seconds",
                        READY_TIMEOUT.as_secs()
                    ),
                ));
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
    let setup = setup::diagnostic_state();
    Ok(DoctorReport {
        status: "ready",
        repository_root: config.root.clone(),
        server_name,
        server_version,
        instructions_loaded,
        tools,
        integration: IntegrationReport {
            registration_status: setup.registration_status,
            configured_clients: setup.configured_clients,
            discovery_status: setup.discovery_status,
            discovery_paths: setup.discovery_paths,
            launcher_status: "healthy",
            handshake_status: "healthy",
            catalog_status: "healthy",
            repair_command: if setup.registration_status == "not_registered" {
                "leantoken setup --all --dry-run".into()
            } else {
                "leantoken doctor --json".into()
            },
        },
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
    writeln!(output, "  ✓ Tool catalog: {} MCP tools", report.tools.len())?;
    writeln!(
        output,
        "  {} Host registration: {}",
        if report.integration.registration_status == "registered" {
            "✓"
        } else {
            "◇"
        },
        report.integration.registration_status
    )?;
    writeln!(
        output,
        "  {} Agent discovery: {}",
        if report.integration.discovery_status == "installed" {
            "✓"
        } else {
            "◇"
        },
        report.integration.discovery_status
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
    stage: &'static str,
) -> Result<&'a serde_json::Map<String, Value>> {
    if let Some(error) = message.get("error") {
        return Err(doctor_error(
            stage,
            format!("{operation} returned an MCP error: {error}"),
        ));
    }
    message
        .get("result")
        .and_then(Value::as_object)
        .ok_or_else(|| doctor_error(stage, format!("{operation} returned no result object")))
}

fn required_string(
    result: &serde_json::Map<String, Value>,
    pointer: &str,
    label: &str,
    stage: &'static str,
) -> Result<String> {
    Value::Object(result.clone())
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| doctor_error(stage, format!("initialize result omitted {label}")))
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

fn doctor_error(stage: &'static str, message: impl Into<String>) -> Error {
    Error::DoctorFailure {
        stage,
        message: message.into(),
    }
}

struct DoctorTransport {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
}

impl DoctorTransport {
    fn spawn(config: &Config) -> Result<Self> {
        let executable = std::env::current_exe()
            .and_then(|path| path.canonicalize())
            .map_err(|error| doctor_error("launch", error.to_string()))?;
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
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| doctor_error("launch", error.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| doctor_error("launch", "could not open MCP stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| doctor_error("launch", "could not open MCP stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| doctor_error("launch", "could not open MCP stderr"))?;
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let diagnostics = Arc::new(Mutex::new(VecDeque::new()));
        let diagnostic_lines = Arc::clone(&diagnostics);
        let redactions = [
            config.root.to_string_lossy().into_owned(),
            config.database_path.to_string_lossy().into_owned(),
        ];
        std::thread::spawn(move || {
            let _ = read_bounded_diagnostic_lines(BufReader::new(stderr), |bytes| {
                let line = String::from_utf8_lossy(bytes);
                let line = sanitize_diagnostic_line(&line, &redactions);
                if line.is_empty() {
                    return;
                }
                let Ok(mut lines) = diagnostic_lines.lock() else {
                    return;
                };
                if lines.len() == MAX_DIAGNOSTIC_LINES {
                    lines.pop_front();
                }
                lines.push_back(line);
            });
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            lines,
            diagnostics,
        })
    }

    fn send(&mut self, message: Value, stage: &'static str) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| doctor_error(stage, "MCP process stdin is closed"))?;
        serde_json::to_writer(&mut *stdin, &message)
            .map_err(|error| doctor_error(stage, error.to_string()))?;
        stdin
            .write_all(b"\n")
            .and_then(|()| stdin.flush())
            .map_err(|error| doctor_error(stage, error.to_string()))?;
        Ok(())
    }

    fn response(&self, id: u64, timeout: Duration, stage: &'static str) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(doctor_error(
                    stage,
                    format!("timed out waiting for MCP response {id}"),
                ));
            }
            let line = self.lines.recv_timeout(remaining).map_err(|error| {
                doctor_error(
                    stage,
                    format!(
                        "MCP response {id} was unavailable: {error}{}",
                        self.diagnostic_context()
                    ),
                )
            })?;
            let message: Value = serde_json::from_str(&line)
                .map_err(|error| doctor_error(stage, error.to_string()))?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(message);
            }
        }
    }

    fn diagnostic_context(&self) -> String {
        let Ok(lines) = self.diagnostics.lock() else {
            return String::new();
        };
        if lines.is_empty() {
            String::new()
        } else {
            format!(
                "; child diagnostics: {}",
                lines.iter().cloned().collect::<Vec<_>>().join(" | ")
            )
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

fn read_bounded_diagnostic_lines(
    mut reader: impl BufRead,
    mut consume_line: impl FnMut(&[u8]),
) -> std::io::Result<()> {
    let mut line = Vec::with_capacity(MAX_DIAGNOSTIC_LINE_BYTES);
    loop {
        let (consumed, line_ended) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if !line.is_empty() {
                    consume_line(&line);
                }
                return Ok(());
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let content_end = newline.unwrap_or(available.len());
            let retained = (MAX_DIAGNOSTIC_LINE_BYTES - line.len()).min(content_end);
            line.extend_from_slice(&available[..retained]);
            (
                newline.map_or(available.len(), |index| index + 1),
                newline.is_some(),
            )
        };
        reader.consume(consumed);
        if line_ended {
            consume_line(&line);
            line.clear();
        }
    }
}

fn sanitize_diagnostic_line(line: &str, redactions: &[String]) -> String {
    let mut sanitized = line
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    for value in redactions.iter().filter(|value| !value.is_empty()) {
        sanitized = sanitized.replace(value, "<redacted-path>");
    }
    sanitized
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_DIAGNOSTIC_LINE_CHARS)
        .collect()
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn child_diagnostics_are_bounded_and_redact_configured_paths() {
        let path = "/private/repository";
        let line = format!("error opening {path}: {}", "x".repeat(1_000));

        let sanitized = sanitize_diagnostic_line(&line, &[path.to_string()]);

        assert!(sanitized.contains("<redacted-path>"));
        assert!(!sanitized.contains(path));
        assert_eq!(sanitized.chars().count(), MAX_DIAGNOSTIC_LINE_CHARS);
    }

    #[test]
    fn child_diagnostic_reader_discards_oversized_line_remainders() {
        let mut input = vec![b'x'; MAX_DIAGNOSTIC_LINE_BYTES * 4];
        input.extend_from_slice(b"\nnext\n");
        let mut lines = Vec::new();

        read_bounded_diagnostic_lines(Cursor::new(input), |line| lines.push(line.to_vec()))
            .expect("read diagnostics");

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), MAX_DIAGNOSTIC_LINE_BYTES);
        assert_eq!(lines[1], b"next");
    }
}
