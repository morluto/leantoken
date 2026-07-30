// Keep integration tests in one crate so libtest can run every module in parallel.
macro_rules! integration_modules {
    ($($module:ident),+ $(,)?) => {
        $(mod $module;)+

        #[test]
        fn every_ordinary_integration_test_file_is_registered() {
            use std::{collections::BTreeSet, ffi::OsStr, fs};

            let tests_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
            let actual = fs::read_dir(tests_dir)
                .expect("read integration test directory")
                .map(|entry| entry.expect("read integration test entry").path())
                .filter(|path| path.extension() == Some(OsStr::new("rs")))
                .filter_map(|path| {
                    let stem = path.file_stem()?.to_str()?;
                    (stem != "integration" && stem != "benchmark_contract")
                        .then(|| stem.to_owned())
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
    process,
    cli,
    graph_signal_ablation_report,
    model_ab_trajectory_report,
    resolved_reference_oracle_report,
    representation_comparison,
    services,
);
