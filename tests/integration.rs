// Keep integration tests in one crate so libtest can run every module in parallel.
macro_rules! integration_modules {
    ($($module:ident),+ $(,)?) => {
        $(mod $module;)+

        #[test]
        fn every_integration_test_file_is_registered() {
            use std::{collections::BTreeSet, ffi::OsStr, fs};

            let tests_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
            let actual = fs::read_dir(tests_dir)
                .expect("read integration test directory")
                .map(|entry| entry.expect("read integration test entry").path())
                .filter(|path| path.extension() == Some(OsStr::new("rs")))
                .filter_map(|path| {
                    let stem = path.file_stem()?.to_str()?;
                    (stem != "integration").then(|| stem.to_owned())
                })
                .collect::<BTreeSet<_>>();
            let registered = [$(stringify!($module)),+]
                .into_iter()
                .map(str::to_owned)
                .collect::<BTreeSet<_>>();

            assert_eq!(actual, registered);
        }
    };
}

integration_modules!(
    benchmark_contract,
    binary,
    cli,
    config,
    graph_signal_ablation_report,
    indexer,
    mcp,
    mcp_token_costs,
    model_ab_trajectory_report,
    ranking,
    repository,
    representation_comparison,
    services,
    storage,
    tokens,
    watcher,
);
