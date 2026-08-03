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
    let mut tool_names = Vec::with_capacity(tools.len());
    let all_tools_have_input_schema = tools.iter().all(|tool| {
        let Some(object) = tool.as_object() else {
            return false;
        };
        let Some(name) = object.get("name").and_then(serde_json::Value::as_str) else {
            return false;
        };
        tool_names.push(name.to_owned());
        object
            .get("inputSchema")
            .is_some_and(serde_json::Value::is_object)
    });
    tool_names.sort();
    let actual = ProtocolCatalogExpectation {
        tool_names,
        all_tools_have_input_schema,
    };
    compare_or_bless(case, &expected, &actual, bless)
}
