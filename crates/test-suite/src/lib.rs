//! Private integration-suite ownership map.
//!
//! The existing root integration executable remains the product package's
//! process-test seam. New domain fixtures and cross-component tests belong in
//! this package as they move over; the package boundary prevents support code
//! from leaking into production.

use std::path::Path;

mod fixture_cases;

/// Run one exact, domain-owned fixture operation.
pub fn run_fixture(identity: &str, bless: bool) -> Result<(), String> {
    if identity.split('/').count() != 2
        || identity
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("fixture identity must be exactly <domain>/<case>".to_owned());
    }
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "test-suite manifest is not below a workspace root".to_owned())?;
    let case_root = workspace_root.join("fixtures").join(identity);
    let mut case =
        leantoken_test_support::FixtureCase::load(&case_root).map_err(|error| error.to_string())?;
    case.identity = identity.to_owned();
    match case.operation.as_str() {
        "repository_path_validation" => fixture_cases::indexing_repository::run(&case, bless),
        "protocol_catalog" => fixture_cases::protocol::run(&case, bless),
        operation => Err(format!("unknown fixture operation `{operation}`")),
    }
}

#[cfg(test)]
mod domains;
