use leantoken::mcp::tool_catalog_json;
use leantoken_test_support::FixtureCase;
use serde::{Deserialize, Serialize};
use std::fs;

use super::compare_or_bless;

#[derive(Debug, Deserialize)]
struct ProtocolCatalogRequest {}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ProtocolCatalogExpectation {
    tool_names: Vec<String>,
    all_tools_have_input_schema: bool,
}

fn catalog_expectation(tools: &[serde_json::Value]) -> ProtocolCatalogExpectation {
    // Keep extraction independent from validation. Iterator::all stops at the
    // first malformed entry, which could otherwise make a broken catalog look
    // like a shorter valid catalog when a fixture is blessed.
    let mut tool_names = tools
        .iter()
        .filter_map(|tool| {
            tool.as_object()
                .and_then(|object| object.get("name"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    tool_names.sort();
    let all_tools_have_input_schema = tools.iter().all(|tool| {
        let Some(object) = tool.as_object() else {
            return false;
        };
        object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some()
            && object
                .get("inputSchema")
                .is_some_and(serde_json::Value::is_object)
    });
    ProtocolCatalogExpectation {
        tool_names,
        all_tools_have_input_schema,
    }
}

pub(crate) fn run(case: &FixtureCase, bless: bool) -> Result<(), String> {
    let _: ProtocolCatalogRequest = serde_json::from_slice(
        &fs::read(&case.request).map_err(|error| format!("read request: {error}"))?,
    )
    .map_err(|error| format!("decode request: {error}"))?;
    let expected: ProtocolCatalogExpectation = serde_json::from_slice(
        &fs::read(&case.expected).map_err(|error| format!("read expected: {error}"))?,
    )
    .map_err(|error| format!("decode expected: {error}"))?;
    let catalog: serde_json::Value = serde_json::from_str(&tool_catalog_json())
        .map_err(|error| format!("decode catalog: {error}"))?;
    let tools = catalog
        .as_array()
        .ok_or_else(|| "tool catalog is not a JSON array".to_owned())?;
    let actual = catalog_expectation(tools);
    compare_or_bless(case, &expected, &actual, bless)
}

#[cfg(test)]
mod tests {
    use super::catalog_expectation;

    #[test]
    fn malformed_middle_entry_does_not_truncate_catalog_names() {
        let tools = vec![
            serde_json::json!({"name": "first", "inputSchema": {}}),
            serde_json::json!({"name": "malformed"}),
            serde_json::json!({"name": "last", "inputSchema": {}}),
        ];

        let actual = catalog_expectation(&tools);

        assert_eq!(actual.tool_names, ["first", "last", "malformed"]);
        assert!(!actual.all_tools_have_input_schema);
    }
}
