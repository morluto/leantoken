//! Executable MCP readiness diagnostics for the current repository.

use std::{
    collections::{BTreeSet, VecDeque},
    ffi::{OsStr, OsString},
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Stdio},
    sync::{Arc, Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::INDEX_CONTENT_VERSION;
use crate::mcp::McpResultMode;
use crate::setup::{self, SetupClient};
use crate::{Config, Error, Result};

const EXPECTED_TOOLS: [&str; 9] = [
    "context",
    "files",
    "history",
    "json",
    "outline",
    "read",
    "receipt_rebase",
    "savings",
    "search",
];
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const DIAGNOSTIC_WAIT_TIMEOUT: Duration = Duration::from_secs(1);
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
    /// Index-content compatibility version used by the executable.
    pub index_content_version: u32,
    /// Whether server-wide agent workflow guidance was present.
    pub instructions_loaded: bool,
    /// Exact MCP tool names exposed by the server.
    pub tools: Vec<String>,
    /// Effective static MCP result mode.
    pub result_mode: McpResultMode,
    /// Host registration and pre-session discovery state.
    pub integration: IntegrationReport,
    /// First-retrieval readiness result.
    pub first_call: FirstCallReport,
}

/// Structured host-integration status independent of repository readiness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationReport {
    /// Configured client whose exact launcher was exercised, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_client: Option<SetupClient>,
    /// `registered`, `not_registered`, or `unknown`.
    pub registration_status: &'static str,
    /// Clients with an existing LeanToken MCP registration.
    pub configured_clients: Vec<SetupClient>,
    /// Configured executable and version details for each LeanToken entry.
    pub registrations: Vec<RegistrationReport>,
    /// `current`, `stale`, `unmanaged_stale`, `not_registered`, or `unknown`.
    pub registration_health: &'static str,
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

/// One host registration as read from the supported client configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistrationReport {
    /// Host client owning the registration.
    pub client: SetupClient,
    /// Configuration file containing the entry.
    pub config_path: std::path::PathBuf,
    /// Executable configured for the host.
    pub command: String,
    /// Arguments configured for the host.
    pub args: Vec<String>,
    /// Release inferred from the configured command or package argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_version: Option<String>,
    /// Version of the executable used by this doctor invocation.
    pub expected_version: String,
    /// Whether command and arguments match the current launcher exactly.
    pub matches_current: bool,
    /// Whether setup ownership was explicit or recognized from a legacy launcher.
    pub managed: bool,
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
pub fn run(config: &Config, ready_timeout: Duration) -> Result<DoctorReport> {
    let mut transport = DoctorTransport::spawn(config)?;
    run_with_transport(config, ready_timeout, &mut transport, None)
}

/// Verify the exact launcher currently stored for one configured MCP client.
pub fn run_configured_client(
    config: &Config,
    ready_timeout: Duration,
    client: SetupClient,
) -> Result<DoctorReport> {
    let registration = setup::configured_registration(client)?.ok_or_else(|| {
        doctor_error(
            "registration",
            format!(
                "{} has no LeanToken MCP registration",
                client.display_name()
            ),
        )
    })?;
    let mut transport =
        DoctorTransport::spawn_launcher(config, &registration.command, &registration.args)?;
    run_with_transport(config, ready_timeout, &mut transport, Some(&registration))
}

/// Verify an exact setup launcher through the same MCP contract used by
/// [`run`]. Setup uses this after configuration so launcher verification cannot
/// drift from the public doctor behavior.
pub(crate) fn run_launcher(
    config: &Config,
    command: &str,
    args: &[String],
    ready_timeout: Duration,
) -> Result<DoctorReport> {
    let mut transport = DoctorTransport::spawn_launcher(config, command, args)?;
    run_with_transport(config, ready_timeout, &mut transport, None)
}

fn run_with_transport(
    config: &Config,
    ready_timeout: Duration,
    transport: &mut DoctorTransport,
    verified_registration: Option<&setup::ConfiguredRegistration>,
) -> Result<DoctorReport> {
    let expected_server_version = expected_server_version(verified_registration);
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
    required_string(result, "/protocolVersion", "protocol version", "handshake")?;
    let server_name = required_string(result, "/serverInfo/name", "server name", "handshake")?;
    let server_version =
        required_string(result, "/serverInfo/version", "server version", "handshake")?;
    if server_name != "leantoken" {
        return Err(doctor_error(
            "handshake",
            format!("MCP identified itself as {server_name:?}, expected \"leantoken\""),
        ));
    }
    if !server_version_matches_runtime(&server_version, expected_server_version) {
        return Err(doctor_error(
            "handshake",
            format!(
                "MCP reported version {server_version}, expected {}+contract.<32 hex characters>",
                expected_server_version
            ),
        ));
    }
    let instructions_loaded = result
        .get("instructions")
        .and_then(Value::as_str)
        .is_some_and(|instructions| {
            instructions.contains("call leantoken.savings directly")
                && instructions.contains("call leantoken.context once")
                && instructions.contains("plan_only=false")
                && instructions.contains("Reserve plan_only=true")
                && instructions.contains("leantoken.search over grep or rg")
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

    let deadline = Instant::now() + ready_timeout;
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
                    "name": "context",
                    "arguments": {
                        "task": "Verify LeanToken Context Distillery first-run readiness",
                        "token_budget": 200
                    }
                }
            }),
            "first_retrieval",
        )?;
        let response = match transport.response(
            id,
            deadline.saturating_duration_since(Instant::now()),
            "first_retrieval",
        ) {
            Ok(response) => response,
            Err(_error) if Instant::now() >= deadline => {
                return Err(doctor_error(
                    "first_retrieval",
                    format!(
                        "first retrieval did not become ready within {} seconds; the repository may still be indexing. Rerun with `--ready-timeout-seconds` or retry after indexing completes{}",
                        ready_timeout.as_secs(),
                        transport.diagnostic_context()
                    ),
                ));
            }
            Err(error) => return Err(error),
        };
        let call = result_object(&response, "context", "first_retrieval")?;
        if call.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(doctor_error(
                "first_retrieval",
                format!(
                    "first retrieval failed: {}{}",
                    tool_message(call),
                    transport.diagnostic_context_wait(DIAGNOSTIC_WAIT_TIMEOUT)
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
                        "repository index is still building; it did not become ready within {} seconds. Rerun with `--ready-timeout-seconds` or retry after indexing completes",
                        ready_timeout.as_secs()
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
    let expected_launcher = verified_registration.map(|registration| {
        (
            registration.command.as_str(),
            registration.args.as_slice(),
            expected_server_version,
        )
    });
    let setup = setup::diagnostic_state(expected_launcher);
    let verified_client = verified_registration.map(|registration| registration.client);
    let registrations = setup
        .registrations
        .iter()
        .map(|registration| RegistrationReport {
            client: registration.client,
            config_path: registration.path.clone(),
            command: registration.command.clone(),
            args: registration.args.clone(),
            configured_version: registration.version.clone(),
            expected_version: registration.expected_version.clone(),
            matches_current: registration.matches_current,
            managed: registration.managed,
        })
        .collect::<Vec<_>>();
    let registration_health = registration_health(setup.registration_status, &registrations);
    let repair_command = registration_repair_command(
        setup.registration_status,
        registration_health,
        &registrations,
    );
    let result_mode = McpResultMode::Structured;
    Ok(DoctorReport {
        status: "ready",
        repository_root: config.root.clone(),
        server_name,
        server_version,
        index_content_version: INDEX_CONTENT_VERSION,
        instructions_loaded,
        tools,
        result_mode,
        integration: IntegrationReport {
            verified_client,
            registration_status: setup.registration_status,
            configured_clients: setup.configured_clients,
            registrations,
            registration_health,
            discovery_status: setup.discovery_status,
            discovery_paths: setup.discovery_paths,
            launcher_status: "healthy",
            handshake_status: "healthy",
            catalog_status: "healthy",
            repair_command,
        },
        first_call: FirstCallReport {
            status: "ready",
            warmed_index,
            attempts,
            repository_generation,
        },
    })
}

fn registration_health(
    registration_status: &'static str,
    registrations: &[RegistrationReport],
) -> &'static str {
    if registrations.is_empty() {
        registration_status
    } else if registrations
        .iter()
        .all(|registration| registration.matches_current)
    {
        "current"
    } else if registrations
        .iter()
        .any(|registration| !registration.matches_current && !registration.managed)
    {
        "unmanaged_stale"
    } else {
        "stale"
    }
}

fn registration_repair_command(
    registration_status: &str,
    registration_health: &str,
    registrations: &[RegistrationReport],
) -> String {
    if registration_status == "not_registered" {
        return "leantoken setup --all --dry-run".into();
    }
    if registration_health == "unmanaged_stale" {
        let client_flags = registrations
            .iter()
            .filter(|registration| !registration.matches_current)
            .map(|registration| format!("--{}", registration.client.cli_name()))
            .collect::<Vec<_>>()
            .join(" ");
        return format!("leantoken setup {client_flags} --force-unmanaged --dry-run");
    }
    if registration_health == "stale" {
        "leantoken setup --refresh --yes".into()
    } else {
        "leantoken doctor --json".into()
    }
}

fn server_version_matches_runtime(version: &str, expected_version: &str) -> bool {
    version
        .strip_prefix(&format!("{expected_version}+contract."))
        .is_some_and(|fingerprint| {
            fingerprint.len() == 32 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn expected_server_version(verified_registration: Option<&setup::ConfiguredRegistration>) -> &str {
    verified_registration
        .and_then(|registration| registration.version.as_deref())
        .filter(|version| semver::Version::parse(version).is_ok())
        .unwrap_or(env!("CARGO_PKG_VERSION"))
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
    writeln!(
        output,
        "  ✓ Index compatibility: v{}",
        report.index_content_version
    )?;
    writeln!(output, "  ✓ Agent guidance loaded")?;
    writeln!(output, "  ✓ Tool catalog: {} MCP tools", report.tools.len())?;
    writeln!(output, "  ✓ Result mode: {:?}", report.result_mode)?;
    if let Some(client) = report.integration.verified_client {
        writeln!(
            output,
            "  ✓ Verified configured launcher: {}",
            client.display_name()
        )?;
    }
    writeln!(
        output,
        "  {} Host registration: {}",
        if report.integration.registration_health == "current" {
            "✓"
        } else {
            "◇"
        },
        report.integration.registration_health
    )?;
    for registration in &report.integration.registrations {
        let version = registration
            .configured_version
            .as_deref()
            .unwrap_or("unknown version");
        writeln!(
            output,
            "    {:?}: {} ({})",
            registration.client, registration.command, version
        )?;
    }
    if matches!(
        report.integration.registration_health,
        "stale" | "unmanaged_stale"
    ) {
        writeln!(
            output,
            "    Configured host entries do not match this executable; run {}.",
            report.integration.repair_command
        )?;
    }
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
        "Ready. Distill broad tasks with leantoken.context first."
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
    diagnostics: Arc<DiagnosticBuffer>,
}

#[derive(Default)]
struct DiagnosticBuffer {
    lines: Mutex<VecDeque<String>>,
    available: Condvar,
}

impl DiagnosticBuffer {
    fn push(&self, line: String) {
        let Ok(mut lines) = self.lines.lock() else {
            return;
        };
        if lines.len() == MAX_DIAGNOSTIC_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
        self.available.notify_all();
    }

    fn context(&self) -> String {
        let Ok(lines) = self.lines.lock() else {
            return String::new();
        };
        diagnostic_context(&lines)
    }

    fn wait_context(&self, timeout: Duration) -> String {
        let Ok(lines) = self.lines.lock() else {
            return String::new();
        };
        let Ok((lines, _)) = self
            .available
            .wait_timeout_while(lines, timeout, |lines| lines.is_empty())
        else {
            return String::new();
        };
        diagnostic_context(&lines)
    }
}

fn diagnostic_context(lines: &VecDeque<String>) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!(
            "; child diagnostics: {}",
            lines.iter().cloned().collect::<Vec<_>>().join(" | ")
        )
    }
}

fn launcher_arguments(config: &Config, args: &[String]) -> Result<Vec<OsString>> {
    let Some(mcp_index) = args.iter().rposition(|argument| argument == "mcp") else {
        return Err(doctor_error(
            "launch",
            "configured launcher does not contain the `mcp` subcommand",
        ));
    };
    let mut launch_args = args.iter().map(OsString::from).collect::<Vec<_>>();
    launch_args.splice(
        mcp_index..mcp_index,
        [
            "--root".into(),
            config.root.as_os_str().to_owned(),
            "--database".into(),
            config.database_path.as_os_str().to_owned(),
            "--tokenizer".into(),
            config.tokenizer.name().into(),
        ],
    );
    launch_args.extend(["--result-mode".into(), "structured".into()]);
    Ok(launch_args)
}

impl DoctorTransport {
    fn spawn(config: &Config) -> Result<Self> {
        let executable = std::env::current_exe()
            .and_then(|path| path.canonicalize())
            .map_err(|error| doctor_error("launch", error.to_string()))?;
        Self::spawn_command(config, executable.as_os_str(), &["mcp".into()])
    }

    fn spawn_launcher(config: &Config, command: &str, args: &[String]) -> Result<Self> {
        Self::spawn_command(config, OsStr::new(command), args)
    }

    fn spawn_command(config: &Config, command: &OsStr, args: &[String]) -> Result<Self> {
        let launch_args = launcher_arguments(config, args)?;
        let mut child = std::process::Command::new(command)
            .args(&launch_args)
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
        let diagnostics = Arc::new(DiagnosticBuffer::default());
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
                diagnostic_lines.push(line);
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
        self.diagnostics.context()
    }

    fn diagnostic_context_wait(&self, timeout: Duration) -> String {
        self.diagnostics.wait_context(timeout)
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

    #[cfg(unix)]
    #[test]
    fn launcher_arguments_preserve_non_utf8_repository_paths() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let root = tempfile::tempdir().expect("repository");
        let mut config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        config.root = root
            .path()
            .join(OsString::from_vec(b"source-\xff".to_vec()));
        config.database_path = root
            .path()
            .join(OsString::from_vec(b"index-\xfe.sqlite".to_vec()));

        let arguments = launcher_arguments(&config, &["mcp".into()]).expect("launcher arguments");

        assert!(
            arguments
                .iter()
                .any(|argument| { argument.as_os_str().as_bytes().ends_with(b"source-\xff") })
        );
        assert!(arguments.iter().any(|argument| {
            argument
                .as_os_str()
                .as_bytes()
                .ends_with(b"index-\xfe.sqlite")
        }));
    }

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
    fn unmanaged_stale_registration_prescribes_a_client_scoped_override_preview() {
        let registration = |client, matches_current, managed| RegistrationReport {
            client,
            config_path: std::path::PathBuf::from("config"),
            command: "/opt/manual-leantoken".into(),
            args: vec!["mcp".into()],
            configured_version: None,
            expected_version: env!("CARGO_PKG_VERSION").into(),
            matches_current,
            managed,
        };
        let registrations = vec![
            registration(SetupClient::Codex, false, false),
            registration(SetupClient::Cursor, false, true),
        ];

        let health = registration_health("registered", &registrations);

        assert_eq!(health, "unmanaged_stale");
        assert_eq!(
            registration_repair_command("registered", health, &registrations),
            "leantoken setup --codex --cursor --force-unmanaged --dry-run"
        );
        let managed = vec![registration(SetupClient::Claude, false, true)];
        assert_eq!(registration_health("registered", &managed), "stale");
        assert_eq!(
            registration_repair_command("registered", "stale", &managed),
            "leantoken setup --refresh --yes"
        );
    }

    #[test]
    fn runtime_version_requires_the_expected_semver_and_bounded_schema_fingerprint() {
        let current = concat!(
            env!("CARGO_PKG_VERSION"),
            "+contract.0123456789abcdef0123456789abcdef"
        );
        let pinned = "0.1.19+contract.0123456789abcdef0123456789abcdef";
        assert!(server_version_matches_runtime(
            current,
            env!("CARGO_PKG_VERSION")
        ));
        assert!(server_version_matches_runtime(pinned, "0.1.19"));
        assert!(!server_version_matches_runtime(
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_VERSION")
        ));
        assert!(!server_version_matches_runtime(
            concat!(env!("CARGO_PKG_VERSION"), "+contract.short"),
            env!("CARGO_PKG_VERSION")
        ));
        assert!(!server_version_matches_runtime(pinned, "0.1.18"));
    }

    #[test]
    fn configured_doctor_expects_the_stored_launcher_version() {
        let registration = setup::ConfiguredRegistration {
            client: SetupClient::Codex,
            path: "config.toml".into(),
            command: "npx".into(),
            args: vec!["--package=leantoken@0.1.19".into()],
            version: Some("0.1.19".into()),
            expected_version: env!("CARGO_PKG_VERSION").into(),
            matches_current: false,
            managed: true,
        };

        assert_eq!(expected_server_version(Some(&registration)), "0.1.19");
        assert_eq!(expected_server_version(None), env!("CARGO_PKG_VERSION"));

        let mut floating = registration;
        floating.version = Some("latest".into());
        assert_eq!(
            expected_server_version(Some(&floating)),
            env!("CARGO_PKG_VERSION")
        );
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

    #[test]
    fn diagnostic_buffer_waits_for_delayed_child_failure_and_remains_bounded() {
        let delayed = Arc::new(DiagnosticBuffer::default());
        let writer_buffer = Arc::clone(&delayed);
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            writer_buffer.push("MCP indexing runtime failed".into());
        });

        let context = delayed.wait_context(Duration::from_secs(1));
        writer.join().expect("diagnostic writer");
        assert!(context.contains("MCP indexing runtime failed"));

        let bounded = DiagnosticBuffer::default();
        for index in 0..=MAX_DIAGNOSTIC_LINES {
            bounded.push(format!("earlier diagnostic {index}"));
        }
        let settled = bounded.context();

        assert!(settled.contains(&format!("earlier diagnostic {MAX_DIAGNOSTIC_LINES}")));
        assert!(!settled.contains("earlier diagnostic 0"));
        assert_eq!(settled.matches(" | ").count(), MAX_DIAGNOSTIC_LINES - 1);
    }
}
