use leantoken::mcp::tool_catalog_json;
use leantoken_test_support::FixtureCase;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Deserialize)]
struct ProtocolCatalogRequest {}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ProtocolCatalogExpectation {
    is_json_array: bool,
    contains_context: bool,
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
    let catalog = tool_catalog_json();
    let actual = ProtocolCatalogExpectation {
        is_json_array: catalog.starts_with('[') && catalog.ends_with(']'),
        contains_context: catalog.contains("context"),
    };
    if bless {
        let rendered = serde_json::to_string_pretty(&actual).map_err(|error| error.to_string())?;
        fs::write(&case.expected, format!("{rendered}\n"))
            .map_err(|error| format!("write blessed expectation: {error}"))?;
        println!("blessed {}: {:?} -> {:?}", case.identity, expected, actual);
        return Ok(());
    }
    if expected != actual {
        return Err(format!(
            "semantic fixture mismatch for {}: expected {:?}, actual {:?}",
            case.identity, expected, actual
        ));
    }
    println!("fixture passed: {}", case.identity);
    Ok(())
}
