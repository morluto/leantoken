use leantoken::repository::validate_relative;
use leantoken_test_support::FixtureCase;
use serde::{Deserialize, Serialize};
use std::fs;

use super::compare_or_bless;

#[derive(Debug, Deserialize)]
struct RepositoryPathRequest {
    path: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct RepositoryPathExpectation {
    valid: bool,
}

pub(crate) fn run(case: &FixtureCase, bless: bool) -> Result<(), String> {
    let request: RepositoryPathRequest = serde_json::from_slice(
        &fs::read(&case.request).map_err(|error| format!("read request: {error}"))?,
    )
    .map_err(|error| format!("decode request: {error}"))?;
    let expected: RepositoryPathExpectation = serde_json::from_slice(
        &fs::read(&case.expected).map_err(|error| format!("read expected: {error}"))?,
    )
    .map_err(|error| format!("decode expected: {error}"))?;
    let actual = RepositoryPathExpectation {
        valid: validate_relative(&request.path).is_ok(),
    };
    compare_or_bless(case, &expected, &actual, bless)
}
