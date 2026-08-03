mod cli;
mod doctor;
mod mcp_lifecycle;
mod mcp_protocol;
mod repository_free;
mod runtime;
mod support;

#[test]
fn cli_indexes_statuses_and_searches_as_json() {
    cli::cli_indexes_statuses_and_searches_as_json();
}

#[test]
fn cli_scoped_index_omits_dependencies_and_discloses_the_boundary() {
    cli::cli_scoped_index_omits_dependencies_and_discloses_the_boundary();
}

#[test]
fn cli_retrieval_reconciles_live_changes_unless_snapshot_consistency_is_requested() {
    cli::cli_retrieval_reconciles_live_changes_unless_snapshot_consistency_is_requested();
}

#[test]
fn cli_savings_renders_a_color_aware_human_table() {
    cli::cli_savings_renders_a_color_aware_human_table();
}

#[test]
fn cli_index_explains_skipped_binary_files_without_returning_paths() {
    cli::cli_index_explains_skipped_binary_files_without_returning_paths();
}

#[test]
fn cli_files_tree_treats_dot_as_the_repository_root() {
    cli::cli_files_tree_treats_dot_as_the_repository_root();
}

#[test]
fn cold_cli_status_and_retrieval_explain_index_readiness() {
    cli::cold_cli_status_and_retrieval_explain_index_readiness();
}

#[test]
fn cli_json_errors_expose_stable_safe_metadata() {
    cli::cli_json_errors_expose_stable_safe_metadata();
}

#[test]
fn cli_json_parse_errors_are_structured_without_changing_clap_help() {
    cli::cli_json_parse_errors_are_structured_without_changing_clap_help();
}

#[test]
fn cli_index_limit_error_is_structured_and_does_not_publish_partial_files() {
    cli::cli_index_limit_error_is_structured_and_does_not_publish_partial_files();
}

#[test]
fn doctor_verifies_identity_catalog_and_first_retrieval() {
    doctor::doctor_verifies_identity_catalog_and_first_retrieval();
}

#[test]
fn doctor_can_exercise_the_exact_codex_registration() {
    doctor::doctor_can_exercise_the_exact_codex_registration();
}

#[test]
fn configured_doctor_launches_workspace_relative_commands_from_the_workspace() {
    doctor::configured_doctor_launches_workspace_relative_commands_from_the_workspace();
}

#[test]
fn configured_doctor_isolates_selected_client_from_unrelated_config_errors() {
    doctor::configured_doctor_isolates_selected_client_from_unrelated_config_errors();
}

#[test]
fn configured_doctor_maps_malformed_client_config_to_registration_stage() {
    doctor::configured_doctor_maps_malformed_client_config_to_registration_stage();
}

#[test]
fn configured_doctor_rejects_a_disabled_opencode_registration() {
    doctor::configured_doctor_rejects_a_disabled_opencode_registration();
}

#[cfg(unix)]
#[test]
fn configured_doctor_rejects_a_registration_changed_during_the_probe() {
    doctor::configured_doctor_rejects_a_registration_changed_during_the_probe();
}

#[test]
fn doctor_surfaces_bounded_redacted_child_diagnostics() {
    doctor::doctor_surfaces_bounded_redacted_child_diagnostics();
}

#[test]
fn doctor_human_output_uses_context_distillery_handoff() {
    doctor::doctor_human_output_uses_context_distillery_handoff();
}

#[test]
fn mcp_repeatedly_exits_cleanly_on_stdio_eof() {
    mcp_protocol::mcp_repeatedly_exits_cleanly_on_stdio_eof();
}

#[test]
fn mcp_survives_malformed_and_invalid_messages() {
    mcp_protocol::mcp_survives_malformed_and_invalid_messages();
}

#[test]
fn mcp_result_modes_project_exact_wire_shapes() {
    mcp_protocol::mcp_result_modes_project_exact_wire_shapes();
}

#[test]
fn mcp_receipt_created_by_one_process_is_reused_by_another() {
    mcp_protocol::mcp_receipt_created_by_one_process_is_reused_by_another();
}

#[test]
fn mcp_query_receipt_created_by_one_process_is_reused_by_another() {
    mcp_protocol::mcp_query_receipt_created_by_one_process_is_reused_by_another();
}

#[test]
fn mcp_receipt_rebase_is_cross_process_and_exact_only() {
    mcp_protocol::mcp_receipt_rebase_is_cross_process_and_exact_only();
}

#[test]
fn mcp_initialize_precedes_storage_open() {
    mcp_lifecycle::mcp_initialize_precedes_storage_open();
}

#[test]
fn mcp_cold_first_call_completes_the_public_acceptance_flow() {
    mcp_lifecycle::mcp_cold_first_call_completes_the_public_acceptance_flow();
}

#[test]
fn mcp_recovers_when_startup_database_contention_clears() {
    mcp_lifecycle::mcp_recovers_when_startup_database_contention_clears();
}

#[test]
fn mcp_eof_cancels_contended_startup_promptly() {
    mcp_lifecycle::mcp_eof_cancels_contended_startup_promptly();
}

#[test]
fn mcp_runtime_failure_transitions_tools_out_of_starting_state() {
    mcp_lifecycle::mcp_runtime_failure_transitions_tools_out_of_starting_state();
}

#[test]
fn cli_json_mcp_failure_is_one_document_after_a_logged_error() {
    mcp_lifecycle::cli_json_mcp_failure_is_one_document_after_a_logged_error();
}

#[test]
fn mcp_rejects_home_root_after_initialize_without_opening_storage() {
    mcp_lifecycle::mcp_rejects_home_root_after_initialize_without_opening_storage();
}

#[test]
fn mcp_index_limit_failure_is_terminal_and_does_not_retry() {
    mcp_lifecycle::mcp_index_limit_failure_is_terminal_and_does_not_retry();
}

#[test]
fn concurrent_mcp_startup_initializes_once_and_followers_read() {
    mcp_lifecycle::concurrent_mcp_startup_initializes_once_and_followers_read();
}

#[test]
fn mcp_follower_takes_over_after_leader_exit() {
    mcp_lifecycle::mcp_follower_takes_over_after_leader_exit();
}

#[test]
fn mcp_follower_does_not_hide_terminal_generation_zero_failover() {
    mcp_lifecycle::mcp_follower_does_not_hide_terminal_generation_zero_failover();
}

#[test]
fn mcp_follower_rebuilds_after_leader_is_killed_during_reconciliation() {
    mcp_lifecycle::mcp_follower_rebuilds_after_leader_is_killed_during_reconciliation();
}

#[test]
fn setup_and_remove_do_not_require_a_repository() {
    repository_free::setup_and_remove_do_not_require_a_repository();
}

#[test]
fn repository_options_are_rejected_by_repository_free_commands() {
    repository_free::repository_options_are_rejected_by_repository_free_commands();
}

#[test]
fn episode_audit_is_repo_free_deterministic_and_read_only() {
    repository_free::episode_audit_is_repo_free_deterministic_and_read_only();
}

#[test]
fn setup_requires_yes_before_non_interactive_mutation() {
    repository_free::setup_requires_yes_before_non_interactive_mutation();
}

#[test]
#[cfg(not(windows))]
// Windows ProjectDirs resolves the known cache folder without honoring the
// fixture's environment override; cache cleanup safety is covered by the
// platform-independent cache module tests instead of touching a user cache.
fn cache_list_and_prune_do_not_require_a_repository() {
    repository_free::cache_list_and_prune_do_not_require_a_repository();
}

#[test]
fn setup_dry_run_reports_exact_plan_without_mutation() {
    runtime::setup_dry_run_reports_exact_plan_without_mutation();
}

#[test]
fn malformed_selected_config_blocks_all_setup_writes() {
    runtime::malformed_selected_config_blocks_all_setup_writes();
}

#[test]
fn npx_setup_registers_exact_release_instead_of_its_cache_path() {
    runtime::npx_setup_registers_exact_release_instead_of_its_cache_path();
}

#[test]
fn setup_refresh_targets_only_existing_mcp_entries() {
    runtime::setup_refresh_targets_only_existing_mcp_entries();
}

#[test]
fn private_runtime_setup_installs_and_registers_the_verified_native_binary() {
    runtime::private_runtime_setup_installs_and_registers_the_verified_native_binary();
}

#[cfg(unix)]
#[test]
fn runtime_commands_refuse_a_symlinked_runtime_root_without_mutation() {
    runtime::runtime_commands_refuse_a_symlinked_runtime_root_without_mutation();
}

#[cfg(not(windows))]
#[test]
fn runtime_list_and_prune_are_bounded_reference_safe_and_dry_run_by_default() {
    runtime::runtime_list_and_prune_are_bounded_reference_safe_and_dry_run_by_default();
}

#[test]
fn npx_setup_explains_that_it_does_not_install_a_global_cli() {
    runtime::npx_setup_explains_that_it_does_not_install_a_global_cli();
}

#[test]
fn ambient_npx_metadata_does_not_replace_the_persistent_setup_launcher() {
    runtime::ambient_npx_metadata_does_not_replace_the_persistent_setup_launcher();
}

#[test]
fn ambient_npx_metadata_keeps_the_persistent_setup_handoff() {
    runtime::ambient_npx_metadata_keeps_the_persistent_setup_handoff();
}
