//! Global MCP client registration and removal.

use std::{
    fmt, fs,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

use directories::BaseDirs;
use inquire::{Confirm, InquireError, MultiSelect};
use jsonc_parser::{ParseOptions, cst::CstInputValue, cst::CstRootNode};
use serde::Serialize;
use serde_json::{Value, json};
use tempfile::NamedTempFile;
use toml_edit::{Array, DocumentMut, Item, Table, value};

use crate::{Error, Result};

#[path = "setup/launcher.rs"]
mod launcher;

use launcher::McpLauncher;

const SERVER_NAME: &str = "leantoken";

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
    let environment = SetupEnvironment {
        home,
        persistent_cli: !launcher.uses_npx(),
        launcher,
        interactive: !json_output
            && std::io::stdin().is_terminal()
            && std::io::stderr().is_terminal(),
    };
    run_with(operation, request, &environment, &InteractivePrompt)
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

fn run_with(
    operation: SetupOperation,
    request: SetupRequest,
    environment: &SetupEnvironment,
    prompt: &dyn SetupPrompt,
) -> Result<SetupReport> {
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

    let detected = SetupClient::ALL
        .into_iter()
        .filter(|client| client.is_detected(&environment.home))
        .collect::<Vec<_>>();

    let clients = if request.refresh {
        configured_clients(&environment.home, &environment.launcher)?
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

    let plan = resolve_plan(
        operation,
        &clients,
        &detected,
        &environment.home,
        &environment.launcher,
        environment.persistent_cli,
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
    SetupReport {
        operation: plan.operation,
        cancelled,
        dry_run,
        persistent_cli: plan.persistent_cli,
        launcher: plan.launcher.clone(),
        plan: plan.edits.iter().map(|edit| edit.public.clone()).collect(),
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
    edits: Vec<PlannedClientEdit>,
}

#[derive(Debug)]
struct PlannedClientEdit {
    public: ClientSetupPlan,
    status: EditStatus,
    original: Option<String>,
    updated: Option<String>,
}

fn resolve_plan(
    operation: SetupOperation,
    clients: &[SetupClient],
    detected: &[SetupClient],
    home: &Path,
    launcher: &McpLauncher,
    persistent_cli: bool,
) -> Result<ResolvedSetupPlan> {
    let edits = clients
        .iter()
        .copied()
        .map(|client| resolve_client_edit(operation, client, detected, home, launcher))
        .collect::<Result<Vec<_>>>()?;
    let launcher = (operation == SetupOperation::Setup)
        .then(|| launcher_plan(launcher))
        .transpose()?;
    Ok(ResolvedSetupPlan {
        operation,
        persistent_cli,
        launcher,
        edits,
    })
}

fn launcher_plan(launcher: &McpLauncher) -> Result<LauncherPlan> {
    Ok(LauncherPlan {
        command: launcher.command()?.to_string(),
        args: launcher.args.clone(),
        version: launcher.version().into(),
        package: launcher.npm_package().map(str::to_owned),
        may_contact_network: launcher.uses_npx(),
    })
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
    plan.edits
        .iter()
        .map(|edit| {
            let outcome = apply_edit(edit);
            match outcome {
                Ok(()) => ClientSetupResult {
                    client: edit.public.client,
                    path: edit.public.path.clone(),
                    status: edit.status.to_string(),
                    error: None,
                },
                Err(error) => ClientSetupResult {
                    client: edit.public.client,
                    path: edit.public.path.clone(),
                    status: "failed".into(),
                    error: Some(error.to_string()),
                },
            }
        })
        .collect()
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
        if report.persistent_cli {
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
            launcher: McpLauncher::from_executable(&temp.path().join("bin/lean token")),
            interactive: true,
            persistent_cli: true,
        }
    }

    fn npx_environment(temp: &tempfile::TempDir, version: &str) -> SetupEnvironment {
        let runtime = temp.path().join("node runtime");
        SetupEnvironment {
            home: temp.path().join("home"),
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
}
