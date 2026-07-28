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
