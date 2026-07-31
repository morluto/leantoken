//! JSON Pointer and JMESPath selection over loaded values.

use serde_json::Value;

use crate::model::JsonSelector;
use crate::{Error, Result};

pub(super) struct SelectedJson {
    present: bool,
    value: Option<Value>,
}

impl SelectedJson {
    pub(super) fn is_present(&self) -> bool {
        self.present
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

pub(super) fn select_json(value: &Value, selector: Option<&JsonSelector>) -> Result<SelectedJson> {
    match selector {
        None => Ok(SelectedJson {
            present: true,
            value: Some(value.clone()),
        }),
        Some(JsonSelector::Pointer { pointer }) => {
            let selected = value.pointer(pointer).cloned();
            Ok(SelectedJson {
                present: selected.is_some(),
                value: selected,
            })
        }
        Some(JsonSelector::Jmespath { expression }) => {
            let expression = jmespath::compile(expression)
                .map_err(|error| invalid_json_selector("compile", error))?;
            let selected = expression
                .search(value)
                .map_err(|error| invalid_json_selector("evaluate", error))?;
            let selected = serde_json::to_value(selected.as_ref())
                .map_err(|error| Error::SerializationFailure(error.to_string()))?;
            Ok(SelectedJson {
                present: true,
                value: Some(selected),
            })
        }
    }
}
