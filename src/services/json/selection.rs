//! JSON Pointer and JMESPath selection over loaded values.

use serde_json::Value;

use crate::model::JsonSelector;
use crate::services::validation::{MAX_PATTERN_BYTES, validate_input};
use crate::{Error, Result};

pub(super) enum ParsedJsonSelector {
    Pointer(String),
    Jmespath {
        expression: String,
        compiled: jmespath::Expression<'static>,
    },
}

impl ParsedJsonSelector {
    pub(super) fn parse(selector: JsonSelector) -> Result<Self> {
        match selector {
            JsonSelector::Pointer { pointer } => {
                validate_input(&pointer, "JSON Pointer", MAX_PATTERN_BYTES)?;
                if !pointer.is_empty() && !pointer.starts_with('/') {
                    return Err(Error::InvalidInput {
                        field: "JSON Pointer",
                        reason: "must be empty or start with a slash",
                    });
                }
                Ok(Self::Pointer(pointer))
            }
            JsonSelector::Jmespath { expression } => {
                validate_input(&expression, "JMESPath expression", MAX_PATTERN_BYTES)?;
                if expression.trim().is_empty() {
                    return Err(Error::InvalidInput {
                        field: "JMESPath expression",
                        reason: "must not be empty",
                    });
                }
                let compiled = jmespath::compile(&expression)
                    .map_err(|error| invalid_json_selector("compile", error))?;
                Ok(Self::Jmespath {
                    expression,
                    compiled,
                })
            }
        }
    }

    pub(super) fn into_wire(self) -> JsonSelector {
        match self {
            Self::Pointer(pointer) => JsonSelector::Pointer { pointer },
            Self::Jmespath { expression, .. } => JsonSelector::Jmespath { expression },
        }
    }
}

pub(super) struct SelectedJson {
    value: Option<Value>,
}

impl SelectedJson {
    pub(super) fn is_present(&self) -> bool {
        self.value.is_some()
    }

    pub(super) fn value(&self) -> Option<&Value> {
        self.value.as_ref()
    }

    pub(super) fn into_required_value(self) -> Result<Value> {
        self.value.ok_or(Error::InvalidInput {
            field: "selector",
            reason: "did not match a JSON value",
        })
    }
}

fn invalid_json_selector(stage: &'static str, error: jmespath::JmespathError) -> Error {
    Error::InvalidJsonSelector {
        stage,
        offset: error.offset,
        line: error.line.saturating_add(1),
        column: error.column.saturating_add(1),
        reason: error.reason.to_string(),
    }
}

pub(super) fn select_json(
    value: &Value,
    selector: Option<&ParsedJsonSelector>,
) -> Result<SelectedJson> {
    match selector {
        None => Ok(SelectedJson {
            value: Some(value.clone()),
        }),
        Some(ParsedJsonSelector::Pointer(pointer)) => {
            let selected = value.pointer(pointer).cloned();
            Ok(SelectedJson { value: selected })
        }
        Some(ParsedJsonSelector::Jmespath { compiled, .. }) => {
            let selected = compiled
                .search(value)
                .map_err(|error| invalid_json_selector("evaluate", error))?;
            let selected = serde_json::to_value(selected.as_ref())
                .map_err(|error| Error::SerializationFailure(error.to_string()))?;
            Ok(SelectedJson {
                value: Some(selected),
            })
        }
    }
}
