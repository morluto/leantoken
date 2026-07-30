fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(identity) = arguments.first() else {
        eprintln!("usage: fixture-runner <domain>/<case> [--bless]");
        std::process::exit(2);
    };
    let bless = match arguments.as_slice() {
        [_] => false,
        [_, flag] if flag == "--bless" => true,
        _ => {
            eprintln!("usage: fixture-runner <domain>/<case> [--bless]");
            std::process::exit(2);
        }
    };
    if let Err(error) = leantoken_test_suite::run_fixture(identity, bless) {
        eprintln!("fixture failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
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
            leantoken_test_suite::run_fixture(&case.identity, false)
                .unwrap_or_else(|error| panic!("fixture {} failed: {error}", case.identity));
        }
    }
}
