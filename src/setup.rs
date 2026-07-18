//! Global MCP client registration and removal.

use std::{
    fmt, fs,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

use dialoguer::{Confirm, MultiSelect, theme::ColorfulTheme};
use directories::BaseDirs;
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
}

/// Parsed request for the setup or removal workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupRequest {
    /// Explicitly selected clients.
    pub clients: Vec<SetupClient>,
    /// Select every supported client.
    pub all: bool,
    /// Skip interactive prompts and use detected clients when none are explicit.
    pub yes: bool,
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
    /// Whether setup ran from a persistent CLI installation.
    pub persistent_cli: bool,
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

    fn confirm(&self, operation: SetupOperation, clients: &[SetupClient]) -> Result<bool>;
}

struct DialoguerPrompt;

impl SetupPrompt for DialoguerPrompt {
    fn select(
        &self,
        operation: SetupOperation,
        detected: &[SetupClient],
    ) -> Result<Option<Vec<SetupClient>>> {
        let labels = SetupClient::ALL
            .iter()
            .map(|client| client.display_name())
            .collect::<Vec<_>>();
        let defaults = SetupClient::ALL
            .iter()
            .map(|client| detected.contains(client))
            .collect::<Vec<_>>();
        let selection = MultiSelect::with_theme(&ColorfulTheme::default())
            .with_prompt(format!(
                "Which clients should LeanToken {}?",
                operation.action()
            ))
            .items(&labels)
            .defaults(&defaults)
            .interact_opt()
            .map_err(prompt_error)?;
        Ok(selection.map(|indices| {
            indices
                .into_iter()
                .map(|index| SetupClient::ALL[index])
                .collect()
        }))
    }

    fn confirm(&self, operation: SetupOperation, clients: &[SetupClient]) -> Result<bool> {
        let names = clients
            .iter()
            .map(|client| client.display_name())
            .collect::<Vec<_>>()
            .join(", ");
        Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(format!(
                "{} LeanToken for {names}?",
                operation.action_label()
            ))
            .default(true)
            .interact()
            .map_err(prompt_error)
    }
}

fn prompt_error(error: dialoguer::Error) -> Error {
    Error::InvalidRequest(format!("interactive setup failed: {error}"))
}

/// Run global MCP setup or removal using the current user environment.
pub fn run(operation: SetupOperation, request: SetupRequest) -> Result<SetupReport> {
    let home = home_directory()
        .ok_or_else(|| Error::InvalidRequest("could not determine the home directory".into()))?;
    let environment = SetupEnvironment {
        home,
        launcher: McpLauncher::current()?,
        interactive: std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        persistent_cli: std::env::var_os("npm_lifecycle_event").as_deref()
            != Some(std::ffi::OsStr::new("npx")),
    };
    run_with(operation, request, &environment, &DialoguerPrompt)
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
    let detected = SetupClient::ALL
        .into_iter()
        .filter(|client| client.is_detected(&environment.home))
        .collect::<Vec<_>>();

    let (clients, prompted) = if request.all {
        (SetupClient::ALL.to_vec(), false)
    } else if !request.clients.is_empty() {
        (deduplicate(request.clients), false)
    } else if request.yes {
        if detected.is_empty() {
            return Err(Error::InvalidRequest(
                "no supported clients detected; pass a client flag or --all".into(),
            ));
        }
        (detected.clone(), false)
    } else {
        if !environment.interactive {
            return Err(Error::InvalidRequest(
                "interactive setup requires a terminal; pass client flags or --all with --yes"
                    .into(),
            ));
        }
        let Some(selected) = prompt.select(operation, &detected)? else {
            return Ok(cancelled_report(operation, environment.persistent_cli));
        };
        if selected.is_empty() {
            return Ok(cancelled_report(operation, environment.persistent_cli));
        }
        (selected, true)
    };

    if prompted && !prompt.confirm(operation, &clients)? {
        return Ok(cancelled_report(operation, environment.persistent_cli));
    }

    let results = clients
        .into_iter()
        .map(|client| configure_client(operation, client, &environment.home, &environment.launcher))
        .collect();
    Ok(SetupReport {
        operation,
        cancelled: false,
        persistent_cli: environment.persistent_cli,
        results,
    })
}

fn cancelled_report(operation: SetupOperation, persistent_cli: bool) -> SetupReport {
    SetupReport {
        operation,
        cancelled: true,
        persistent_cli,
        results: Vec::new(),
    }
}

fn deduplicate(clients: Vec<SetupClient>) -> Vec<SetupClient> {
    SetupClient::ALL
        .into_iter()
        .filter(|client| clients.contains(client))
        .collect()
}

fn configure_client(
    operation: SetupOperation,
    client: SetupClient,
    home: &Path,
    launcher: &McpLauncher,
) -> ClientSetupResult {
    let definition = client.definition(home);
    let outcome = match definition.format {
        ConfigFormat::Json { section, shape } => {
            edit_json_config(operation, &definition.path, section, shape, launcher)
        }
        ConfigFormat::Toml => edit_toml_config(operation, &definition.path, launcher),
    };
    match outcome {
        Ok(status) => ClientSetupResult {
            client,
            path: definition.path,
            status: status.to_string(),
            error: None,
        },
        Err(error) => ClientSetupResult {
            client,
            path: definition.path,
            status: "failed".into(),
            error: Some(error.to_string()),
        },
    }
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

fn edit_json_config(
    operation: SetupOperation,
    path: &Path,
    section_name: &str,
    shape: JsonEntryShape,
    launcher: &McpLauncher,
) -> Result<EditStatus> {
    let original = read_optional(path)?.unwrap_or_else(|| "{}\n".into());
    let root = CstRootNode::parse(&original, &ParseOptions::default())
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
                        return Ok(EditStatus::AlreadyConfigured);
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
                return Ok(EditStatus::NotConfigured);
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

    write_if_changed(path, &original, &root.to_string())?;
    Ok(status)
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

fn edit_toml_config(
    operation: SetupOperation,
    path: &Path,
    launcher: &McpLauncher,
) -> Result<EditStatus> {
    let original = read_optional(path)?.unwrap_or_default();
    let mut document = if original.trim().is_empty() {
        DocumentMut::new()
    } else {
        original
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
                return Ok(EditStatus::AlreadyConfigured);
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
                return Ok(EditStatus::NotConfigured);
            };
            let servers = servers_item
                .as_table_mut()
                .ok_or_else(|| invalid_config(path, "mcp_servers must be a table"))?;
            if servers.remove(SERVER_NAME).is_none() {
                return Ok(EditStatus::NotConfigured);
            }
            if servers.is_empty() {
                document.remove("mcp_servers");
            }
            EditStatus::Removed
        }
    };

    write_if_changed(path, &original, &document.to_string())?;
    Ok(status)
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
        Error::InvalidRequest(format!("config path has no parent: {}", path.display()))
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
    Error::InvalidRequest(format!(
        "refusing to overwrite malformed config {}: {error}",
        path.display()
    ))
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
        writeln!(output, "LeanToken {} cancelled.", report.operation.action())?;
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
        writeln!(output)?;
        writeln!(
            output,
            "Configuration verified for {configured} client{}.",
            if configured == 1 { "" } else { "s" }
        )?;
        writeln!(
            output,
            "Restart or reload those clients to connect LeanToken."
        )?;
        writeln!(output)?;
        if report.persistent_cli {
            writeln!(output, "Verify from a repository: leantoken doctor")?;
            writeln!(output, "Update later with: leantoken upgrade")?;
        } else {
            writeln!(
                output,
                "This was a zero-install npx setup; no global `leantoken` command was installed."
            )?;
            writeln!(
                output,
                "Configured MCP clients follow current npm releases automatically."
            )?;
            writeln!(
                output,
                "Verify from a repository: npx leantoken@latest doctor"
            )?;
            writeln!(
                output,
                "Run one-off commands with: npx leantoken@latest <command>"
            )?;
            writeln!(
                output,
                "Install the shell command with: npm install --global leantoken@latest"
            )?;
        }
        writeln!(output)?;
        writeln!(output, "First prompt:")?;
        writeln!(
            output,
            "  \"Use LeanToken to map the relevant repository context before editing.\""
        )?;
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

        fn confirm(&self, _operation: SetupOperation, _clients: &[SetupClient]) -> Result<bool> {
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
                yes: false,
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
    fn yes_requires_detected_or_explicit_clients() {
        let temp = tempfile::tempdir().unwrap();
        let error = run_with(
            SetupOperation::Setup,
            SetupRequest {
                clients: Vec::new(),
                all: false,
                yes: true,
            },
            &environment(&temp),
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("no supported clients detected"));
    }

    #[test]
    fn all_clients_receive_global_entries_and_second_setup_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        let request = SetupRequest {
            clients: Vec::new(),
            all: true,
            yes: true,
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
    }

    #[test]
    fn one_malformed_client_does_not_block_other_clients() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        fs::create_dir_all(&environment.home).unwrap();
        fs::write(environment.home.join(".claude.json"), "{ broken").unwrap();
        let report = run_with(
            SetupOperation::Setup,
            SetupRequest {
                clients: vec![SetupClient::Claude, SetupClient::Cursor],
                all: false,
                yes: true,
            },
            &environment,
            &FixedPrompt {
                selected: None,
                confirmed: true,
            },
        )
        .unwrap();
        assert!(report.has_failures());
        assert!(report.results[0].error.is_some());
        assert_eq!(report.results[1].status, "configured");
        assert_eq!(
            fs::read_to_string(environment.home.join(".claude.json")).unwrap(),
            "{ broken"
        );
        assert!(environment.home.join(".cursor/mcp.json").exists());
    }
}
