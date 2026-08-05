use super::*;

#[derive(Debug)]
pub(super) struct SetupEnvironment {
    pub(super) home: PathBuf,
    pub(super) runtime_root: PathBuf,
    pub(super) native_executable: PathBuf,
    pub(super) launcher: McpLauncher,
    pub(super) interactive: bool,
    pub(super) persistent_cli: bool,
}

pub(super) fn npx_resolved_from_local_project(executable: &Path, current_directory: &Path) -> bool {
    current_directory
        .ancestors()
        .any(|ancestor| executable.starts_with(ancestor.join("node_modules").join("leantoken")))
}

pub(super) fn require_current_npx_setup(current: &str, latest: Option<&str>) -> Result<()> {
    let Some(latest) = latest else {
        return Err(Error::InvalidRequest(
            "could not verify the locally resolved npx release; retry online or pass \
             --allow-outdated for an intentional pinned setup"
                .into(),
        ));
    };
    match crate::upgrade::version_update_available(current, latest) {
        Some(false) => Ok(()),
        Some(true) => Err(Error::InvalidRequest(format!(
            "npx resolved stale local LeanToken v{current}; latest is v{latest}. Run \
             `npx --yes leantoken@latest setup`, or pass --allow-outdated for an intentional \
             rollback"
        ))),
        None => Err(Error::InvalidRequest(format!(
            "could not compare locally resolved LeanToken v{current} with npm v{latest}; pass \
             --allow-outdated only for an intentional pinned setup"
        ))),
    }
}

pub(super) fn setup_runtime_root(home: &Path) -> PathBuf {
    let data_local = ProjectDirs::from("dev", "LeanToken", "leantoken")
        .map(|directories| directories.data_local_dir().to_path_buf());
    setup_runtime_root_from(home, data_local.as_deref())
}

pub(super) fn setup_runtime_root_from(home: &Path, data_local: Option<&Path>) -> PathBuf {
    data_local
        .map_or_else(
            || home.join(".local").join("share").join("leantoken"),
            Path::to_path_buf,
        )
        .join("runtimes")
}

pub(super) fn home_directory() -> Option<PathBuf> {
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

pub(crate) fn diagnostic_state(
    expected_launcher: Option<(&str, &[String], &str)>,
) -> SetupDiagnostic {
    let Some(home) = home_directory() else {
        return SetupDiagnostic {
            registration_status: "unknown",
            configured_clients: Vec::new(),
            registrations: Vec::new(),
            discovery_status: "unknown",
            discovery_paths: Vec::new(),
        };
    };
    diagnostic_state_at(&home, expected_launcher)
}

pub(super) fn diagnostic_state_at(
    home: &Path,
    expected_launcher: Option<(&str, &[String], &str)>,
) -> SetupDiagnostic {
    let configured = match expected_launcher {
        Some((command, args, version)) => {
            configured_registrations_against(home, command, args, version)
        }
        None => {
            McpLauncher::current().and_then(|launcher| configured_registrations(home, &launcher))
        }
    };
    let inspection_failed = configured.is_err();
    let (registration_status, registrations) = match configured {
        Ok(registrations) if registrations.is_empty() => ("not_registered", registrations),
        Ok(registrations) => ("registered", registrations),
        Err(_) => ("unknown", Vec::new()),
    };
    let configured_clients = registrations
        .iter()
        .map(|registration| registration.client)
        .collect();
    let expected_discovery_paths = registrations
        .iter()
        .filter(|registration| registration.managed)
        .map(|registration| registration.client.discovery_path(home))
        .collect::<std::collections::BTreeSet<_>>();
    let mut discovery_inspection_failed = false;
    let discovery_paths = [
        home.join(".agents/skills/leantoken/SKILL.md"),
        home.join(".claude/skills/leantoken/SKILL.md"),
    ]
    .into_iter()
    .filter_map(|path| match read_optional(&path) {
        Ok(Some(content)) if content.contains(DISCOVERY_SKILL_MARKER) => Some(path),
        Ok(_) => None,
        Err(_) => {
            discovery_inspection_failed = true;
            None
        }
    })
    .collect::<Vec<_>>();
    SetupDiagnostic {
        registration_status,
        configured_clients,
        registrations,
        discovery_status: if inspection_failed || discovery_inspection_failed {
            "unknown"
        } else if expected_discovery_paths.is_empty()
            || expected_discovery_paths
                .iter()
                .all(|path| !discovery_paths.contains(path))
        {
            "missing"
        } else if expected_discovery_paths
            .iter()
            .all(|path| discovery_paths.contains(path))
        {
            "installed"
        } else {
            "partial"
        },
        discovery_paths,
    }
}

pub(super) fn configured_registrations(
    home: &Path,
    launcher: &McpLauncher,
) -> Result<Vec<ConfiguredRegistration>> {
    configured_registrations_against(
        home,
        launcher.command()?,
        &launcher.args,
        launcher.version(),
    )
}

pub(super) fn configured_registrations_with_snapshots(
    home: &Path,
    launcher: &McpLauncher,
    clients: &[SetupClient],
) -> Result<(
    Vec<ConfiguredRegistration>,
    Vec<PlannedConfigurationSnapshot>,
)> {
    let mut snapshots = Vec::with_capacity(
        clients.len() + usize::from(clients.contains(&SetupClient::OpenCode)).saturating_mul(3),
    );
    for client in clients {
        for path in client.configuration_paths(home) {
            let original = read_optional(&path)?;
            snapshots.push(PlannedConfigurationSnapshot { path, original });
        }
    }
    let registrations =
        configured_registrations_from_snapshots(home, launcher, clients, &snapshots)?;
    Ok((registrations, snapshots))
}

pub(super) fn configured_registrations_from_snapshots(
    home: &Path,
    launcher: &McpLauncher,
    clients: &[SetupClient],
    snapshots: &[PlannedConfigurationSnapshot],
) -> Result<Vec<ConfiguredRegistration>> {
    clients
        .iter()
        .filter_map(|client| {
            let definition = client_definition_from_snapshots(*client, home, snapshots);
            let source = configuration_snapshot_source(&definition.path, snapshots);
            configured_registration_from_definition(*client, home, launcher, &definition, source)
                .transpose()
        })
        .collect()
}

pub(super) fn client_definition_from_snapshots(
    client: SetupClient,
    home: &Path,
    snapshots: &[PlannedConfigurationSnapshot],
) -> ClientDefinition {
    let candidates = client.configuration_paths(home);
    if !candidates
        .iter()
        .any(|candidate| snapshots.iter().any(|snapshot| snapshot.path == *candidate))
    {
        return client.definition(home);
    }
    let path = candidates
        .iter()
        .find(|candidate| configuration_snapshot_source(candidate, snapshots).is_some())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone());
    client.definition_at(path)
}

pub(super) fn configuration_snapshot_source<'a>(
    path: &Path,
    snapshots: &'a [PlannedConfigurationSnapshot],
) -> Option<&'a str> {
    snapshots
        .iter()
        .find(|snapshot| snapshot.path == path)
        .and_then(|snapshot| snapshot.original.as_deref())
}

fn configured_registrations_against(
    home: &Path,
    expected_command: &str,
    expected_args: &[String],
    expected_version: &str,
) -> Result<Vec<ConfiguredRegistration>> {
    SetupClient::ALL
        .into_iter()
        .filter_map(|client| {
            read_configured_registration_against(
                client,
                home,
                expected_command,
                expected_args,
                expected_version,
            )
            .transpose()
        })
        .collect()
}

pub(super) fn read_configured_registration(
    client: SetupClient,
    home: &Path,
    launcher: &McpLauncher,
) -> Result<Option<ConfiguredRegistration>> {
    let source = read_optional(&client.definition(home).path)?;
    configured_registration_from_source(client, home, launcher, source.as_deref())
}

pub(super) fn configured_registration_from_source(
    client: SetupClient,
    home: &Path,
    launcher: &McpLauncher,
    source: Option<&str>,
) -> Result<Option<ConfiguredRegistration>> {
    let definition = client.definition(home);
    configured_registration_from_definition(client, home, launcher, &definition, source)
}

pub(super) fn configured_registration_from_definition(
    client: SetupClient,
    home: &Path,
    launcher: &McpLauncher,
    definition: &ClientDefinition,
    source: Option<&str>,
) -> Result<Option<ConfiguredRegistration>> {
    configured_registration_from_definition_against(
        client,
        home,
        launcher.command()?,
        &launcher.args,
        launcher.version(),
        definition,
        source,
    )
}

fn read_configured_registration_against(
    client: SetupClient,
    home: &Path,
    expected_command: &str,
    expected_args: &[String],
    expected_version: &str,
) -> Result<Option<ConfiguredRegistration>> {
    let definition = client.definition(home);
    let source = read_optional(&definition.path)?;
    configured_registration_from_definition_against(
        client,
        home,
        expected_command,
        expected_args,
        expected_version,
        &definition,
        source.as_deref(),
    )
}

fn configured_registration_from_definition_against(
    client: SetupClient,
    home: &Path,
    expected_command: &str,
    expected_args: &[String],
    expected_version: &str,
    definition: &ClientDefinition,
    source: Option<&str>,
) -> Result<Option<ConfiguredRegistration>> {
    let Some(source) = source else {
        return Ok(None);
    };
    let (command, args, enabled, startup_timeout_seconds, launcher_settings_match) =
        match definition.format {
            ConfigFormat::Json { section, shape } => {
                let root: Value =
                    jsonc_parser::parse_to_serde_value(source, &ParseOptions::default())
                        .map_err(|error| invalid_config(&definition.path, error))?;
                let Some(entry) = root
                    .get(section)
                    .and_then(Value::as_object)
                    .and_then(|section| section.get(SERVER_NAME))
                else {
                    return Ok(None);
                };
                let enabled = match shape {
                    JsonEntryShape::CommandAndArgs => true,
                    JsonEntryShape::OpenCode => entry
                        .get("enabled")
                        .map(|enabled| {
                            enabled.as_bool().ok_or_else(|| {
                                invalid_config(
                                    &definition.path,
                                    "OpenCode MCP enabled flag must be a boolean",
                                )
                            })
                        })
                        .transpose()?
                        .unwrap_or(true),
                };
                let (command, args) = json_registration_command(entry, shape, &definition.path)?;
                (command, args, enabled, None, true)
            }
            ConfigFormat::Toml => {
                let document = source
                    .parse::<DocumentMut>()
                    .map_err(|error| invalid_config(&definition.path, error))?;
                let Some(entry) = document
                    .get("mcp_servers")
                    .and_then(Item::as_table)
                    .and_then(|servers| servers.get(SERVER_NAME))
                else {
                    return Ok(None);
                };
                let (command, args, startup_timeout_seconds) =
                    toml_registration_command(entry, &definition.path)?;
                let launcher_settings_match =
                    startup_timeout_seconds == Some(CODEX_STARTUP_TIMEOUT_SECONDS);
                (
                    command,
                    args,
                    true,
                    startup_timeout_seconds,
                    launcher_settings_match,
                )
            }
        };
    let matches_current =
        command == expected_command && args == expected_args && launcher_settings_match;
    let managed = is_managed_registration(&command, &args, &setup_runtime_root(home));
    Ok(Some(ConfiguredRegistration {
        client,
        path: definition.path.clone(),
        source_hash: *blake3::hash(source.as_bytes()).as_bytes(),
        version: registered_version(&command, &args),
        command,
        args,
        startup_timeout_seconds,
        expected_version: expected_version.to_owned(),
        matches_current,
        managed,
        enabled,
    }))
}

pub(crate) fn configured_registration(
    client: SetupClient,
) -> Result<Option<ConfiguredRegistration>> {
    let home = home_directory()
        .ok_or_else(|| Error::SetupFailure("could not determine the home directory".into()))?;
    let launcher = McpLauncher::current()?;
    read_configured_registration(client, &home, &launcher)
}

pub(super) fn is_managed_registration(command: &str, args: &[String], runtime_root: &Path) -> bool {
    if args.iter().any(|argument| argument == "--managed-by-setup") {
        return true;
    }
    is_legacy_package_manager_registration(command, args)
        || is_legacy_private_runtime_registration(command, args, runtime_root)
}

fn is_legacy_package_manager_registration(command: &str, args: &[String]) -> bool {
    is_legacy_node_npx_registration(command, args)
        || is_legacy_node_npm_registration(command, args)
        || is_legacy_direct_npx_registration(command, args)
        || is_legacy_direct_npm_registration(command, args)
        || is_legacy_dlx_registration(command, args)
}

fn command_has_stem(command: &str, expected: &str) -> bool {
    Path::new(command)
        .file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

fn argument_has_file_name(argument: &str, expected: &str) -> bool {
    Path::new(argument)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

fn is_exact_legacy_package(argument: &str, prefix: &str) -> bool {
    argument
        .strip_prefix(prefix)
        .is_some_and(|version| semver::Version::parse(version).is_ok())
}

fn is_legacy_node_npx_registration(command: &str, args: &[String]) -> bool {
    command_has_stem(command, "node")
        && args.len() == 7
        && Path::new(&args[0])
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("npx-cli.js"))
        && args[1] == "--yes"
        && args[2] == "--prefer-offline"
        && is_exact_legacy_package(&args[3], "--package=leantoken@")
        && args[4] == "--"
        && args[5] == "leantoken"
        && args[6] == "mcp"
}

fn is_legacy_node_npm_registration(command: &str, args: &[String]) -> bool {
    if !command_has_stem(command, "node")
        || !matches!(args.len(), 7 | 8)
        // Releases before the direct-npx launcher change could combine the
        // npx CLI path with npm's `exec` subcommand. Keep those exact shapes
        // refreshable so users can migrate to the current launcher.
        || !(argument_has_file_name(&args[0], "npm-cli.js")
            || argument_has_file_name(&args[0], "npx-cli.js"))
        || args[1] != "exec"
        || args[2] != "--yes"
    {
        return false;
    }
    let package_index = match args.len() {
        7 => 3,
        8 if args[3] == "--prefer-offline" => 4,
        _ => return false,
    };
    is_exact_legacy_package(&args[package_index], "--package=leantoken@")
        && args[package_index + 1] == "--"
        && args[package_index + 2] == "leantoken"
        && args[package_index + 3] == "mcp"
}

fn is_legacy_direct_npx_registration(command: &str, args: &[String]) -> bool {
    command_has_stem(command, "npx")
        && args.len() == 3
        && args[0] == "--yes"
        && is_exact_legacy_package(&args[1], "leantoken@")
        && args[2] == "mcp"
}

fn is_legacy_direct_npm_registration(command: &str, args: &[String]) -> bool {
    command_has_stem(command, "npm")
        && args.len() == 6
        && args[0] == "exec"
        && args[1] == "--yes"
        && is_exact_legacy_package(&args[2], "--package=leantoken@")
        && args[3] == "--"
        && args[4] == "leantoken"
        && args[5] == "mcp"
}

fn is_legacy_dlx_registration(command: &str, args: &[String]) -> bool {
    (command_has_stem(command, "pnpm") || command_has_stem(command, "yarn"))
        && args.len() == 3
        && args[0] == "dlx"
        && is_exact_legacy_package(&args[1], "leantoken@")
        && args[2] == "mcp"
}

fn is_legacy_private_runtime_registration(
    command: &str,
    args: &[String],
    runtime_root: &Path,
) -> bool {
    if args != ["mcp"] {
        return false;
    }
    let Ok(relative) = Path::new(command).strip_prefix(runtime_root) else {
        return false;
    };
    let components = relative.components().collect::<Vec<_>>();
    components.len() == 2
        && components[0]
            .as_os_str()
            .to_str()
            .is_some_and(|version| semver::Version::parse(version).is_ok())
        && components[1].as_os_str() == runtime_executable_name(cfg!(windows))
}

pub(super) fn json_registration_command(
    entry: &Value,
    shape: JsonEntryShape,
    path: &Path,
) -> Result<(String, Vec<String>)> {
    let object = entry
        .as_object()
        .ok_or_else(|| invalid_config(path, "LeanToken MCP entry must be an object"))?;
    match shape {
        JsonEntryShape::CommandAndArgs => {
            let command = object
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_config(path, "LeanToken MCP command must be a string"))?;
            let args = json_string_array(object.get("args"), path, "args")?;
            Ok((command.to_owned(), args))
        }
        JsonEntryShape::OpenCode => {
            let command = object
                .get("command")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_config(path, "OpenCode MCP command must be an array"))?;
            let mut values = command.iter();
            let executable = values
                .next()
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_config(path, "OpenCode MCP command is empty"))?;
            let args = values
                .map(|value| {
                    value.as_str().map(str::to_owned).ok_or_else(|| {
                        invalid_config(path, "OpenCode MCP command arguments must be strings")
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((executable.to_owned(), args))
        }
    }
}

pub(super) fn json_string_array(
    value: Option<&Value>,
    path: &Path,
    field: &str,
) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| invalid_config(path, format!("LeanToken MCP {field} must be an array")))?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                invalid_config(path, format!("LeanToken MCP {field} must contain strings"))
            })
        })
        .collect()
}

pub(super) fn toml_registration_command(
    entry: &Item,
    path: &Path,
) -> Result<(String, Vec<String>, Option<u64>)> {
    let table = entry
        .as_table()
        .ok_or_else(|| invalid_config(path, "LeanToken MCP entry must be a table"))?;
    let command = table
        .get("command")
        .and_then(Item::as_str)
        .ok_or_else(|| invalid_config(path, "LeanToken MCP command must be a string"))?;
    let args = table
        .get("args")
        .and_then(Item::as_array)
        .map(|args| {
            args.iter()
                .map(|value| {
                    value.as_str().map(str::to_owned).ok_or_else(|| {
                        invalid_config(path, "LeanToken MCP arguments must be strings")
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let startup_timeout_seconds = table
        .get("startup_timeout_sec")
        .map(|timeout| {
            let timeout = timeout.as_integer().ok_or_else(|| {
                invalid_config(path, "LeanToken MCP startup_timeout_sec must be an integer")
            })?;
            u64::try_from(timeout)
                .ok()
                .filter(|timeout| *timeout > 0)
                .ok_or_else(|| {
                    invalid_config(
                        path,
                        "LeanToken MCP startup_timeout_sec must be a positive integer",
                    )
                })
        })
        .transpose()?;
    Ok((command.to_owned(), args, startup_timeout_seconds))
}

pub(super) fn registered_version(command: &str, args: &[String]) -> Option<String> {
    args.iter()
        .find_map(|argument| {
            argument
                .strip_prefix("--package=leantoken@")
                .or_else(|| argument.strip_prefix("leantoken@"))
        })
        .map(str::to_owned)
        .or_else(|| {
            Path::new(command)
                .components()
                .collect::<Vec<_>>()
                .windows(2)
                .find(|components| components[0].as_os_str() == "runtimes")
                .and_then(|components| components[1].as_os_str().to_str())
                .map(str::to_owned)
        })
}

pub(super) fn managed_clients_from_registrations(
    registrations: &[ConfiguredRegistration],
) -> Vec<SetupClient> {
    registrations
        .iter()
        .filter(|registration| registration.managed)
        .map(|registration| registration.client)
        .collect()
}
