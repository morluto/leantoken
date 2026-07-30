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

pub(crate) fn diagnostic_state() -> SetupDiagnostic {
    let Some(home) = home_directory() else {
        return SetupDiagnostic {
            registration_status: "unknown",
            configured_clients: Vec::new(),
            registrations: Vec::new(),
            discovery_status: "unknown",
            discovery_paths: Vec::new(),
        };
    };
    let configured =
        McpLauncher::current().and_then(|launcher| configured_registrations(&home, &launcher));
    let (registration_status, registrations) = match configured {
        Ok(registrations) if registrations.is_empty() => ("not_registered", registrations),
        Ok(registrations) => ("registered", registrations),
        Err(_) => ("unknown", Vec::new()),
    };
    let configured_clients = registrations
        .iter()
        .map(|registration| registration.client)
        .collect();
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
        registrations,
        discovery_status: match discovery_paths.len() {
            0 => "missing",
            2 => "installed",
            _ => "partial",
        },
        discovery_paths,
    }
}

pub(super) fn configured_registrations(
    home: &Path,
    launcher: &McpLauncher,
) -> Result<Vec<ConfiguredRegistration>> {
    SetupClient::ALL
        .into_iter()
        .filter_map(|client| read_configured_registration(client, home, launcher).transpose())
        .collect()
}

pub(super) fn read_configured_registration(
    client: SetupClient,
    home: &Path,
    launcher: &McpLauncher,
) -> Result<Option<ConfiguredRegistration>> {
    let definition = client.definition(home);
    let Some(source) = read_optional(&definition.path)? else {
        return Ok(None);
    };
    let (command, args) = match definition.format {
        ConfigFormat::Json { section, shape } => {
            let root: Value = jsonc_parser::parse_to_serde_value(&source, &ParseOptions::default())
                .map_err(|error| invalid_config(&definition.path, error))?;
            let Some(entry) = root
                .get(section)
                .and_then(Value::as_object)
                .and_then(|section| section.get(SERVER_NAME))
            else {
                return Ok(None);
            };
            json_registration_command(entry, shape, &definition.path)?
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
            toml_registration_command(entry, &definition.path)?
        }
    };
    let expected_command = launcher.command()?.to_owned();
    let matches_current = command == expected_command && args == launcher.args;
    Ok(Some(ConfiguredRegistration {
        client,
        path: definition.path,
        version: registered_version(&command, &args),
        command,
        args,
        expected_version: launcher.version().to_owned(),
        matches_current,
    }))
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
) -> Result<(String, Vec<String>)> {
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
    Ok((command.to_owned(), args))
}

pub(super) fn registered_version(command: &str, args: &[String]) -> Option<String> {
    args.iter()
        .find_map(|argument| argument.strip_prefix("--package=leantoken@"))
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

pub(super) fn configured_clients(home: &Path, launcher: &McpLauncher) -> Result<Vec<SetupClient>> {
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
