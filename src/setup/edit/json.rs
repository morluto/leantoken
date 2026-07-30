use super::*;

#[cfg(test)]
pub(super) fn edit_json_config(
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

pub(super) fn resolve_json_edit(
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

pub(super) fn json_entry(shape: JsonEntryShape, launcher: &McpLauncher) -> Result<Value> {
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

pub(super) fn to_cst_input(value: &Value) -> CstInputValue {
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
