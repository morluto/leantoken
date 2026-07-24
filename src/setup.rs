//! Global MCP client registration and removal.

use std::{
    fmt, fs,
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
};

use directories::{BaseDirs, ProjectDirs};
use inquire::{Confirm, InquireError, MultiSelect};
use jsonc_parser::{ParseOptions, cst::CstInputValue, cst::CstRootNode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::NamedTempFile;
use toml_edit::{Array, DocumentMut, Item, Table, value};

use crate::{Error, Result};

#[path = "setup/launcher.rs"]
mod launcher;

use launcher::McpLauncher;

const SERVER_NAME: &str = "leantoken";
const DISCOVERY_SKILL_MARKER: &str = "<!-- managed by leantoken setup -->";

#[derive(Debug)]
pub(crate) struct SetupDiagnostic {
    pub(crate) registration_status: &'static str,
    pub(crate) configured_clients: Vec<SetupClient>,
    pub(crate) discovery_status: &'static str,
    pub(crate) discovery_paths: Vec<PathBuf>,
}

/// Coding clients supported by the global setup wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupClient {
    /// Claude Code.
    Claude,
    /// Cursor.
    Cursor,
    /// OpenCode.
    OpenCode,
    /// Codex CLI, desktop, and IDE integrations.
    Codex,
    /// Gemini CLI.
    Gemini,
    /// Google Antigravity.
    Antigravity,
}

impl SetupClient {
    /// Every supported client in display order.
    pub const ALL: [Self; 6] = [
        Self::Claude,
        Self::Cursor,
        Self::OpenCode,
        Self::Codex,
        Self::Gemini,
        Self::Antigravity,
    ];

    fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::Antigravity => "Antigravity",
        }
    }

    fn definition(self, home: &Path) -> ClientDefinition {
        match self {
            Self::Claude => ClientDefinition::json(
                home.join(".claude.json"),
                "mcpServers",
                JsonEntryShape::CommandAndArgs,
            ),
            Self::Cursor => ClientDefinition::json(
                home.join(".cursor/mcp.json"),
                "mcpServers",
                JsonEntryShape::CommandAndArgs,
            ),
            Self::OpenCode => {
                let directory = home.join(".config/opencode");
                let candidates = [
                    directory.join("opencode.json"),
                    directory.join("opencode.jsonc"),
                    directory.join(".opencode.json"),
                    directory.join(".opencode.jsonc"),
                ];
                let path = candidates
                    .iter()
                    .find(|candidate| candidate.exists())
                    .cloned()
                    .unwrap_or_else(|| candidates[0].clone());
                ClientDefinition::json(path, "mcp", JsonEntryShape::OpenCode)
            }
            Self::Codex => ClientDefinition {
                path: home.join(".codex/config.toml"),
                format: ConfigFormat::Toml,
            },
            Self::Gemini => ClientDefinition::json(
                home.join(".gemini/settings.json"),
                "mcpServers",
                JsonEntryShape::CommandAndArgs,
            ),
            Self::Antigravity => ClientDefinition::json(
                home.join(".gemini/config/mcp_config.json"),
                "mcpServers",
                JsonEntryShape::CommandAndArgs,
            ),
        }
    }

    fn is_detected(self, home: &Path) -> bool {
        match self {
            Self::Claude => home.join(".claude").exists() || home.join(".claude.json").exists(),
            Self::Cursor => home.join(".cursor").exists(),
            Self::OpenCode => home.join(".config/opencode").exists(),
            Self::Codex => home.join(".codex").exists(),
            Self::Gemini => home.join(".gemini").exists(),
            Self::Antigravity => {
                home.join(".gemini/antigravity").exists() || home.join(".agent").exists()
            }
        }
    }
}

/// Setup or removal operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupOperation {
    /// Add or update global MCP entries.
    Setup,
    /// Remove global MCP entries.
    Remove,
}

impl SetupOperation {
    fn action(self) -> &'static str {
        match self {
            Self::Setup => "set up",
            Self::Remove => "remove",
        }
    }

    fn action_label(self) -> &'static str {
        match self {
            Self::Setup => "Set up",
            Self::Remove => "Remove",
        }
    }

    fn selection_prompt(self) -> &'static str {
        match self {
            Self::Setup => "Which coding agents should use LeanToken?",
            Self::Remove => "Remove LeanToken from which coding agents?",
        }
    }

    fn plan_label(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Remove => "removal",
        }
    }
}

/// Parsed request for the setup or removal workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupRequest {
    /// Explicitly selected clients.
    pub clients: Vec<SetupClient>,
    /// Select every supported client.
    pub all: bool,
    /// Refresh every existing LeanToken entry without selecting new clients.
    pub refresh: bool,
    /// Install and register a direct application-owned native runtime.
    pub private_runtime: bool,
    /// Apply an explicitly scoped plan without interactive confirmation.
    pub yes: bool,
    /// Resolve and print the setup plan without changing configuration.
    pub dry_run: bool,
}

/// Planned action for one client configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientPlanAction {
    /// Create a new configuration file.
    Create,
    /// Update an existing configuration file.
    Update,
    /// The requested setup is already current.
    AlreadyCurrent,
    /// Remove an existing LeanToken entry.
    Remove,
    /// No LeanToken entry exists to remove.
    NotConfigured,
}

impl fmt::Display for ClientPlanAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::AlreadyCurrent => "already current",
            Self::Remove => "remove",
            Self::NotConfigured => "not configured",
        };
        formatter.write_str(label)
    }
}

/// Public, secret-free description of one planned client effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSetupPlan {
    /// Client whose global configuration was inspected.
    pub client: SetupClient,
    /// Exact global configuration path.
    pub path: PathBuf,
    /// Resolved action for the current state.
    pub action: ClientPlanAction,
    /// Whether local client state was detected.
    pub detected: bool,
}

/// One agent-discovery artifact owned by LeanToken setup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoverySetupPlan {
    /// Host-native skill path.
    pub path: PathBuf,
    /// Resolved action for the current state.
    pub action: ClientPlanAction,
}

/// Exact MCP launcher that setup will register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LauncherPlan {
    /// Executable written to client configuration.
    pub command: String,
    /// Arguments written to client configuration.
    pub args: Vec<String>,
    /// Exact LeanToken version represented by this launcher.
    pub version: String,
    /// Exact npm package specifier, when the launcher uses npm.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Whether client startup may contact the package registry.
    pub may_contact_network: bool,
    /// Application-owned native runtime path, when private-runtime mode is selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<PathBuf>,
    /// BLAKE3 digest of the native executable installed at `runtime_path`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_digest: Option<String>,
}

/// Outcome for one client configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSetupResult {
    /// Client that was processed.
    pub client: SetupClient,
    /// Global configuration path.
    pub path: PathBuf,
    /// Human-readable result status.
    pub status: String,
    /// Failure detail when configuration was not changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Aggregate setup or removal report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupReport {
    /// Operation that produced the report.
    pub operation: SetupOperation,
    /// Whether an interactive user cancelled before mutation.
    pub cancelled: bool,
    /// Whether this report describes a dry-run without mutation.
    pub dry_run: bool,
    /// Whether setup ran from a persistent CLI installation.
    pub persistent_cli: bool,
    /// Exact launcher considered for setup, omitted for removal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launcher: Option<LauncherPlan>,
    /// Secret-free resolved plan used for confirmation and execution.
    pub plan: Vec<ClientSetupPlan>,
    /// Agent-visible discovery artifacts included in the same transaction.
    pub discovery_plan: Vec<DiscoverySetupPlan>,
    /// Exact cl100k token count of one managed discovery skill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery_skill_tokens: Option<usize>,
    /// Per-client outcomes.
    pub results: Vec<ClientSetupResult>,
}

impl SetupReport {
    /// Return true when at least one selected client failed.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.results.iter().any(|result| result.error.is_some())
    }
}

#[derive(Debug, Clone)]
struct ClientDefinition {
    path: PathBuf,
    format: ConfigFormat,
}

impl ClientDefinition {
    fn json(path: PathBuf, section: &'static str, shape: JsonEntryShape) -> Self {
        Self {
            path,
            format: ConfigFormat::Json { section, shape },
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ConfigFormat {
    Json {
        section: &'static str,
        shape: JsonEntryShape,
    },
    Toml,
}

#[derive(Debug, Clone, Copy)]
enum JsonEntryShape {
    CommandAndArgs,
    OpenCode,
}

#[derive(Debug)]
struct SetupEnvironment {
    home: PathBuf,
    runtime_root: PathBuf,
    native_executable: PathBuf,
    launcher: McpLauncher,
    interactive: bool,
    persistent_cli: bool,
}

trait SetupPrompt {
    fn select(
        &self,
        operation: SetupOperation,
        detected: &[SetupClient],
    ) -> Result<Option<Vec<SetupClient>>>;

    fn confirm(&self, operation: SetupOperation, plan: &ResolvedSetupPlan) -> Result<bool>;
}

struct InteractivePrompt;

#[derive(Clone)]
struct AgentOption {
    client: SetupClient,
    detected: bool,
}

impl fmt::Display for AgentOption {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.client.display_name())?;
        if self.detected {
            formatter.write_str(" — detected")?;
        }
        Ok(())
    }
}

impl SetupPrompt for InteractivePrompt {
    fn select(
        &self,
        operation: SetupOperation,
        detected: &[SetupClient],
    ) -> Result<Option<Vec<SetupClient>>> {
        let stderr = std::io::stderr();
        let mut output = stderr.lock();
        writeln!(output, "◆ LeanToken // Context Distillery")?;
        writeln!(
            output,
            "  Detected agents are labeled for context; none are selected automatically."
        )?;
        writeln!(output)?;
        drop(output);
        let options = SetupClient::ALL
            .iter()
            .copied()
            .map(|client| AgentOption {
                client,
                detected: detected.contains(&client),
            })
            .collect::<Vec<_>>();
        match MultiSelect::new(operation.selection_prompt(), options)
            .without_filtering()
            .with_help_message("↑/↓ move • Space select • Enter continue • Esc cancel")
            .prompt_skippable()
        {
            Ok(selection) => {
                Ok(selection
                    .map(|options| options.into_iter().map(|option| option.client).collect()))
            }
            Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => Ok(None),
            Err(error) => Err(prompt_error(error)),
        }
    }

    fn confirm(&self, operation: SetupOperation, plan: &ResolvedSetupPlan) -> Result<bool> {
        print_preflight(plan)?;
        match Confirm::new(&format!("{} these changes?", operation.action_label()))
            .with_default(false)
            .prompt()
        {
            Ok(answer) => Ok(answer),
            Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => Ok(false),
            Err(error) => Err(prompt_error(error)),
        }
    }
}

fn prompt_error(error: InquireError) -> Error {
    Error::InternalFailure(format!("interactive setup failed: {error}"))
}

/// Run global MCP setup or removal using the current user environment.
pub fn run(
    operation: SetupOperation,
    request: SetupRequest,
    json_output: bool,
) -> Result<SetupReport> {
    let home = home_directory()
        .ok_or_else(|| Error::InternalFailure("could not determine the home directory".into()))?;
    let launcher = McpLauncher::current()?;
    let runtime_root = setup_runtime_root(&home);
    let environment = SetupEnvironment {
        home,
        runtime_root,
        native_executable: std::env::current_exe()?.canonicalize()?,
        persistent_cli: !launcher.uses_npx(),
        launcher,
        interactive: !json_output
            && std::io::stdin().is_terminal()
            && std::io::stderr().is_terminal(),
    };
    run_with(operation, request, &environment, &InteractivePrompt)
}

fn setup_runtime_root(home: &Path) -> PathBuf {
    let data_local = ProjectDirs::from("dev", "LeanToken", "leantoken")
        .map(|directories| directories.data_local_dir().to_path_buf());
    setup_runtime_root_from(home, data_local.as_deref())
}

fn setup_runtime_root_from(home: &Path, data_local: Option<&Path>) -> PathBuf {
    data_local
        .map_or_else(
            || home.join(".local").join("share").join("leantoken"),
            Path::to_path_buf,
        )
        .join("runtimes")
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or({
            #[cfg(windows)]
            {
                std::env::var_os("USERPROFILE")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
            }
            #[cfg(not(windows))]
            {
                None
            }
        })
        .or_else(|| BaseDirs::new().map(|directories| directories.home_dir().to_path_buf()))
}

pub(crate) fn diagnostic_state() -> SetupDiagnostic {
    let Some(home) = home_directory() else {
        return SetupDiagnostic {
            registration_status: "unknown",
            configured_clients: Vec::new(),
            discovery_status: "unknown",
            discovery_paths: Vec::new(),
        };
    };
    let configured =
        McpLauncher::current().and_then(|launcher| configured_clients(&home, &launcher));
    let (registration_status, configured_clients) = match configured {
        Ok(clients) if clients.is_empty() => ("not_registered", clients),
        Ok(clients) => ("registered", clients),
        Err(_) => ("unknown", Vec::new()),
    };
    let discovery_paths = [
        home.join(".agents/skills/leantoken/SKILL.md"),
        home.join(".claude/skills/leantoken/SKILL.md"),
    ]
    .into_iter()
    .filter(|path| {
        read_optional(path)
            .ok()
            .flatten()
            .is_some_and(|content| content.contains(DISCOVERY_SKILL_MARKER))
    })
    .collect::<Vec<_>>();
    SetupDiagnostic {
        registration_status,
        configured_clients,
        discovery_status: match discovery_paths.len() {
            0 => "missing",
            2 => "installed",
            _ => "partial",
        },
        discovery_paths,
    }
}

fn runtime_install_plan(environment: &SetupEnvironment) -> Result<RuntimeInstallPlan> {
    let digest = file_digest(&environment.native_executable)?;
    let executable_name = runtime_executable_name(cfg!(windows));
    let destination = environment
        .runtime_root
        .join(environment.launcher.version())
        .join(executable_name);
    let install_required = if destination.exists() {
        let installed_digest = file_digest(&destination)?;
        if installed_digest != digest {
            return Err(Error::InternalFailure(format!(
                "private runtime identity mismatch at {}",
                destination.display()
            )));
        }
        false
    } else {
        true
    };
    Ok(RuntimeInstallPlan {
        source: environment.native_executable.clone(),
        destination,
        digest,
        install_required,
    })
}

fn runtime_executable_name(windows: bool) -> &'static str {
    if windows {
        "leantoken.exe"
    } else {
        "leantoken"
    }
}

fn file_digest(path: &Path) -> Result<String> {
    let mut input = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn install_runtime(plan: &RuntimeInstallPlan) -> Result<bool> {
    if !plan.install_required {
        return Ok(false);
    }
    let parent = plan.destination.parent().ok_or_else(|| {
        Error::InternalFailure("private runtime destination has no parent".into())
    })?;
    fs::create_dir_all(parent)?;
    let mut staged = NamedTempFile::new_in(parent)?;
    let mut source = fs::File::open(&plan.source)?;
    std::io::copy(&mut source, staged.as_file_mut())?;
    staged
        .as_file_mut()
        .set_permissions(source.metadata()?.permissions())?;
    staged.as_file_mut().sync_all()?;
    if file_digest(staged.path())? != plan.digest {
        return Err(Error::InternalFailure(
            "staged private runtime digest mismatch".into(),
        ));
    }
    match staged.persist_noclobber(&plan.destination) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if file_digest(&plan.destination)? == plan.digest {
                Ok(false)
            } else {
                Err(Error::InternalFailure(format!(
                    "private runtime identity mismatch at {}",
                    plan.destination.display()
                )))
            }
        }
        Err(error) => Err(Error::Io(error.error)),
    }
}

fn run_with(
    operation: SetupOperation,
    request: SetupRequest,
    environment: &SetupEnvironment,
    prompt: &dyn SetupPrompt,
) -> Result<SetupReport> {
    let recovery_path = transaction_path(&environment.runtime_root);
    if request.dry_run && recovery_path.exists() {
        return Err(Error::InternalFailure(format!(
            "interrupted setup requires recovery before dry-run: {}",
            recovery_path.display()
        )));
    }
    let _setup_lock = (!request.dry_run)
        .then(|| acquire_setup_lock(&environment.runtime_root))
        .transpose()?;
    if !request.dry_run {
        recover_interrupted_transaction(&environment.runtime_root)?;
    }
    if request.refresh && operation != SetupOperation::Setup {
        return Err(Error::InvalidRequest(
            "--refresh is only valid with the setup command".into(),
        ));
    }
    if request.refresh && (request.all || !request.clients.is_empty()) {
        return Err(Error::InvalidRequest(
            "--refresh cannot be combined with client flags or --all".into(),
        ));
    }
    if request.private_runtime && operation != SetupOperation::Setup {
        return Err(Error::InvalidRequest(
            "--private-runtime is only valid with the setup command".into(),
        ));
    }

    let runtime = request
        .private_runtime
        .then(|| runtime_install_plan(environment))
        .transpose()?;
    let private_launcher = runtime.as_ref().map(|runtime| {
        McpLauncher::from_executable_with_version(
            &runtime.destination,
            environment.launcher.version(),
        )
    });
    let launcher = private_launcher.as_ref().unwrap_or(&environment.launcher);

    let detected = SetupClient::ALL
        .into_iter()
        .filter(|client| client.is_detected(&environment.home))
        .collect::<Vec<_>>();

    let clients = if request.refresh {
        configured_clients(&environment.home, launcher)?
    } else if request.all {
        SetupClient::ALL.to_vec()
    } else if !request.clients.is_empty() {
        deduplicate(request.clients)
    } else if request.yes {
        return Err(Error::InvalidRequest(
            "--yes requires explicit client flags or --all; detection is not consent".into(),
        ));
    } else {
        if !environment.interactive {
            return Err(Error::InvalidRequest(
                "interactive setup requires a terminal; pass client flags or --all with --yes"
                    .into(),
            ));
        }
        let Some(selected) = prompt.select(operation, &detected)? else {
            return Ok(empty_report(operation, environment.persistent_cli));
        };
        if selected.is_empty() {
            return Ok(empty_report(operation, environment.persistent_cli));
        }
        selected
    };

    if !environment.interactive && !request.dry_run && !request.yes {
        return Err(Error::InvalidRequest(
            "non-interactive setup requires explicit client flags, --all, or --refresh with --yes"
                .into(),
        ));
    }
    let manage_discovery = if operation == SetupOperation::Setup {
        true
    } else {
        configured_clients(&environment.home, launcher)?
            .into_iter()
            .all(|configured| clients.contains(&configured))
    };

    let plan = resolve_plan(
        operation,
        &clients,
        PlanEnvironment {
            detected: &detected,
            home: &environment.home,
            launcher,
            persistent_cli: environment.persistent_cli,
            runtime,
            manage_discovery,
            transaction_root: &environment.runtime_root,
        },
    )?;

    if request.dry_run {
        return Ok(report_from_plan(&plan, false, true, Vec::new()));
    }

    if !request.yes && !prompt.confirm(operation, &plan)? {
        return Ok(report_from_plan(&plan, true, false, Vec::new()));
    }

    let results = apply_plan(&plan);
    Ok(report_from_plan(&plan, false, false, results))
}

fn report_from_plan(
    plan: &ResolvedSetupPlan,
    cancelled: bool,
    dry_run: bool,
    results: Vec<ClientSetupResult>,
) -> SetupReport {
    let discovery_skill_tokens = plan.discovery_edits.first().and_then(|edit| {
        edit.updated
            .as_ref()
            .or(edit.original.as_ref())
            .map(|content| crate::tokens::Tokenizer::Cl100kBase.count(content))
    });
    SetupReport {
        operation: plan.operation,
        cancelled,
        dry_run,
        persistent_cli: plan.persistent_cli,
        launcher: plan.launcher.clone(),
        plan: plan.edits.iter().map(|edit| edit.public.clone()).collect(),
        discovery_plan: plan
            .discovery_edits
            .iter()
            .map(|edit| edit.public.clone())
            .collect(),
        discovery_skill_tokens,
        results,
    }
}

fn empty_report(operation: SetupOperation, persistent_cli: bool) -> SetupReport {
    SetupReport {
        operation,
        cancelled: true,
        dry_run: false,
        persistent_cli,
        launcher: None,
        plan: Vec::new(),
        discovery_plan: Vec::new(),
        discovery_skill_tokens: None,
        results: Vec::new(),
    }
}

fn deduplicate(clients: Vec<SetupClient>) -> Vec<SetupClient> {
    SetupClient::ALL
        .into_iter()
        .filter(|client| clients.contains(client))
        .collect()
}

fn configured_clients(home: &Path, launcher: &McpLauncher) -> Result<Vec<SetupClient>> {
    SetupClient::ALL
        .into_iter()
        .filter_map(|client| {
            let resolved = resolve_client_edit(SetupOperation::Remove, client, &[], home, launcher);
            match resolved {
                Ok(edit) if matches!(edit.status, EditStatus::Removed) => Some(Ok(client)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}

#[derive(Debug)]
struct ResolvedSetupPlan {
    operation: SetupOperation,
    persistent_cli: bool,
    launcher: Option<LauncherPlan>,
    runtime: Option<RuntimeInstallPlan>,
    edits: Vec<PlannedClientEdit>,
    discovery_edits: Vec<PlannedDiscoveryEdit>,
    transaction_root: PathBuf,
}

#[derive(Debug)]
struct RuntimeInstallPlan {
    source: PathBuf,
    destination: PathBuf,
    digest: String,
    install_required: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct SetupTransactionJournal {
    schema_version: u32,
    entries: Vec<SetupTransactionEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SetupTransactionEntry {
    path: PathBuf,
    original: Option<String>,
    updated_hash: Option<String>,
    updated_exists: bool,
}

struct SetupTransaction {
    path: PathBuf,
}

struct SetupLock {
    _file: fs::File,
}

fn acquire_setup_lock(runtime_root: &Path) -> Result<SetupLock> {
    fs::create_dir_all(runtime_root)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(runtime_root.join("setup.lock"))?;
    file.lock()?;
    Ok(SetupLock { _file: file })
}

impl SetupTransaction {
    fn commit(self) -> Result<()> {
        fs::remove_file(self.path)?;
        Ok(())
    }
}

fn transaction_path(runtime_root: &Path) -> PathBuf {
    runtime_root.join("setup-transaction-v1.json")
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

fn recover_interrupted_transaction(runtime_root: &Path) -> Result<()> {
    let path = transaction_path(runtime_root);
    let Some(serialized) = read_optional(&path)? else {
        return Ok(());
    };
    let journal: SetupTransactionJournal = serde_json::from_str(&serialized).map_err(|error| {
        Error::InternalFailure(format!(
            "invalid setup recovery journal {}: {error}",
            path.display()
        ))
    })?;
    if journal.schema_version != 1 {
        return Err(Error::InternalFailure(format!(
            "unsupported setup recovery journal version at {}",
            path.display()
        )));
    }
    for entry in &journal.entries {
        let current = read_optional(&entry.path)?;
        let still_original = current == entry.original;
        let matches_applied = current.as_ref().is_some_and(|value| {
            entry.updated_exists
                && entry
                    .updated_hash
                    .as_deref()
                    .is_some_and(|hash| content_hash(value) == hash)
        }) || (!entry.updated_exists && current.is_none());
        if !still_original && !matches_applied {
            return Err(Error::InternalFailure(format!(
                "cannot recover interrupted setup because {} changed afterward",
                entry.path.display()
            )));
        }
        restore_path(&entry.path, entry.original.as_deref())?;
    }
    fs::remove_file(path)?;
    Ok(())
}

fn begin_setup_transaction(plan: &ResolvedSetupPlan) -> Result<Option<SetupTransaction>> {
    let mut entries = Vec::new();
    for edit in &plan.edits {
        if let Some(updated) = &edit.updated {
            entries.push(SetupTransactionEntry {
                path: edit.public.path.clone(),
                original: edit.original.clone(),
                updated_hash: Some(content_hash(updated)),
                updated_exists: true,
            });
        }
    }
    for edit in &plan.discovery_edits {
        let (updated_hash, updated_exists) = match edit.public.action {
            ClientPlanAction::Create | ClientPlanAction::Update => {
                (edit.updated.as_deref().map(content_hash), true)
            }
            ClientPlanAction::Remove => (None, false),
            ClientPlanAction::AlreadyCurrent | ClientPlanAction::NotConfigured => continue,
        };
        entries.push(SetupTransactionEntry {
            path: edit.public.path.clone(),
            original: edit.original.clone(),
            updated_hash,
            updated_exists,
        });
    }
    if entries.is_empty() {
        return Ok(None);
    }
    fs::create_dir_all(&plan.transaction_root)?;
    let path = transaction_path(&plan.transaction_root);
    if path.exists() {
        return Err(Error::InternalFailure(format!(
            "setup recovery journal already exists at {}",
            path.display()
        )));
    }
    let journal = SetupTransactionJournal {
        schema_version: 1,
        entries,
    };
    let serialized = serde_json::to_string(&journal)?;
    let mut temporary = NamedTempFile::new_in(&plan.transaction_root)?;
    temporary.write_all(serialized.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist_noclobber(&path).map_err(|error| {
        Error::InternalFailure(format!(
            "another setup transaction became active at {}: {}",
            path.display(),
            error.error
        ))
    })?;
    Ok(Some(SetupTransaction { path }))
}

fn restore_path(path: &Path, original: Option<&str>) -> Result<()> {
    match original {
        Some(original) => {
            let current = read_optional(path)?.unwrap_or_default();
            write_if_changed(path, &current, original)
        }
        None => {
            if path.exists() {
                fs::remove_file(path)?;
            }
            Ok(())
        }
    }
}

#[derive(Debug)]
struct PlannedClientEdit {
    public: ClientSetupPlan,
    status: EditStatus,
    original: Option<String>,
    updated: Option<String>,
}

#[derive(Debug)]
struct PlannedDiscoveryEdit {
    public: DiscoverySetupPlan,
    original: Option<String>,
    updated: Option<String>,
}

struct PlanEnvironment<'a> {
    detected: &'a [SetupClient],
    home: &'a Path,
    launcher: &'a McpLauncher,
    persistent_cli: bool,
    runtime: Option<RuntimeInstallPlan>,
    manage_discovery: bool,
    transaction_root: &'a Path,
}

fn resolve_plan(
    operation: SetupOperation,
    clients: &[SetupClient],
    environment: PlanEnvironment<'_>,
) -> Result<ResolvedSetupPlan> {
    let edits = clients
        .iter()
        .copied()
        .map(|client| {
            resolve_client_edit(
                operation,
                client,
                environment.detected,
                environment.home,
                environment.launcher,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let discovery_edits = if environment.manage_discovery {
        resolve_discovery_edits(operation, environment.home, Some(environment.launcher))?
    } else {
        Vec::new()
    };
    let launcher = (operation == SetupOperation::Setup)
        .then(|| launcher_plan(environment.launcher, environment.runtime.as_ref()))
        .transpose()?;
    Ok(ResolvedSetupPlan {
        operation,
        persistent_cli: environment.persistent_cli,
        launcher,
        runtime: environment.runtime,
        edits,
        discovery_edits,
        transaction_root: environment.transaction_root.to_path_buf(),
    })
}

fn launcher_plan(
    launcher: &McpLauncher,
    runtime: Option<&RuntimeInstallPlan>,
) -> Result<LauncherPlan> {
    Ok(LauncherPlan {
        command: launcher.command()?.to_string(),
        args: launcher.args.clone(),
        version: launcher.version().into(),
        package: launcher.npm_package().map(str::to_owned),
        may_contact_network: launcher.uses_npx(),
        runtime_path: runtime.map(|runtime| runtime.destination.clone()),
        runtime_digest: runtime.map(|runtime| runtime.digest.clone()),
    })
}

fn resolve_discovery_edits(
    operation: SetupOperation,
    home: &Path,
    launcher: Option<&McpLauncher>,
) -> Result<Vec<PlannedDiscoveryEdit>> {
    let content = launcher.map(discovery_skill).transpose()?;
    [
        home.join(".agents/skills/leantoken/SKILL.md"),
        home.join(".claude/skills/leantoken/SKILL.md"),
    ]
    .into_iter()
    .map(|path| {
        let original = read_optional(&path)?;
        let owned = original
            .as_deref()
            .is_some_and(|value| value.contains(DISCOVERY_SKILL_MARKER));
        let (action, updated) = match operation {
            SetupOperation::Setup => {
                if original.as_deref() == content.as_deref() {
                    (ClientPlanAction::AlreadyCurrent, None)
                } else if original.is_none() || owned {
                    (
                        if original.is_none() {
                            ClientPlanAction::Create
                        } else {
                            ClientPlanAction::Update
                        },
                        content.clone(),
                    )
                } else {
                    return Err(Error::InternalFailure(format!(
                        "refusing to overwrite unowned discovery skill {}",
                        path.display()
                    )));
                }
            }
            SetupOperation::Remove if owned => (ClientPlanAction::Remove, Some(String::new())),
            SetupOperation::Remove => (ClientPlanAction::NotConfigured, None),
        };
        Ok(PlannedDiscoveryEdit {
            public: DiscoverySetupPlan { path, action },
            original,
            updated,
        })
    })
    .collect()
}

fn discovery_skill(launcher: &McpLauncher) -> Result<String> {
    let doctor = if launcher.uses_npx() {
        format!(
            "npx --yes {} doctor --json",
            launcher.npm_package().unwrap_or("leantoken")
        )
    } else {
        "leantoken doctor --json".into()
    };
    Ok(format!(
        "---\nname: leantoken\ndescription: Use LeanToken for token-bounded repository exploration, audits, codebase investigations, architecture reviews, source archaeology, code search, symbol outlines, and exact source reads.\n---\n\n{DISCOVERY_SKILL_MARKER}\n\nBefore retrieving repository source, including for audits and code archaeology, discover the deferred `leantoken` MCP server and route progressively:\n\n1. `leantoken_files` finds paths or returns a compact tree.\n2. `leantoken_outline` maps definitions and imports; `leantoken_search` locates symbols, references, identifiers, text, or regex matches.\n3. `leantoken_read` returns only the exact symbol or narrow line range needed.\n\nUse `leantoken_context` only while scope remains uncertain and `leantoken_savings` for repository-local savings. Use native workspace tools for edits, commands, tests, runtime probes, Git operations, and evidence that is not source retrieval. If the server or tools cannot be discovered, run `{doctor}` and report its structured registration, launch, handshake, and catalog status instead of silently claiming LeanToken was used.\n"
    ))
}

fn resolve_client_edit(
    operation: SetupOperation,
    client: SetupClient,
    detected: &[SetupClient],
    home: &Path,
    launcher: &McpLauncher,
) -> Result<PlannedClientEdit> {
    let definition = client.definition(home);
    let (status, original, updated) = match definition.format {
        ConfigFormat::Json { section, shape } => {
            resolve_json_edit(operation, &definition.path, section, shape, launcher)?
        }
        ConfigFormat::Toml => resolve_toml_edit(operation, &definition.path, launcher)?,
    };
    let action = match status {
        EditStatus::Configured if original.is_none() => ClientPlanAction::Create,
        EditStatus::Configured | EditStatus::Updated => ClientPlanAction::Update,
        EditStatus::AlreadyConfigured => ClientPlanAction::AlreadyCurrent,
        EditStatus::Removed => ClientPlanAction::Remove,
        EditStatus::NotConfigured => ClientPlanAction::NotConfigured,
    };
    Ok(PlannedClientEdit {
        public: ClientSetupPlan {
            client,
            path: definition.path,
            action,
            detected: detected.contains(&client),
        },
        status,
        original,
        updated,
    })
}

fn apply_plan(plan: &ResolvedSetupPlan) -> Vec<ClientSetupResult> {
    let runtime_installed = match plan.runtime.as_ref().map(install_runtime).transpose() {
        Ok(installed) => installed.unwrap_or(false),
        Err(error) => return failed_results(&plan.edits, error.to_string()),
    };
    if let Err(error) =
        preflight_edits(&plan.edits).and_then(|()| preflight_discovery(&plan.discovery_edits))
    {
        if runtime_installed && let Some(runtime) = &plan.runtime {
            let _ = fs::remove_file(&runtime.destination);
        }
        return failed_results(&plan.edits, error.to_string());
    }
    let transaction = match begin_setup_transaction(plan) {
        Ok(transaction) => transaction,
        Err(error) => {
            if runtime_installed && let Some(runtime) = &plan.runtime {
                let _ = fs::remove_file(&runtime.destination);
            }
            return failed_results(&plan.edits, error.to_string());
        }
    };

    let mut applied: Vec<&PlannedClientEdit> = Vec::new();
    let mut applied_discovery: Vec<&PlannedDiscoveryEdit> = Vec::new();
    for edit in &plan.edits {
        if let Err(error) = apply_edit(edit) {
            let rollback = rollback_setup(
                plan,
                runtime_installed,
                &applied,
                &applied_discovery,
                transaction,
            );
            return failed_results(&plan.edits, rollback_message(error, rollback));
        }
        applied.push(edit);
    }
    for edit in &plan.discovery_edits {
        if let Err(error) = apply_discovery_edit(edit) {
            let rollback = rollback_setup(
                plan,
                runtime_installed,
                &applied,
                &applied_discovery,
                transaction,
            );
            return failed_results(&plan.edits, rollback_message(error, rollback));
        }
        applied_discovery.push(edit);
    }
    if let Some(transaction) = transaction
        && let Err(error) = transaction.commit()
    {
        return failed_results(&plan.edits, error.to_string());
    }
    plan.edits
        .iter()
        .map(|edit| ClientSetupResult {
            client: edit.public.client,
            path: edit.public.path.clone(),
            status: edit.status.to_string(),
            error: None,
        })
        .collect()
}

fn rollback_setup(
    plan: &ResolvedSetupPlan,
    runtime_installed: bool,
    applied: &[&PlannedClientEdit],
    applied_discovery: &[&PlannedDiscoveryEdit],
    transaction: Option<SetupTransaction>,
) -> Result<()> {
    for edit in applied_discovery.iter().rev() {
        restore_discovery_edit(edit)?;
    }
    for edit in applied.iter().rev() {
        restore_edit(edit)?;
    }
    if runtime_installed && let Some(runtime) = &plan.runtime {
        let _ = fs::remove_file(&runtime.destination);
    }
    if let Some(transaction) = transaction {
        transaction.commit()?;
    }
    Ok(())
}

fn rollback_message(error: Error, rollback: Result<()>) -> String {
    match rollback {
        Ok(()) => format!("setup transaction rolled back: {error}"),
        Err(rollback_error) => format!(
            "setup transaction failed: {error}; rollback requires recovery: {rollback_error}"
        ),
    }
}

fn preflight_edits(edits: &[PlannedClientEdit]) -> Result<()> {
    for edit in edits {
        if read_optional(&edit.public.path)? != edit.original {
            return Err(Error::InternalFailure(format!(
                "configuration changed after preflight: {}",
                edit.public.path.display()
            )));
        }
    }
    Ok(())
}

fn preflight_discovery(edits: &[PlannedDiscoveryEdit]) -> Result<()> {
    for edit in edits {
        if read_optional(&edit.public.path)? != edit.original {
            return Err(Error::InternalFailure(format!(
                "discovery skill changed after preflight: {}",
                edit.public.path.display()
            )));
        }
    }
    Ok(())
}

fn failed_results(edits: &[PlannedClientEdit], error: String) -> Vec<ClientSetupResult> {
    edits
        .iter()
        .map(|edit| ClientSetupResult {
            client: edit.public.client,
            path: edit.public.path.clone(),
            status: "failed".into(),
            error: Some(error.clone()),
        })
        .collect()
}

fn restore_edit(edit: &PlannedClientEdit) -> Result<()> {
    restore_path(&edit.public.path, edit.original.as_deref())
}

fn apply_discovery_edit(edit: &PlannedDiscoveryEdit) -> Result<()> {
    match edit.public.action {
        ClientPlanAction::Create | ClientPlanAction::Update => write_if_changed(
            &edit.public.path,
            edit.original.as_deref().unwrap_or_default(),
            edit.updated.as_deref().unwrap_or_default(),
        ),
        ClientPlanAction::Remove => {
            if edit.public.path.exists() {
                fs::remove_file(&edit.public.path)?;
            }
            Ok(())
        }
        ClientPlanAction::AlreadyCurrent | ClientPlanAction::NotConfigured => Ok(()),
    }
}

fn restore_discovery_edit(edit: &PlannedDiscoveryEdit) -> Result<()> {
    restore_path(&edit.public.path, edit.original.as_deref())
}

fn apply_edit(edit: &PlannedClientEdit) -> Result<()> {
    let current = read_optional(&edit.public.path)?;
    if current != edit.original {
        return Err(Error::InternalFailure(format!(
            "configuration changed after preflight: {}",
            edit.public.path.display()
        )));
    }
    if let Some(updated) = &edit.updated {
        write_if_changed(
            &edit.public.path,
            edit.original.as_deref().unwrap_or_default(),
            updated,
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum EditStatus {
    Configured,
    Updated,
    AlreadyConfigured,
    Removed,
    NotConfigured,
}

impl fmt::Display for EditStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Configured => "configured",
            Self::Updated => "updated",
            Self::AlreadyConfigured => "already configured",
            Self::Removed => "removed",
            Self::NotConfigured => "not configured",
        };
        formatter.write_str(value)
    }
}

#[cfg(test)]
fn edit_json_config(
    operation: SetupOperation,
    path: &Path,
    section_name: &str,
    shape: JsonEntryShape,
    launcher: &McpLauncher,
) -> Result<EditStatus> {
    let (status, original, updated) =
        resolve_json_edit(operation, path, section_name, shape, launcher)?;
    let edit = PlannedClientEdit {
        public: ClientSetupPlan {
            client: SetupClient::Claude,
            path: path.to_path_buf(),
            action: ClientPlanAction::Update,
            detected: false,
        },
        status,
        original,
        updated,
    };
    apply_edit(&edit)?;
    Ok(status)
}

fn resolve_json_edit(
    operation: SetupOperation,
    path: &Path,
    section_name: &str,
    shape: JsonEntryShape,
    launcher: &McpLauncher,
) -> Result<(EditStatus, Option<String>, Option<String>)> {
    let original = read_optional(path)?;
    let source = original.clone().unwrap_or_else(|| "{}\n".into());
    let root = CstRootNode::parse(&source, &ParseOptions::default())
        .map_err(|error| invalid_config(path, error))?;
    let object = root
        .object_value_or_create()
        .ok_or_else(|| invalid_config(path, "top-level value must be an object"))?;
    let section = match object.object_value_or_create(section_name) {
        Some(section) => section,
        None => {
            return Err(invalid_config(
                path,
                format!("{section_name} must be an object"),
            ));
        }
    };

    let status = match operation {
        SetupOperation::Setup => {
            let expected = json_entry(shape, launcher)?;
            match section.get(SERVER_NAME) {
                Some(property) => {
                    let current = property
                        .value()
                        .ok_or_else(|| invalid_config(path, "LeanToken entry has no value"))?;
                    let current_value: Value = jsonc_parser::parse_to_serde_value(
                        &current.to_string(),
                        &ParseOptions::default(),
                    )
                    .map_err(|error| invalid_config(path, error))?;
                    if current_value == expected {
                        return Ok((EditStatus::AlreadyConfigured, original, None));
                    }
                    property.set_value(to_cst_input(&expected));
                    EditStatus::Updated
                }
                None => {
                    section.append(SERVER_NAME, to_cst_input(&expected));
                    EditStatus::Configured
                }
            }
        }
        SetupOperation::Remove => {
            let Some(property) = section.get(SERVER_NAME) else {
                return Ok((EditStatus::NotConfigured, original, None));
            };
            property.remove();
            if section.properties().is_empty() {
                object
                    .get(section_name)
                    .expect("section property exists")
                    .remove();
            }
            EditStatus::Removed
        }
    };

    let updated = root.to_string();
    Ok((status, original, Some(updated)))
}

fn json_entry(shape: JsonEntryShape, launcher: &McpLauncher) -> Result<Value> {
    let command = launcher.command()?;
    Ok(match shape {
        JsonEntryShape::CommandAndArgs => json!({
            "command": command,
            "args": launcher.args
        }),
        JsonEntryShape::OpenCode => json!({
            "type": "local",
            "command": std::iter::once(command).chain(launcher.args.iter().map(String::as_str)).collect::<Vec<_>>(),
            "cwd": ".",
            "enabled": true
        }),
    })
}

fn to_cst_input(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(value) => CstInputValue::Bool(*value),
        Value::Number(value) => CstInputValue::Number(value.to_string()),
        Value::String(value) => CstInputValue::String(value.clone()),
        Value::Array(values) => CstInputValue::Array(values.iter().map(to_cst_input).collect()),
        Value::Object(values) => CstInputValue::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), to_cst_input(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
fn edit_toml_config(
    operation: SetupOperation,
    path: &Path,
    launcher: &McpLauncher,
) -> Result<EditStatus> {
    let (status, original, updated) = resolve_toml_edit(operation, path, launcher)?;
    let edit = PlannedClientEdit {
        public: ClientSetupPlan {
            client: SetupClient::Codex,
            path: path.to_path_buf(),
            action: ClientPlanAction::Update,
            detected: false,
        },
        status,
        original,
        updated,
    };
    apply_edit(&edit)?;
    Ok(status)
}

fn resolve_toml_edit(
    operation: SetupOperation,
    path: &Path,
    launcher: &McpLauncher,
) -> Result<(EditStatus, Option<String>, Option<String>)> {
    let original = read_optional(path)?;
    let source = original.clone().unwrap_or_default();
    let mut document = if source.trim().is_empty() {
        DocumentMut::new()
    } else {
        source
            .parse::<DocumentMut>()
            .map_err(|error| invalid_config(path, error))?
    };

    let status = match operation {
        SetupOperation::Setup => {
            let command = launcher.command()?;
            let servers = ensure_toml_table(&mut document, "mcp_servers", path)?;
            if let Some(existing) = servers.get(SERVER_NAME)
                && toml_entry_matches(existing, command, &launcher.args)
            {
                return Ok((EditStatus::AlreadyConfigured, original, None));
            }
            let existed = servers.contains_key(SERVER_NAME);
            let mut server = Table::new();
            server["command"] = value(command);
            let mut args = Array::new();
            launcher
                .args
                .iter()
                .for_each(|argument| args.push(argument));
            server["args"] = value(args);
            server["startup_timeout_sec"] = value(30);
            servers.insert(SERVER_NAME, Item::Table(server));
            if existed {
                EditStatus::Updated
            } else {
                EditStatus::Configured
            }
        }
        SetupOperation::Remove => {
            let Some(servers_item) = document.get_mut("mcp_servers") else {
                return Ok((EditStatus::NotConfigured, original, None));
            };
            let servers = servers_item
                .as_table_mut()
                .ok_or_else(|| invalid_config(path, "mcp_servers must be a table"))?;
            if servers.remove(SERVER_NAME).is_none() {
                return Ok((EditStatus::NotConfigured, original, None));
            }
            if servers.is_empty() {
                document.remove("mcp_servers");
            }
            EditStatus::Removed
        }
    };

    Ok((status, original, Some(document.to_string())))
}

fn ensure_toml_table<'a>(
    document: &'a mut DocumentMut,
    name: &str,
    path: &Path,
) -> Result<&'a mut Table> {
    if !document.contains_key(name) {
        document.insert(name, Item::Table(Table::new()));
    }
    document
        .get_mut(name)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| invalid_config(path, format!("{name} must be a table")))
}

fn toml_entry_matches(item: &Item, command: &str, expected_args: &[String]) -> bool {
    let Some(table) = item.as_table() else {
        return false;
    };
    let command_matches = table
        .get("command")
        .and_then(Item::as_str)
        .is_some_and(|value| value == command);
    let args_match = table
        .get("args")
        .and_then(Item::as_array)
        .is_some_and(|args| {
            args.iter()
                .filter_map(|value| value.as_str())
                .eq(expected_args.iter().map(String::as_str))
                && args.len() == expected_args.len()
        });
    let timeout_matches = table.get("startup_timeout_sec").and_then(Item::as_integer) == Some(30);
    command_matches && args_match && timeout_matches
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_if_changed(path: &Path, original: &str, updated: &str) -> Result<()> {
    if original == updated {
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| {
        Error::InternalFailure(format!("config path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(updated.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    if let Ok(metadata) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary
        .persist(path)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
}

fn invalid_config(path: &Path, error: impl fmt::Display) -> Error {
    Error::InternalFailure(format!(
        "refusing to overwrite malformed config {}: {error}",
        path.display()
    ))
}

fn print_preflight(plan: &ResolvedSetupPlan) -> Result<()> {
    let stderr = std::io::stderr();
    let mut output = stderr.lock();
    writeln!(output)?;
    writeln!(output, "◆ LeanToken {} plan", plan.operation.plan_label())?;
    for edit in &plan.edits {
        writeln!(
            output,
            "  {} {}",
            plan_symbol(edit.public.action),
            edit.public.client.display_name()
        )?;
        writeln!(
            output,
            "    {} · {}",
            edit.public.action,
            edit.public.path.display()
        )?;
    }
    for edit in &plan.discovery_edits {
        writeln!(
            output,
            "  {} Agent discovery",
            plan_symbol(edit.public.action)
        )?;
        writeln!(
            output,
            "    {} · {}",
            edit.public.action,
            edit.public.path.display()
        )?;
    }
    if let Some(launcher) = &plan.launcher {
        writeln!(output)?;
        writeln!(output, "  MCP launcher")?;
        writeln!(output, "    command: {}", launcher.command)?;
        writeln!(
            output,
            "    args: {}",
            serde_json::to_string(&launcher.args)?
        )?;
        writeln!(output, "    version: {}", launcher.version)?;
        if let Some(package) = &launcher.package {
            writeln!(output, "    package: {package}")?;
        }
        if let (Some(path), Some(digest)) = (&launcher.runtime_path, &launcher.runtime_digest) {
            writeln!(output, "    private runtime: {}", path.display())?;
            writeln!(output, "    BLAKE3: {digest}")?;
        }
        if launcher.may_contact_network {
            writeln!(
                output,
                "    Client startup may contact npm, but it can resolve only this exact version."
            )?;
        } else {
            writeln!(output, "    Uses the current LeanToken executable.")?;
        }
    }
    writeln!(output)?;
    writeln!(
        output,
        "  Only the `leantoken` MCP entry will change; unrelated settings are preserved."
    )?;
    Ok(())
}

fn plan_symbol(action: ClientPlanAction) -> &'static str {
    match action {
        ClientPlanAction::Create | ClientPlanAction::Update | ClientPlanAction::Remove => "◇",
        ClientPlanAction::AlreadyCurrent | ClientPlanAction::NotConfigured => "─",
    }
}

fn print_report_plan(output: &mut impl Write, report: &SetupReport) -> Result<()> {
    writeln!(output, "◆ LeanToken dry-run")?;
    writeln!(output, "  No changes were made.")?;
    for effect in &report.plan {
        writeln!(
            output,
            "  {} {}: {} ({})",
            plan_symbol(effect.action),
            effect.client.display_name(),
            effect.path.display(),
            effect.action
        )?;
    }
    for effect in &report.discovery_plan {
        writeln!(
            output,
            "  {} Agent discovery: {} ({})",
            plan_symbol(effect.action),
            effect.path.display(),
            effect.action
        )?;
    }
    if let Some(launcher) = &report.launcher {
        writeln!(output)?;
        writeln!(output, "  Launcher: {}", launcher.command)?;
        writeln!(
            output,
            "  Arguments: {}",
            serde_json::to_string(&launcher.args)?
        )?;
        writeln!(output, "  Version: {}", launcher.version)?;
        if let Some(package) = &launcher.package {
            writeln!(output, "  Package: {package}")?;
        }
        if let (Some(path), Some(digest)) = (&launcher.runtime_path, &launcher.runtime_digest) {
            writeln!(output, "  Private runtime: {}", path.display())?;
            writeln!(output, "  BLAKE3: {digest}")?;
        }
        if launcher.may_contact_network {
            writeln!(
                output,
                "  Client startup may contact npm, but it can resolve only this exact version."
            )?;
        }
    }
    Ok(())
}

/// Print a setup report as JSON or concise human-readable output.
pub fn print_report(report: &SetupReport, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    if report.cancelled {
        writeln!(
            output,
            "LeanToken {} cancelled. No changes were made.",
            report.operation.action()
        )?;
        return Ok(());
    }
    if report.dry_run {
        print_report_plan(&mut output, report)?;
        return Ok(());
    }
    writeln!(output, "◆ LeanToken // Context Distillery")?;
    let operation_label = match report.operation {
        SetupOperation::Setup => "Global MCP setup",
        SetupOperation::Remove => "Global MCP removal",
    };
    writeln!(output, "  {operation_label}")?;
    for result in &report.results {
        if let Some(error) = &result.error {
            writeln!(
                output,
                "  ✗ {}: {} ({})",
                result.client.display_name(),
                result.path.display(),
                error
            )?;
        } else {
            writeln!(
                output,
                "  ✓ {}: {} ({})",
                result.client.display_name(),
                result.path.display(),
                result.status
            )?;
        }
    }
    if report.operation == SetupOperation::Setup {
        let configured = report
            .results
            .iter()
            .filter(|result| result.error.is_none())
            .count();
        let changed = report
            .results
            .iter()
            .filter(|result| matches!(result.status.as_str(), "configured" | "updated"))
            .count();
        writeln!(output)?;
        writeln!(
            output,
            "LeanToken is configured for {configured} client{}.",
            if configured == 1 { "" } else { "s" }
        )?;
        if report.has_failures() {
            writeln!(
                output,
                "Some selected clients failed; successful changes were not rolled back."
            )?;
        } else if changed > 0 {
            writeln!(
                output,
                "Restart or reload the configured clients to connect LeanToken."
            )?;
        } else {
            writeln!(output, "No configuration changes were needed.")?;
        }
        writeln!(output)?;
        if report
            .launcher
            .as_ref()
            .is_some_and(|launcher| launcher.runtime_path.is_some())
        {
            writeln!(
                output,
                "MCP clients now launch the pinned private native runtime directly."
            )?;
            writeln!(output, "Versioned runtimes are retained during removal.")?;
            writeln!(output, "Verify from a repository: leantoken doctor")?;
        } else if report.persistent_cli {
            writeln!(output, "Verify from a repository: leantoken doctor")?;
            writeln!(output, "Update later with: leantoken upgrade")?;
        } else {
            let version = report
                .launcher
                .as_ref()
                .map_or(env!("CARGO_PKG_VERSION"), |launcher| {
                    launcher.version.as_str()
                });
            writeln!(
                output,
                "This was a zero-install npx setup; no global `leantoken` command was installed."
            )?;
            writeln!(
                output,
                "Configured MCP clients are pinned to LeanToken v{version}."
            )?;
            writeln!(
                output,
                "Refresh existing MCP entries explicitly with: npx --yes leantoken@latest setup --refresh --yes"
            )?;
            writeln!(
                output,
                "Verify from a repository: npx leantoken@{version} doctor"
            )?;
            writeln!(
                output,
                "Install the shell command with: npm install --global leantoken@latest"
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_root_falls_back_below_the_resolved_home() {
        assert_eq!(
            setup_runtime_root_from(Path::new("/home/agent"), None),
            Path::new("/home/agent/.local/share/leantoken/runtimes")
        );
    }

    struct FixedPrompt {
        selected: Option<Vec<SetupClient>>,
        confirmed: bool,
    }

    impl SetupPrompt for FixedPrompt {
        fn select(
            &self,
            _operation: SetupOperation,
            _detected: &[SetupClient],
        ) -> Result<Option<Vec<SetupClient>>> {
            Ok(self.selected.clone())
        }

        fn confirm(&self, _operation: SetupOperation, _plan: &ResolvedSetupPlan) -> Result<bool> {
            Ok(self.confirmed)
        }
    }

    fn environment(temp: &tempfile::TempDir) -> SetupEnvironment {
        SetupEnvironment {
            home: temp.path().join("home"),
            runtime_root: temp.path().join("runtime"),
            native_executable: temp.path().join("bin/lean token"),
            launcher: McpLauncher::from_executable(&temp.path().join("bin/lean token")),
            interactive: true,
            persistent_cli: true,
        }
    }

    fn npx_environment(temp: &tempfile::TempDir, version: &str) -> SetupEnvironment {
        let runtime = temp.path().join("node runtime");
        SetupEnvironment {
            home: temp.path().join("home"),
            runtime_root: temp.path().join("runtime"),
            native_executable: temp.path().join("native/leantoken"),
            launcher: McpLauncher::from_npx_paths_with_version(
                &runtime.join(if cfg!(windows) { "node.exe" } else { "node" }),
                &runtime.join("npm cli.js"),
                version,
            )
            .unwrap(),
            interactive: false,
            persistent_cli: false,
        }
    }

    #[test]
    fn json_setup_preserves_comments_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mcp.json");
        fs::write(
            &path,
            "{\n  // keep me\n  \"theme\": \"dark\",\n  \"mcpServers\": {\n    \"other\": { \"command\": \"other\" },\n  },\n}\n",
        )
        .unwrap();
        let launcher = McpLauncher::from_executable(&temp.path().join("bin/léan token"));

        let first = edit_json_config(
            SetupOperation::Setup,
            &path,
            "mcpServers",
            JsonEntryShape::CommandAndArgs,
            &launcher,
        )
        .unwrap();
        assert!(matches!(first, EditStatus::Configured));
        let configured = fs::read_to_string(&path).unwrap();
        assert!(configured.contains("// keep me"));
        assert!(configured.contains("\"other\""));
        assert!(configured.contains("léan token"));

        let second = edit_json_config(
            SetupOperation::Setup,
            &path,
            "mcpServers",
            JsonEntryShape::CommandAndArgs,
            &launcher,
        )
        .unwrap();
        assert!(matches!(second, EditStatus::AlreadyConfigured));
        assert_eq!(fs::read_to_string(path).unwrap(), configured);
    }

    #[test]
    fn json_remove_preserves_sibling_server_and_prunes_empty_section() {
        let temp = tempfile::tempdir().unwrap();
        let launcher = McpLauncher::from_executable(&temp.path().join("leantoken"));
        let with_sibling = temp.path().join("with-sibling.json");
        fs::write(
            &with_sibling,
            "{\n  \"mcpServers\": {\n    \"leantoken\": {},\n    \"other\": {}\n  }\n}\n",
        )
        .unwrap();
        edit_json_config(
            SetupOperation::Remove,
            &with_sibling,
            "mcpServers",
            JsonEntryShape::CommandAndArgs,
            &launcher,
        )
        .unwrap();
        let contents = fs::read_to_string(with_sibling).unwrap();
        assert!(!contents.contains("leantoken"));
        assert!(contents.contains("other"));

        let only = temp.path().join("only.json");
        fs::write(
            &only,
            "{\n  \"mcpServers\": { \"leantoken\": {} },\n  \"x\": 1\n}\n",
        )
        .unwrap();
        edit_json_config(
            SetupOperation::Remove,
            &only,
            "mcpServers",
            JsonEntryShape::CommandAndArgs,
            &launcher,
        )
        .unwrap();
        let contents = fs::read_to_string(only).unwrap();
        assert!(!contents.contains("mcpServers"));
        assert!(contents.contains("\"x\": 1"));
    }

    #[cfg(unix)]
    #[test]
    fn json_remove_does_not_require_a_utf8_executable_path() {
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        fs::write(
            &path,
            "{\"mcpServers\":{\"leantoken\":{\"command\":\"old\",\"args\":[\"mcp\"]}}}\n",
        )
        .unwrap();
        let executable = PathBuf::from(std::ffi::OsString::from_vec(vec![b'l', 0x80]));
        let launcher = McpLauncher::from_executable(&executable);

        assert!(matches!(
            edit_json_config(
                SetupOperation::Remove,
                &path,
                "mcpServers",
                JsonEntryShape::CommandAndArgs,
                &launcher,
            )
            .unwrap(),
            EditStatus::Removed
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), "{}\n");
    }

    #[test]
    fn toml_setup_and_remove_preserve_unrelated_content() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        fs::write(
            &path,
            "# keep me\nmodel = \"test\"\n\n[mcp_servers.other]\ncommand = \"other\"\n",
        )
        .unwrap();
        let launcher = McpLauncher::from_executable(&temp.path().join("bin/leantoken"));
        edit_toml_config(SetupOperation::Setup, &path, &launcher).unwrap();
        let configured = fs::read_to_string(&path).unwrap();
        assert!(configured.contains("# keep me"));
        assert!(configured.contains("[mcp_servers.other]"));
        assert!(configured.contains("[mcp_servers.leantoken]"));
        assert!(matches!(
            edit_toml_config(SetupOperation::Setup, &path, &launcher).unwrap(),
            EditStatus::AlreadyConfigured
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), configured);

        edit_toml_config(SetupOperation::Remove, &path, &launcher).unwrap();
        let removed = fs::read_to_string(path).unwrap();
        assert!(removed.contains("# keep me"));
        assert!(removed.contains("[mcp_servers.other]"));
        assert!(!removed.contains("[mcp_servers.leantoken]"));
    }

    #[test]
    fn malformed_config_is_never_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("broken.json");
        let original = "{ nope";
        fs::write(&path, original).unwrap();
        assert!(
            edit_json_config(
                SetupOperation::Setup,
                &path,
                "mcpServers",
                JsonEntryShape::CommandAndArgs,
                &McpLauncher::from_executable(&temp.path().join("leantoken")),
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn interactive_selection_can_cancel_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let report = run_with(
            SetupOperation::Setup,
            SetupRequest {
                clients: Vec::new(),
                all: false,
                refresh: false,
                private_runtime: false,
                yes: false,
                dry_run: false,
            },
            &environment(&temp),
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap();
        assert!(report.cancelled);
        assert!(!temp.path().join("home").exists());
    }

    #[test]
    fn yes_requires_explicit_clients_even_when_a_client_is_detected() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        fs::create_dir_all(environment.home.join(".codex")).unwrap();
        let error = run_with(
            SetupOperation::Setup,
            SetupRequest {
                clients: Vec::new(),
                all: false,
                refresh: false,
                private_runtime: false,
                yes: true,
                dry_run: false,
            },
            &environment,
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("detection is not consent"));
        assert!(!environment.home.join(".codex/config.toml").exists());
    }

    #[test]
    fn all_clients_receive_global_entries_and_second_setup_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        let request = SetupRequest {
            clients: Vec::new(),
            all: true,
            refresh: false,
            private_runtime: false,
            yes: true,
            dry_run: false,
        };
        let first = run_with(
            SetupOperation::Setup,
            request.clone(),
            &environment,
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap();
        assert_eq!(first.results.len(), SetupClient::ALL.len());
        assert!(!first.has_failures());

        let home = &environment.home;
        for path in [
            home.join(".claude.json"),
            home.join(".cursor/mcp.json"),
            home.join(".config/opencode/opencode.json"),
            home.join(".codex/config.toml"),
            home.join(".gemini/settings.json"),
            home.join(".gemini/config/mcp_config.json"),
        ] {
            assert!(path.exists(), "missing {}", path.display());
        }
        let opencode = fs::read_to_string(home.join(".config/opencode/opencode.json")).unwrap();
        assert!(opencode.contains("\"type\": \"local\""));
        assert!(opencode.contains("\"cwd\": \".\""));
        assert!(opencode.contains("\"enabled\": true"));

        let before = first
            .results
            .iter()
            .map(|result| {
                (
                    result.path.clone(),
                    fs::read_to_string(&result.path).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let second = run_with(
            SetupOperation::Setup,
            request,
            &environment,
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap();
        assert!(
            second
                .results
                .iter()
                .all(|result| result.status == "already configured")
        );
        for (path, contents) in before {
            assert_eq!(fs::read_to_string(path).unwrap(), contents);
        }

        let refreshed = run_with(
            SetupOperation::Setup,
            SetupRequest {
                clients: Vec::new(),
                all: false,
                refresh: true,
                private_runtime: false,
                yes: true,
                dry_run: false,
            },
            &environment,
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap();
        assert_eq!(refreshed.results.len(), SetupClient::ALL.len());
        assert!(
            refreshed
                .results
                .iter()
                .all(|result| result.status == "already configured")
        );
    }

    #[test]
    fn malformed_client_blocks_the_entire_plan_before_writes() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        fs::create_dir_all(&environment.home).unwrap();
        fs::write(environment.home.join(".claude.json"), "{ broken").unwrap();
        let error = run_with(
            SetupOperation::Setup,
            SetupRequest {
                clients: vec![SetupClient::Claude, SetupClient::Cursor],
                all: false,
                refresh: false,
                private_runtime: false,
                yes: true,
                dry_run: false,
            },
            &environment,
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("refusing to overwrite malformed config")
        );
        assert_eq!(
            fs::read_to_string(environment.home.join(".claude.json")).unwrap(),
            "{ broken"
        );
        assert!(!environment.home.join(".cursor/mcp.json").exists());
    }

    #[test]
    fn non_interactive_explicit_selection_requires_yes() {
        let temp = tempfile::tempdir().unwrap();
        let mut environment = environment(&temp);
        environment.interactive = false;
        let error = run_with(
            SetupOperation::Setup,
            SetupRequest {
                clients: vec![SetupClient::Codex],
                all: false,
                refresh: false,
                private_runtime: false,
                yes: false,
                dry_run: false,
            },
            &environment,
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("non-interactive setup requires"));
        assert!(!environment.home.join(".codex/config.toml").exists());
    }

    #[test]
    fn dry_run_resolves_exact_plan_without_writes_or_yes() {
        let temp = tempfile::tempdir().unwrap();
        let mut environment = environment(&temp);
        environment.interactive = false;
        let report = run_with(
            SetupOperation::Setup,
            SetupRequest {
                clients: vec![SetupClient::Codex],
                all: false,
                refresh: false,
                private_runtime: false,
                yes: false,
                dry_run: true,
            },
            &environment,
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap();
        assert!(report.dry_run);
        assert_eq!(report.plan[0].action, ClientPlanAction::Create);
        assert!(report.results.is_empty());
        assert!(!environment.home.join(".codex/config.toml").exists());
    }

    #[test]
    fn explicit_interactive_selection_still_requires_confirmation() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        let report = run_with(
            SetupOperation::Setup,
            SetupRequest {
                clients: vec![SetupClient::Codex],
                all: false,
                refresh: false,
                private_runtime: false,
                yes: false,
                dry_run: false,
            },
            &environment,
            &FixedPrompt {
                selected: None,
                confirmed: false,
            },
        )
        .unwrap();
        assert!(report.cancelled);
        assert!(!environment.home.join(".codex/config.toml").exists());
    }

    #[test]
    fn refresh_updates_only_existing_entries_and_supports_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let original = npx_environment(&temp, "1.2.3");
        fs::create_dir_all(original.home.join(".cursor")).unwrap();
        fs::write(
            original.home.join(".cursor/mcp.json"),
            "{\"mcpServers\":{\"other\":{\"command\":\"other\"}}}\n",
        )
        .unwrap();
        run_with(
            SetupOperation::Setup,
            SetupRequest {
                clients: vec![SetupClient::Claude, SetupClient::Codex],
                all: false,
                refresh: false,
                private_runtime: false,
                yes: true,
                dry_run: false,
            },
            &original,
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap();

        let upgraded = npx_environment(&temp, "2.0.0");
        let refresh = SetupRequest {
            clients: Vec::new(),
            all: false,
            refresh: true,
            private_runtime: false,
            yes: true,
            dry_run: false,
        };
        let report = run_with(
            SetupOperation::Setup,
            refresh.clone(),
            &upgraded,
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap();
        assert_eq!(report.results.len(), 2);
        assert!(
            report
                .results
                .iter()
                .all(|result| result.status == "updated")
        );
        assert_eq!(report.launcher.unwrap().version, "2.0.0");
        assert!(
            fs::read_to_string(upgraded.home.join(".claude.json"))
                .unwrap()
                .contains("--package=leantoken@2.0.0")
        );
        assert!(
            fs::read_to_string(upgraded.home.join(".codex/config.toml"))
                .unwrap()
                .contains("--package=leantoken@2.0.0")
        );
        assert!(
            !fs::read_to_string(upgraded.home.join(".cursor/mcp.json"))
                .unwrap()
                .contains("leantoken@")
        );

        let rollback = run_with(
            SetupOperation::Setup,
            refresh,
            &original,
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap();
        assert_eq!(rollback.results.len(), 2);
        assert!(
            fs::read_to_string(original.home.join(".claude.json"))
                .unwrap()
                .contains("--package=leantoken@1.2.3")
        );
    }

    #[test]
    fn refresh_does_not_create_entries_or_fall_back_to_latest_without_an_npm_cache() {
        let temp = tempfile::tempdir().unwrap();
        let environment = npx_environment(&temp, "1.2.3");
        fs::create_dir_all(&environment.home).unwrap();

        let report = run_with(
            SetupOperation::Setup,
            SetupRequest {
                clients: Vec::new(),
                all: false,
                refresh: true,
                private_runtime: false,
                yes: true,
                dry_run: false,
            },
            &environment,
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap();

        assert!(report.results.is_empty());
        assert_eq!(
            report.launcher.unwrap().package.as_deref(),
            Some("leantoken@1.2.3")
        );
        assert!(!environment.home.join(".claude.json").exists());
        assert!(
            environment
                .launcher
                .args
                .iter()
                .all(|argument| !argument.contains("@latest"))
        );
    }

    #[test]
    fn refresh_rejects_ambiguous_selection_and_remove_usage() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        let prompt = FixedPrompt {
            selected: None,
            confirmed: true,
        };
        let ambiguous = SetupRequest {
            clients: vec![SetupClient::Codex],
            all: false,
            refresh: true,
            private_runtime: false,
            yes: true,
            dry_run: false,
        };
        assert!(
            run_with(SetupOperation::Setup, ambiguous, &environment, &prompt)
                .unwrap_err()
                .to_string()
                .contains("cannot be combined")
        );
        let remove = SetupRequest {
            clients: Vec::new(),
            all: false,
            refresh: true,
            private_runtime: false,
            yes: true,
            dry_run: false,
        };
        assert!(
            run_with(SetupOperation::Remove, remove, &environment, &prompt)
                .unwrap_err()
                .to_string()
                .contains("only valid with the setup command")
        );
    }

    #[test]
    fn private_runtime_dry_run_install_and_remove_are_pinned_and_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let environment = npx_environment(&temp, "1.2.3");
        fs::create_dir_all(environment.native_executable.parent().unwrap()).unwrap();
        fs::write(
            &environment.native_executable,
            b"verified native executable",
        )
        .unwrap();
        let request = SetupRequest {
            clients: vec![SetupClient::Codex],
            all: false,
            refresh: false,
            private_runtime: true,
            yes: true,
            dry_run: true,
        };
        let prompt = FixedPrompt {
            selected: None,
            confirmed: true,
        };

        let dry_run = run_with(
            SetupOperation::Setup,
            request.clone(),
            &environment,
            &prompt,
        )
        .unwrap();
        let launcher = dry_run.launcher.expect("launcher plan");
        let runtime_path = launcher.runtime_path.expect("private runtime path");
        assert_eq!(
            runtime_path,
            environment
                .runtime_root
                .join("1.2.3")
                .join(if cfg!(windows) {
                    "leantoken.exe"
                } else {
                    "leantoken"
                })
        );
        let expected_digest = file_digest(&environment.native_executable).unwrap();
        assert_eq!(
            launcher.runtime_digest.as_deref(),
            Some(expected_digest.as_str())
        );
        assert!(!runtime_path.exists(), "dry-run must not install");

        let mut apply = request;
        apply.dry_run = false;
        let first = run_with(SetupOperation::Setup, apply.clone(), &environment, &prompt).unwrap();
        assert!(!first.has_failures());
        assert_eq!(
            fs::read(&runtime_path).unwrap(),
            b"verified native executable"
        );
        let codex = fs::read_to_string(environment.home.join(".codex/config.toml")).unwrap();
        assert!(codex.contains(runtime_path.to_str().unwrap()));
        assert!(!codex.contains("npm"));

        let second = run_with(SetupOperation::Setup, apply, &environment, &prompt).unwrap();
        assert!(!second.has_failures());
        assert_eq!(second.plan[0].action, ClientPlanAction::AlreadyCurrent);

        let removal = run_with(
            SetupOperation::Remove,
            SetupRequest {
                clients: vec![SetupClient::Codex],
                all: false,
                refresh: false,
                private_runtime: false,
                yes: true,
                dry_run: false,
            },
            &environment,
            &prompt,
        )
        .unwrap();
        assert!(!removal.has_failures());
        assert!(runtime_path.exists(), "removal retains versioned runtimes");
    }

    #[test]
    fn private_runtime_uses_native_executable_names_for_supported_package_layouts() {
        for (platform, windows, expected) in [
            ("linux", false, "leantoken"),
            ("macos", false, "leantoken"),
            ("windows", true, "leantoken.exe"),
        ] {
            assert_eq!(runtime_executable_name(windows), expected, "{platform}");
        }
    }

    #[test]
    fn setup_transaction_rolls_back_earlier_client_edits() {
        let temp = tempfile::tempdir().unwrap();
        let first_path = temp.path().join("first/config.json");
        let blocked_parent = temp.path().join("blocked");
        fs::write(&blocked_parent, "not a directory").unwrap();
        let edits = vec![
            PlannedClientEdit {
                public: ClientSetupPlan {
                    client: SetupClient::Claude,
                    path: first_path.clone(),
                    action: ClientPlanAction::Create,
                    detected: true,
                },
                status: EditStatus::Configured,
                original: None,
                updated: Some("{\"mcpServers\":{}}".into()),
            },
            PlannedClientEdit {
                public: ClientSetupPlan {
                    client: SetupClient::Cursor,
                    path: blocked_parent.join("config.json"),
                    action: ClientPlanAction::Create,
                    detected: true,
                },
                status: EditStatus::Configured,
                original: None,
                updated: Some("{\"mcpServers\":{}}".into()),
            },
        ];
        let plan = ResolvedSetupPlan {
            operation: SetupOperation::Setup,
            persistent_cli: true,
            launcher: None,
            runtime: None,
            edits,
            discovery_edits: Vec::new(),
            transaction_root: temp.path().join("runtime"),
        };

        let results = apply_plan(&plan);

        assert!(results.iter().all(|result| result.error.is_some()));
        assert!(!first_path.exists(), "first edit must be rolled back");
        assert_eq!(
            fs::read_to_string(blocked_parent).unwrap(),
            "not a directory"
        );
    }

    #[test]
    fn failed_rollback_retains_recovery_journal() {
        let temp = tempfile::tempdir().unwrap();
        let runtime_root = temp.path().join("runtime");
        let parent = temp.path().join("config");
        let path = parent.join("client.json");
        fs::create_dir(&parent).unwrap();
        fs::write(&path, "old").unwrap();
        let edit = PlannedClientEdit {
            public: ClientSetupPlan {
                client: SetupClient::Codex,
                path: path.clone(),
                action: ClientPlanAction::Update,
                detected: true,
            },
            status: EditStatus::Updated,
            original: Some("old".into()),
            updated: Some("new".into()),
        };
        let plan = ResolvedSetupPlan {
            operation: SetupOperation::Setup,
            persistent_cli: true,
            launcher: None,
            runtime: None,
            edits: vec![edit],
            discovery_edits: Vec::new(),
            transaction_root: runtime_root.clone(),
        };
        let transaction = begin_setup_transaction(&plan)
            .unwrap()
            .expect("transaction");
        fs::write(&path, "new").unwrap();
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&parent).unwrap();
        fs::write(&parent, "blocks restoration").unwrap();

        let error = rollback_setup(&plan, false, &[&plan.edits[0]], &[], Some(transaction))
            .expect_err("rollback must fail");
        assert!(matches!(error, Error::Io(_)));
        assert!(transaction_path(&runtime_root).exists());
    }

    #[test]
    fn setup_manages_compact_discovery_skills_without_overwriting_unowned_content() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        let prompt = FixedPrompt {
            selected: None,
            confirmed: true,
        };
        let request = SetupRequest {
            clients: vec![SetupClient::Codex],
            all: false,
            refresh: false,
            private_runtime: false,
            yes: true,
            dry_run: false,
        };

        let report = run_with(
            SetupOperation::Setup,
            request.clone(),
            &environment,
            &prompt,
        )
        .unwrap();
        assert_eq!(report.discovery_plan.len(), 2);
        assert!(
            report
                .discovery_skill_tokens
                .is_some_and(|tokens| tokens > 0)
        );
        for effect in &report.discovery_plan {
            let skill = fs::read_to_string(&effect.path).unwrap();
            assert!(skill.contains(DISCOVERY_SKILL_MARKER));
            assert!(skill.contains("leantoken_context"));
            assert!(skill.contains("leantoken_savings"));
            assert!(skill.contains("audits and code archaeology"));
            assert!(skill.contains("runtime probes"));
            assert!(
                skill.find("leantoken_files").unwrap() < skill.find("leantoken_outline").unwrap()
            );
            assert!(
                skill.find("leantoken_outline").unwrap() < skill.find("leantoken_read").unwrap()
            );
            assert!(skill.contains("leantoken doctor --json"));
            assert!(!skill.contains("inputSchema"));
            assert_eq!(
                report.discovery_skill_tokens,
                Some(crate::tokens::Tokenizer::Cl100kBase.count(&skill))
            );
        }

        let shared_skill = environment.home.join(".agents/skills/leantoken/SKILL.md");
        fs::write(&shared_skill, "user-owned skill").unwrap();
        let error = run_with(SetupOperation::Setup, request, &environment, &prompt)
            .expect_err("unowned skill must block setup");
        assert!(error.to_string().contains("unowned discovery skill"));
        assert_eq!(
            fs::read_to_string(shared_skill).unwrap(),
            "user-owned skill"
        );
    }

    #[test]
    fn interrupted_setup_journal_restores_applied_and_unapplied_entries() {
        let temp = tempfile::tempdir().unwrap();
        let runtime_root = temp.path().join("runtime");
        fs::create_dir_all(&runtime_root).unwrap();
        let applied = temp.path().join("applied.json");
        let untouched = temp.path().join("untouched.json");
        fs::write(&applied, "new").unwrap();
        fs::write(&untouched, "old-two").unwrap();
        let journal = SetupTransactionJournal {
            schema_version: 1,
            entries: vec![
                SetupTransactionEntry {
                    path: applied.clone(),
                    original: Some("old-one".into()),
                    updated_hash: Some(content_hash("new")),
                    updated_exists: true,
                },
                SetupTransactionEntry {
                    path: untouched.clone(),
                    original: Some("old-two".into()),
                    updated_hash: Some(content_hash("new-two")),
                    updated_exists: true,
                },
            ],
        };
        fs::write(
            transaction_path(&runtime_root),
            serde_json::to_string(&journal).unwrap(),
        )
        .unwrap();

        recover_interrupted_transaction(&runtime_root).unwrap();

        assert_eq!(fs::read_to_string(applied).unwrap(), "old-one");
        assert_eq!(fs::read_to_string(untouched).unwrap(), "old-two");
        assert!(!transaction_path(&runtime_root).exists());
    }
}
