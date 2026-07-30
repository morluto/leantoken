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
    if !valid_fixture_identity(identity) {
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

fn valid_fixture_identity(identity: &str) -> bool {
    let path = Path::new(identity);
    !identity.starts_with('/')
        && !identity.starts_with('\\')
        && path.is_relative()
        && identity.split('/').count() == 2
        && identity.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':'])
        })
}

#[cfg(test)]
mod domains;

#[cfg(test)]
mod tests {
    use super::{run_fixture, valid_fixture_identity};
    use leantoken_test_support::FixtureCase;
    use std::path::Path;

    const BENCHMARK_REPOSITORY_FIXTURE: &str = "sample_repo";

    #[test]
    fn checked_in_fixture_cases_match() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("test-suite manifest is below the workspace root");
        let cases = FixtureCase::list(workspace_root.join("fixtures"), None)
            .expect("checked-in fixture inventory is valid")
            .into_iter()
            .filter(|case| case.identity.split('/').next() != Some(BENCHMARK_REPOSITORY_FIXTURE))
            .collect::<Vec<_>>();
        assert!(!cases.is_empty(), "checked-in fixture inventory is empty");
        for case in cases {
            run_fixture(&case.identity, false)
                .unwrap_or_else(|error| panic!("fixture {} failed: {error}", case.identity));
        }
    }

    #[test]
    fn fixture_identity_rejects_absolute_and_drive_qualified_paths() {
        for identity in [
            "/tmp/case",
            r"\tmp\case",
            "C:/case",
            "C:case",
            "d:temp/case",
            "domain/../case",
        ] {
            assert!(!valid_fixture_identity(identity), "accepted {identity}");
        }
    }

    #[test]
    fn fixture_identity_accepts_two_relative_components() {
        assert!(valid_fixture_identity("protocol/catalog"));
    }
}
