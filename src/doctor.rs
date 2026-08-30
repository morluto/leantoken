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
const V0_1_17_TO_V0_1_18_TOOLS: [&str; 8] = [
    "context", "files", "history", "json", "outline", "read", "savings", "search",
];
const V0_1_19_TOOLS: [&str; 9] = [
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
const COMPATIBLE_REQUIRED_TOOLS: [&str; 8] = [
    "context", "files", "history", "json", "outline", "read", "savings", "search",
];
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const DIAGNOSTIC_WAIT_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_DIAGNOSTIC_LINES: usize = 8;
const MAX_DIAGNOSTIC_LINE_CHARS: usize = 512;
const MAX_DIAGNOSTIC_LINE_BYTES: usize = MAX_DIAGNOSTIC_LINE_CHARS * 4;
const MAX_PROTOCOL_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PROTOCOL_QUEUED_RECORDS: usize = 4;

/// Successful MCP self-diagnostic report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    /// Overall diagnostic status.
    pub status: &'static str,
    /// Version of the executable running this doctor process.
    pub process_version: &'static str,
    /// Canonical repository checked by the diagnostic.
    pub repository_root: std::path::PathBuf,
    /// MCP implementation name returned during initialization.
    pub server_name: String,
    /// MCP implementation version returned during initialization.
    pub server_version: String,
    /// Index-content compatibility version, when the verified child exposes a
    /// version this process can identify.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_content_version: Option<u32>,
    /// Exact persisted-index derivation identity for this process's own launcher.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_derivation_fingerprint: Option<String>,
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
    /// `current`, `disabled`, `stale`, `unmanaged_stale`, `not_registered`, or
    /// `unknown`.
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
    /// Explicit release pin read from the client configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_version_pin: Option<String>,
    /// Version the current doctor process expects for a freshly managed launcher.
    pub expected_version: String,
    /// Whether command and arguments match the current launcher exactly.
    pub matches_current: bool,
    /// Whether setup ownership was explicit or recognized from a legacy launcher.
    pub managed: bool,
    /// Whether the host client will launch this registration.
    pub enabled: bool,
}

impl From<&setup::ConfiguredRegistration> for RegistrationReport {
    fn from(registration: &setup::ConfiguredRegistration) -> Self {
        Self {
            client: registration.client,
            config_path: registration.path.clone(),
            command: registration.command.clone(),
            args: registration.args.clone(),
            configured_version: registration.version.clone(),
            configured_version_pin: registration.version.clone(),
            expected_version: registration.expected_version.clone(),
            matches_current: registration.matches_current,
            managed: registration.managed,
            enabled: registration.enabled,
        }
    }
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
    run_with_transport(
        config,
        ready_timeout,
        &mut transport,
        None,
        McpResultMode::Structured,
    )
}

/// Verify the exact launcher currently stored for one configured MCP client.
pub fn run_configured_client(
    config: &Config,
    ready_timeout: Duration,
    client: SetupClient,
) -> Result<DoctorReport> {
    let registration = setup::configured_registration(client)
        .map_err(|error| doctor_error("registration", error.to_string()))?
        .ok_or_else(|| {
            doctor_error(
                "registration",
                format!(
                    "{} has no LeanToken MCP registration",
                    client.display_name()
                ),
            )
        })?;
    if !registration.enabled {
        return Err(doctor_error(
            "registration",
            format!(
                "{} has a disabled LeanToken MCP registration",
                client.display_name()
            ),
        ));
    }
    let mut transport = DoctorTransport::spawn_launcher(
        config,
        &registration.command,
        &registration.args,
        DatabaseForwarding::ExplicitOnly,
    )?;
    run_with_transport(
        config,
        ready_timeout,
        &mut transport,
        Some(&registration),
        result_mode_from_arguments(&registration.args),
    )
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
    let mut transport =
        DoctorTransport::spawn_launcher(config, command, args, DatabaseForwarding::Resolved)?;
    run_with_transport(
        config,
        ready_timeout,
        &mut transport,
        None,
        result_mode_from_arguments(args),
    )
}

fn run_with_transport(
    config: &Config,
    ready_timeout: Duration,
    transport: &mut DoctorTransport,
    verified_registration: Option<&setup::ConfiguredRegistration>,
    result_mode: McpResultMode,
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
    let initialize = transport.response(
        1,
        configured_handshake_timeout(verified_registration),
        "handshake",
    )?;
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
        let expected = expected_server_version.map_or_else(
            || "a compatible semantic release with a 32-hex contract fingerprint".to_owned(),
            |version| {
                format!(
                    "{version}+{}.<32 hex characters>",
                    version_marker_for_release(version)
                )
            },
        );
        return Err(doctor_error(
            "handshake",
            format!("MCP reported version {server_version}, expected {expected}"),
        ));
    }
    let server_release = server_version_release(&server_version)
        .expect("a matching runtime version has a semantic release");
    let instructions_loaded = result
        .get("instructions")
        .and_then(Value::as_str)
        .is_some_and(|instructions| instructions_match_release(instructions, server_release));
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
    if !catalog_matches_release(&tools, server_release) {
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
        let result = model_visible_result(call, result_mode)?;
        if result.get("status").and_then(Value::as_str) == Some("retryable") {
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
            let retry_after = result
                .get("retry_after_ms")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .clamp(10, 1_000);
            std::thread::sleep(Duration::from_millis(retry_after));
            id += 1;
            continue;
        }
        break result
            .pointer("/meta/repository_generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                doctor_error(
                    "first_retrieval",
                    "ready response omitted committed repository_generation",
                )
            })?;
    };

    transport.close();
    if let Some(registration) = verified_registration {
        let current = setup::configured_registration(registration.client)
            .map_err(|error| doctor_error("registration", error.to_string()))?;
        if !current.is_some_and(|current| {
            current.path == registration.path && current.source_hash == registration.source_hash
        }) {
            return Err(doctor_error(
                "registration",
                format!(
                    "{} MCP registration changed while its launcher was being verified",
                    registration.client.display_name()
                ),
            ));
        }
    }
    let expected_launcher = verified_registration.map(|registration| {
        (
            registration.command.as_str(),
            registration.args.as_slice(),
            expected_server_version.unwrap_or(server_release),
        )
    });
    let setup = setup::diagnostic_state(expected_launcher);
    let verified_client = verified_registration.map(|registration| registration.client);
    let registrations = setup
        .registrations
        .iter()
        .map(RegistrationReport::from)
        .collect::<Vec<_>>();
    let registration_health = registration_health(setup.registration_status, &registrations);
    let repair_command = registration_repair_command(
        setup.registration_status,
        registration_health,
        &registrations,
    );
    Ok(DoctorReport {
        status: "ready",
        process_version: env!("CARGO_PKG_VERSION"),
        repository_root: config.root.clone(),
        server_name,
        server_version,
        // The MCP child contract does not currently disclose its index schema.
        // A configured launcher can target another exact release, so only
        // report the value for the current executable's own launcher.
        index_content_version: verified_registration
            .is_none()
            .then_some(INDEX_CONTENT_VERSION),
        index_derivation_fingerprint: verified_registration
            .is_none()
            .then(|| crate::index_derivation::index_derivation_fingerprint().to_owned()),
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
            repository_generation: Some(repository_generation),
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
        .any(|registration| !registration.enabled)
    {
        "disabled"
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
    if registration_health == "disabled" {
        let affected = registrations
            .iter()
            .filter(|registration| !registration.enabled || !registration.matches_current)
            .collect::<Vec<_>>();
        if affected.iter().any(|registration| !registration.managed) {
            let client_flags = affected
                .into_iter()
                .map(|registration| format!("--{}", registration.client.cli_name()))
                .collect::<Vec<_>>();
            return format!(
                "leantoken setup {} --force-unmanaged --dry-run",
                client_flags.join(" ")
            );
        }
    }
    if matches!(registration_health, "disabled" | "stale") {
        "leantoken setup --refresh --yes".into()
    } else {
        "leantoken doctor --json".into()
    }
}

fn server_version_release(version: &str) -> Option<&str> {
    let (release, fingerprint, marker) =
        if let Some((release, fingerprint)) = version.split_once("+contract.") {
            (release, fingerprint, "contract")
        } else {
            let (release, fingerprint) = version.split_once("+schema.")?;
            (release, fingerprint, "schema")
        };
    (semver::Version::parse(release).is_ok()
        && marker == version_marker_for_release(release)
        && fingerprint.len() == 32
        && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then_some(release)
}

fn version_marker_for_release(release: &str) -> &'static str {
    match release {
        "0.1.17" | "0.1.18" | "0.1.19" => "schema",
        _ => "contract",
    }
}

fn instructions_match_release(instructions: &str, release: &str) -> bool {
    let compact_guidance = semver::Version::parse(release).is_ok_and(|release| {
        semver::Version::parse(env!("CARGO_PKG_VERSION")).is_ok_and(|current| release >= current)
    });
    if compact_guidance {
        return instructions.contains("Use savings for token statistics")
            && instructions.contains("plan_only=false")
            && instructions.contains("For a known scope")
            && instructions.contains("call leantoken.context once");
    }
    instructions.contains("call leantoken.savings directly")
        && instructions.contains("plan_only=false")
        && instructions.contains("leantoken.search over grep or rg")
        && if release == "0.1.17" {
            instructions.contains("call leantoken.context first")
                && instructions.contains("context plan_only=true")
        } else {
            instructions.contains("call leantoken.context once")
                && instructions.contains("Reserve plan_only=true")
        }
}

fn catalog_matches_release(tools: &[String], release: &str) -> bool {
    let actual = tools.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if actual.len() != tools.len() {
        return false;
    }
    let exact = match release {
        "0.1.17" | "0.1.18" => Some(V0_1_17_TO_V0_1_18_TOOLS.as_slice()),
        "0.1.19" => Some(V0_1_19_TOOLS.as_slice()),
        env!("CARGO_PKG_VERSION") => Some(EXPECTED_TOOLS.as_slice()),
        _ => None,
    };
    exact.map_or_else(
        || {
            COMPATIBLE_REQUIRED_TOOLS
                .iter()
                .all(|name| actual.contains(name))
        },
        |expected| {
            tools.len() == expected.len()
                && actual == expected.iter().copied().collect::<BTreeSet<_>>()
        },
    )
}

fn server_version_matches_runtime(version: &str, expected_version: Option<&str>) -> bool {
    server_version_release(version).is_some_and(|release| {
        expected_version.is_none_or(|expected_version| release == expected_version)
    })
}

fn expected_server_version(
    verified_registration: Option<&setup::ConfiguredRegistration>,
) -> Option<&str> {
    match verified_registration {
        Some(registration) => registration
            .version
            .as_deref()
            .filter(|version| semver::Version::parse(version).is_ok()),
        None => Some(env!("CARGO_PKG_VERSION")),
    }
}

fn configured_handshake_timeout(
    verified_registration: Option<&setup::ConfiguredRegistration>,
) -> Duration {
    verified_registration
        .and_then(|registration| registration.startup_timeout_seconds)
        .map(Duration::from_secs)
        .map_or(RESPONSE_TIMEOUT, |timeout| timeout.min(RESPONSE_TIMEOUT))
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
        "  ✓ Doctor process: leantoken {}",
        report.process_version
    )?;
    writeln!(
        output,
        "  ✓ MCP identity: {} {}",
        report.server_name, report.server_version
    )?;
    if let Some(index_content_version) = report.index_content_version {
        writeln!(output, "  ✓ Index compatibility: v{index_content_version}")?;
    } else {
        writeln!(
            output,
            "  ◇ Index compatibility: not disclosed by configured launcher"
        )?;
    }
    if let Some(fingerprint) = &report.index_derivation_fingerprint {
        writeln!(output, "  ✓ Index derivation: {fingerprint}")?;
    }
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
            "    {:?}: {} (config pin: {})",
            registration.client, registration.command, version
        )?;
    }
    if matches!(
        report.integration.registration_health,
        "disabled" | "stale" | "unmanaged_stale"
    ) {
        writeln!(
            output,
            "    Configured host entries are not launch-ready; run {}.",
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

fn model_visible_result(
    call: &serde_json::Map<String, Value>,
    result_mode: McpResultMode,
) -> Result<Value> {
    match result_mode {
        McpResultMode::Structured | McpResultMode::Dual => {
            call.get("structuredContent").cloned().ok_or_else(|| {
                doctor_error(
                    "first_retrieval",
                    "first retrieval omitted structuredContent",
                )
            })
        }
        McpResultMode::Text => {
            let text = call
                .get("content")
                .and_then(Value::as_array)
                .and_then(|content| {
                    content.iter().find_map(|item| {
                        item.get("text")
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                    })
                })
                .ok_or_else(|| {
                    doctor_error("first_retrieval", "first retrieval omitted text content")
                })?;
            serde_json::from_str(text).map_err(|error| {
                doctor_error(
                    "first_retrieval",
                    format!("first retrieval text content was not JSON: {error}"),
                )
            })
        }
    }
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
    lines: mpsc::Receiver<std::io::Result<String>>,
    diagnostics: Arc<DiagnosticBuffer>,
}

type ProtocolRecord = std::io::Result<String>;

fn protocol_channel() -> (
    mpsc::SyncSender<ProtocolRecord>,
    mpsc::Receiver<ProtocolRecord>,
) {
    mpsc::sync_channel(MAX_PROTOCOL_QUEUED_RECORDS)
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

#[derive(Clone, Copy)]
enum DatabaseForwarding {
    Resolved,
    ExplicitOnly,
}

fn launcher_arguments(
    config: &Config,
    args: &[String],
    database_forwarding: DatabaseForwarding,
) -> Result<Vec<OsString>> {
    let Some(mcp_index) = args.iter().rposition(|argument| argument == "mcp") else {
        return Err(doctor_error(
            "launch",
            "configured launcher does not contain the `mcp` subcommand",
        ));
    };
    let mut launch_args = args.iter().map(OsString::from).collect::<Vec<_>>();
    let mut global_args = vec!["--root".into(), config.root.as_os_str().to_owned()];
    if matches!(database_forwarding, DatabaseForwarding::Resolved)
        || !config.database_is_managed_cache()
    {
        global_args.extend([
            "--database".into(),
            config.database_path.as_os_str().to_owned(),
        ]);
    }
    global_args.extend(["--tokenizer".into(), config.tokenizer.name().into()]);
    for pattern in config.index_scope().includes() {
        global_args.extend(["--index-include".into(), OsString::from(pattern)]);
    }
    for pattern in config.index_scope().excludes() {
        global_args.extend(["--index-exclude".into(), OsString::from(pattern)]);
    }
    launch_args.splice(mcp_index..mcp_index, global_args);
    if !launch_args.iter().any(|argument| {
        argument == "--result-mode" || argument.to_string_lossy().starts_with("--result-mode=")
    }) {
        launch_args.extend(["--result-mode".into(), "structured".into()]);
    }
    Ok(launch_args)
}

fn result_mode_from_arguments(args: &[String]) -> McpResultMode {
    args.windows(2)
        .rev()
        .find_map(|arguments| {
            (arguments[0] == "--result-mode").then(|| match arguments[1].as_str() {
                "dual" => McpResultMode::Dual,
                "text" => McpResultMode::Text,
                _ => McpResultMode::Structured,
            })
        })
        .or_else(|| {
            args.iter().rev().find_map(|argument| {
                argument
                    .strip_prefix("--result-mode=")
                    .map(|value| match value {
                        "dual" => McpResultMode::Dual,
                        "text" => McpResultMode::Text,
                        _ => McpResultMode::Structured,
                    })
            })
        })
        .unwrap_or(McpResultMode::Structured)
}

impl DoctorTransport {
    fn spawn(config: &Config) -> Result<Self> {
        let executable = std::env::current_exe()
            .and_then(|path| path.canonicalize())
            .map_err(|error| doctor_error("launch", error.to_string()))?;
        Self::spawn_command(
            config,
            executable.as_os_str(),
            &["mcp".into()],
            DatabaseForwarding::Resolved,
        )
    }

    fn spawn_launcher(
        config: &Config,
        command: &str,
        args: &[String],
        database_forwarding: DatabaseForwarding,
    ) -> Result<Self> {
        Self::spawn_command(config, OsStr::new(command), args, database_forwarding)
    }

    fn spawn_command(
        config: &Config,
        command: &OsStr,
        args: &[String],
        database_forwarding: DatabaseForwarding,
    ) -> Result<Self> {
        let launch_args = launcher_arguments(config, args, database_forwarding)?;
        let command = launcher_command_from_root(&config.root, command)?;
        let mut child = std::process::Command::new(command)
            .args(&launch_args)
            .current_dir(&config.root)
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
        let (sender, lines) = protocol_channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_bounded_protocol_line(&mut reader) {
                    Ok(Some(line)) => {
                        if sender.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
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
            let line = line.map_err(|error| {
                doctor_error(stage, format!("invalid MCP protocol output: {error}"))
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

fn launcher_command_from_root(root: &std::path::Path, command: &OsStr) -> Result<OsString> {
    let path = std::path::Path::new(command);
    if path.is_relative() && path.components().count() > 1 {
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(doctor_error(
                "launch",
                "relative launcher command must not contain parent-directory traversal",
            ));
        }
        let canonical_root = root.canonicalize().map_err(|error| {
            doctor_error(
                "launch",
                format!("could not resolve repository root: {error}"),
            )
        })?;
        let resolved = canonical_root.join(path).canonicalize().map_err(|error| {
            doctor_error(
                "launch",
                format!("could not resolve relative launcher command: {error}"),
            )
        })?;
        if !resolved.starts_with(&canonical_root) {
            return Err(doctor_error(
                "launch",
                "relative launcher command resolves outside the repository",
            ));
        }
        Ok(resolved.into_os_string())
    } else {
        Ok(command.to_os_string())
    }
}

fn read_bounded_protocol_line(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let (consumed, line_ended) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if line.is_empty() {
                    return Ok(None);
                }
                return String::from_utf8(line)
                    .map(Some)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let content_end = newline.unwrap_or(available.len());
            if line.len().saturating_add(content_end) > MAX_PROTOCOL_LINE_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("MCP response line exceeds the {MAX_PROTOCOL_LINE_BYTES}-byte limit"),
                ));
            }
            line.extend_from_slice(&available[..content_end]);
            (
                newline.map_or(available.len(), |index| index + 1),
                newline.is_some(),
            )
        };
        reader.consume(consumed);
        if line_ended {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return String::from_utf8(line)
                .map(Some)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        }
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

        let arguments = launcher_arguments(&config, &["mcp".into()], DatabaseForwarding::Resolved)
            .expect("launcher arguments");

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
    fn configured_launcher_omits_only_implicit_managed_database_paths() {
        let root = tempfile::tempdir().expect("repository");
        let managed = Config::discover(root.path(), None).expect("managed config");
        let managed_arguments =
            launcher_arguments(&managed, &["mcp".into()], DatabaseForwarding::ExplicitOnly)
                .expect("managed launcher arguments");
        assert!(
            !managed_arguments
                .iter()
                .any(|argument| argument == "--database")
        );

        let explicit_path = root.path().join("explicit.sqlite");
        let explicit = Config::discover(root.path(), Some(explicit_path)).expect("explicit config");
        let explicit_arguments =
            launcher_arguments(&explicit, &["mcp".into()], DatabaseForwarding::ExplicitOnly)
                .expect("explicit launcher arguments");
        let database_index = explicit_arguments
            .iter()
            .position(|argument| argument == "--database")
            .expect("explicit database flag");
        assert_eq!(
            explicit_arguments.get(database_index + 1),
            Some(&explicit.database_path.into_os_string())
        );
    }

    #[test]
    fn path_bearing_relative_launchers_resolve_from_the_repository_root() {
        let root = tempfile::tempdir().expect("repository");
        let executable = if cfg!(windows) {
            "leantoken.exe"
        } else {
            "leantoken"
        };
        let relative = std::path::Path::new(".").join("bin").join(executable);
        std::fs::create_dir(root.path().join("bin")).expect("launcher directory");
        std::fs::write(root.path().join(&relative), "launcher").expect("launcher");

        assert_eq!(
            launcher_command_from_root(root.path(), relative.as_os_str())
                .expect("relative launcher"),
            root.path()
                .join(&relative)
                .canonicalize()
                .expect("canonical launcher")
                .into_os_string()
        );
        assert_eq!(
            launcher_command_from_root(root.path(), OsStr::new(executable)).expect("PATH launcher"),
            OsString::from(executable)
        );
    }

    #[test]
    fn path_bearing_relative_launchers_cannot_escape_the_repository() {
        let repository = tempfile::tempdir().expect("repository");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("leantoken"), "launcher").expect("outside launcher");

        assert!(matches!(
            launcher_command_from_root(repository.path(), OsStr::new("../bin/leantoken")),
            Err(Error::DoctorFailure {
                stage: "launch",
                ..
            })
        ));

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), repository.path().join("linked"))
                .expect("outside symlink");
            assert!(matches!(
                launcher_command_from_root(repository.path(), OsStr::new("linked/leantoken")),
                Err(Error::DoctorFailure {
                    stage: "launch",
                    ..
                })
            ));
        }
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
            configured_version_pin: None,
            expected_version: env!("CARGO_PKG_VERSION").into(),
            matches_current,
            managed,
            enabled: true,
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

        let mut disabled = registration(SetupClient::OpenCode, true, true);
        disabled.enabled = false;
        assert_eq!(
            registration_health("registered", std::slice::from_ref(&disabled)),
            "disabled"
        );
        assert_eq!(
            registration_repair_command("registered", "disabled", &[disabled]),
            "leantoken setup --refresh --yes"
        );
    }

    #[test]
    fn runtime_version_requires_the_expected_semver_and_bounded_schema_fingerprint() {
        let current = concat!(
            env!("CARGO_PKG_VERSION"),
            "+contract.0123456789abcdef0123456789abcdef"
        );
        let pinned = "0.1.19+schema.0123456789abcdef0123456789abcdef";
        let rollback = "0.1.18+schema.0123456789abcdef0123456789abcdef";
        let first_schema = "0.1.17+schema.0123456789abcdef0123456789abcdef";
        assert!(server_version_matches_runtime(
            current,
            Some(env!("CARGO_PKG_VERSION"))
        ));
        assert!(server_version_matches_runtime(pinned, Some("0.1.19")));
        assert!(server_version_matches_runtime(rollback, Some("0.1.18")));
        assert!(server_version_matches_runtime(first_schema, Some("0.1.17")));
        assert!(server_version_matches_runtime(pinned, None));
        assert!(!server_version_matches_runtime(
            env!("CARGO_PKG_VERSION"),
            Some(env!("CARGO_PKG_VERSION"))
        ));
        assert!(!server_version_matches_runtime(
            concat!(env!("CARGO_PKG_VERSION"), "+contract.short"),
            None
        ));
        assert!(!server_version_matches_runtime(pinned, Some("0.1.18")));
        assert!(!server_version_matches_runtime(
            "0.1.18+contract.0123456789abcdef0123456789abcdef",
            Some("0.1.18")
        ));
    }

    #[test]
    fn configured_catalog_validation_uses_the_child_release_profile() {
        let rollback = V0_1_17_TO_V0_1_18_TOOLS.map(str::to_owned).to_vec();
        let current = EXPECTED_TOOLS.map(str::to_owned).to_vec();

        assert!(catalog_matches_release(&rollback, "0.1.17"));
        assert!(catalog_matches_release(&rollback, "0.1.18"));
        assert!(!catalog_matches_release(&current, "0.1.18"));
        assert!(catalog_matches_release(
            &V0_1_19_TOOLS.map(str::to_owned),
            "0.1.19"
        ));
        assert!(catalog_matches_release(&current, env!("CARGO_PKG_VERSION")));

        let mut future = rollback;
        future.push("future_tool".into());
        assert!(catalog_matches_release(&future, "0.2.0"));
        for required in COMPATIBLE_REQUIRED_TOOLS {
            let incomplete = future
                .iter()
                .filter(|tool| tool.as_str() != required)
                .cloned()
                .collect::<Vec<_>>();
            assert!(!catalog_matches_release(&incomplete, "0.2.0"));
        }
    }

    #[test]
    fn configured_guidance_validation_accepts_the_first_schema_release() {
        let legacy = "For LeanToken savings or token statistics, call leantoken.savings directly. DEFAULT: call leantoken.context first. For an uncertain broad task, first use context plan_only=true, then repeat the same request with plan_only=false. PREFER leantoken.search over grep or rg.";
        assert!(instructions_match_release(legacy, "0.1.17"));
        assert!(!instructions_match_release(legacy, "0.1.18"));
    }

    #[test]
    fn configured_guidance_validation_accepts_compact_current_guidance() {
        let current = "Use LeanToken for indexed repository discovery. For broad work, call leantoken.context once with plan_only=false. For a known scope, choose the matching tool. Use savings for token statistics.";
        assert!(instructions_match_release(
            current,
            env!("CARGO_PKG_VERSION")
        ));
    }

    #[test]
    fn configured_doctor_expects_the_stored_launcher_version() {
        let registration = setup::ConfiguredRegistration {
            client: SetupClient::Codex,
            path: "config.toml".into(),
            source_hash: [0; 32],
            command: "npx".into(),
            args: vec!["--package=leantoken@0.1.19".into()],
            startup_timeout_seconds: Some(setup::CODEX_STARTUP_TIMEOUT_SECONDS),
            version: Some("0.1.19".into()),
            expected_version: env!("CARGO_PKG_VERSION").into(),
            matches_current: false,
            managed: true,
            enabled: true,
        };

        assert_eq!(expected_server_version(Some(&registration)), Some("0.1.19"));
        let report = RegistrationReport::from(&registration);
        assert_eq!(report.configured_version.as_deref(), Some("0.1.19"));
        assert_eq!(report.configured_version_pin.as_deref(), Some("0.1.19"));
        assert_eq!(report.expected_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            configured_handshake_timeout(Some(&registration)),
            RESPONSE_TIMEOUT
        );
        assert_eq!(
            expected_server_version(None),
            Some(env!("CARGO_PKG_VERSION"))
        );

        let mut short_timeout = registration.clone();
        short_timeout.startup_timeout_seconds = Some(2);
        assert_eq!(
            configured_handshake_timeout(Some(&short_timeout)),
            Duration::from_secs(2)
        );

        let mut floating = registration;
        floating.version = Some("latest".into());
        assert_eq!(expected_server_version(Some(&floating)), None);
    }

    #[test]
    fn protocol_reader_rejects_oversized_records_before_json_parsing() {
        let mut allowed = vec![b'x'; MAX_PROTOCOL_LINE_BYTES];
        allowed.push(b'\n');
        assert_eq!(
            read_bounded_protocol_line(&mut Cursor::new(allowed))
                .expect("bounded protocol line")
                .map(|line| line.len()),
            Some(MAX_PROTOCOL_LINE_BYTES)
        );

        let oversized = vec![b'x'; MAX_PROTOCOL_LINE_BYTES + 1];
        let error = read_bounded_protocol_line(&mut Cursor::new(oversized))
            .expect_err("oversized protocol line");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("MCP response line exceeds"));
    }

    #[test]
    fn protocol_output_queue_has_an_exact_record_bound() {
        let (sender, receiver) = protocol_channel();
        for index in 0..MAX_PROTOCOL_QUEUED_RECORDS {
            sender
                .try_send(Ok(format!("record {index}")))
                .expect("record within queue bound");
        }
        assert!(matches!(
            sender.try_send(Ok("over bound".into())),
            Err(mpsc::TrySendError::Full(_))
        ));
        drop(receiver);
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
