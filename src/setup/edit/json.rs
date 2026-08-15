use super::*;
use jsonc_parser::cst::CstObject;

pub(super) fn resolve_json_edit_from_source(
    operation: SetupOperation,
    path: &Path,
    section_name: &str,
    shape: JsonEntryShape,
    launcher: &McpLauncher,
    original: Option<String>,
) -> Result<ResolvedEdit> {
    let source = original.clone().unwrap_or_else(|| "{}\n".into());
    let root = CstRootNode::parse(&source, &ParseOptions::default())
        .map_err(|error| invalid_config(path, error))?;
    let object = root
        .object_value_or_create()
        .ok_or_else(|| invalid_config(path, "top-level value must be an object"))?;
    check_duplicate_keys(path, &object, "top-level")?;
    let section = match object.object_value_or_create(section_name) {
        Some(section) => section,
        None => {
            return Err(invalid_config(
                path,
                format!("{section_name} must be an object"),
            ));
        }
    };
    check_duplicate_keys(path, &section, section_name)?;

    match operation {
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
                        return Ok(ResolvedEdit::AlreadyConfigured { original });
                    }
                    property.set_value(to_cst_input(&expected));
                    Ok(ResolvedEdit::Updated {
                        original,
                        updated: root.to_string(),
                    })
                }
                None => {
                    section.append(SERVER_NAME, to_cst_input(&expected));
                    Ok(ResolvedEdit::Configured {
                        original,
                        updated: root.to_string(),
                    })
                }
            }
        }
        SetupOperation::Remove => {
            let Some(original) = original else {
                return Ok(ResolvedEdit::NotConfigured { original: None });
            };
            let Some(property) = section.get(SERVER_NAME) else {
                return Ok(ResolvedEdit::NotConfigured {
                    original: Some(original),
                });
            };
            property.remove();
            if section.properties().is_empty() {
                object
                    .get(section_name)
                    .expect("section property exists")
                    .remove();
            }
            Ok(ResolvedEdit::Removed {
                original,
                updated: root.to_string(),
            })
        }
    }
}

fn check_duplicate_keys(path: &Path, object: &CstObject, section_name: &str) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for prop in object.properties() {
        if let Some(name) = prop.name() {
            let decoded = match name {
                jsonc_parser::cst::ObjectPropName::String(s) => {
                    s.decoded_value().unwrap_or_default()
                }
                jsonc_parser::cst::ObjectPropName::Word(w) => {
                    format!("{}", w)
                }
            };
            if !seen.insert(decoded.clone()) {
                return Err(invalid_config(
                    path,
                    format!(
                        "duplicate key \"{}\" in {}; refusing to edit ambiguous configuration",
                        decoded, section_name
                    ),
                ));
            }
        }
    }
    Ok(())
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
