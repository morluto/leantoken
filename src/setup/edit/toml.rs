use super::*;

pub(super) fn resolve_toml_edit_from_source(
    operation: SetupOperation,
    path: &Path,
    launcher: &McpLauncher,
    original: Option<String>,
) -> Result<ResolvedEdit> {
    let source = original.clone().unwrap_or_default();
    let mut document = if source.trim().is_empty() {
        DocumentMut::new()
    } else {
        source
            .parse::<DocumentMut>()
            .map_err(|error| invalid_config(path, error))?
    };

    match operation {
        SetupOperation::Setup => {
            let command = launcher.command()?;
            let expected_args = launcher.args_for(SetupClient::Codex);
            let servers = ensure_toml_table(&mut document, "mcp_servers", path)?;
            if let Some(existing) = servers.get(SERVER_NAME)
                && toml_entry_matches(existing, command, &expected_args)
            {
                return Ok(ResolvedEdit::AlreadyConfigured { original });
            }
            let existed = servers.contains_key(SERVER_NAME);
            let mut server = Table::new();
            server["command"] = value(command);
            let mut args = Array::new();
            expected_args
                .iter()
                .for_each(|argument| args.push(argument));
            server["args"] = value(args);
            server["startup_timeout_sec"] = value(CODEX_STARTUP_TIMEOUT_SECONDS as i64);
            servers.insert(SERVER_NAME, Item::Table(server));
            let updated = document.to_string();
            if existed {
                Ok(ResolvedEdit::Updated { original, updated })
            } else {
                Ok(ResolvedEdit::Configured { original, updated })
            }
        }
        SetupOperation::Remove => {
            let Some(original) = original else {
                return Ok(ResolvedEdit::NotConfigured { original: None });
            };
            let Some(servers_item) = document.get_mut("mcp_servers") else {
                return Ok(ResolvedEdit::NotConfigured {
                    original: Some(original),
                });
            };
            let servers = servers_item
                .as_table_mut()
                .ok_or_else(|| invalid_config(path, "mcp_servers must be a table"))?;
            if servers.remove(SERVER_NAME).is_none() {
                return Ok(ResolvedEdit::NotConfigured {
                    original: Some(original),
                });
            }
            if servers.is_empty() {
                document.remove("mcp_servers");
            }
            Ok(ResolvedEdit::Removed {
                original,
                updated: document.to_string(),
            })
        }
    }
}

pub(super) fn ensure_toml_table<'a>(
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

pub(super) fn toml_entry_matches(item: &Item, command: &str, expected_args: &[String]) -> bool {
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
    let timeout_matches = table
        .get("startup_timeout_sec")
        .and_then(toml_positive_integer)
        == Some(CODEX_STARTUP_TIMEOUT_SECONDS);
    command_matches && args_match && timeout_matches
}
