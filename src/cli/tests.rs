use super::*;
use clap::CommandFactory;

#[test]
fn scoped_global_option_registry_matches_clap_arguments() {
    let command = Cli::command();
    for option in COMMAND_SCOPE_OPTIONS {
        assert!(
            command
                .get_arguments()
                .any(|argument| argument.get_id().as_str() == option.id),
            "scoped option {} is not defined by Clap",
            option.id
        );
    }
}

#[test]
fn repository_overrides_apply_to_secondary_context_configuration() {
    let primary = tempfile::tempdir().expect("primary repository");
    let context = tempfile::tempdir().expect("approved repository");
    let cli = Cli::try_parse_from([
        "leantoken",
        "--root",
        primary.path().to_str().expect("primary UTF-8"),
        "--include-generated",
        "--index-include",
        "src/**",
        "--max-walk-entries",
        "101",
        "--max-files",
        "102",
        "--max-total-source-bytes",
        "103",
        "--max-depth",
        "4",
        "--max-file-bytes",
        "5",
        "--max-prepare-batch-files",
        "6",
        "--max-prepare-batch-bytes",
        "7",
        "--max-index-workers",
        "2",
        "--tokenizer",
        "estimate",
        "mcp",
    ])
    .expect("MCP CLI");

    let config = cli
        .config_for_root(context.path(), None)
        .expect("secondary context config");
    assert!(config.include_generated);
    assert_eq!(config.index_scope().includes(), ["src/**"]);
    assert_eq!(config.max_walk_entries, 101);
    assert_eq!(config.max_files, 102);
    assert_eq!(config.max_total_source_bytes, 103);
    assert_eq!(config.max_depth, 4);
    assert_eq!(config.max_file_bytes, 5);
    assert_eq!(config.max_prepare_batch_files, 6);
    assert_eq!(config.max_prepare_batch_bytes, 7);
    assert_eq!(config.max_index_workers, 2);
    assert_eq!(config.tokenizer, crate::tokens::Tokenizer::Estimate);
}
