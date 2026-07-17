use super::*;

fn patch(path: &str, hunk: &str) -> String {
    format!(
        "diff --git a/{path} b/{path}\nindex 1111111..2222222 100644\n--- a/{path}\n+++ b/{path}\n{hunk}"
    )
}

#[test]
fn removed_lines_and_insertion_neighbors_form_base_revision_regions() {
    let modified = patch(
        "src/lib.rs",
        "@@ -10,3 +10,3 @@\n keep\n-old\n+new\n keep2\n",
    );
    let evidence = extract_patch_evidence(&modified, false).expect("modified patch");
    assert_eq!(evidence.primary["src/lib.rs"], BTreeSet::from([11]));

    let inserted = patch(
        "src/lib.rs",
        "@@ -20,2 +20,3 @@\n before\n+inserted\n after\n",
    );
    let evidence = extract_patch_evidence(&inserted, false).expect("insert patch");
    assert_eq!(evidence.primary["src/lib.rs"], BTreeSet::from([20, 21]));
}

#[test]
fn added_files_are_counted_but_never_labeled_at_the_base_revision() {
    let added = "diff --git a/src/new.rs b/src/new.rs\nnew file mode 100644\nindex 0000000..2222222\n--- /dev/null\n+++ b/src/new.rs\n@@ -0,0 +1,2 @@\n+one\n+two\n";
    let evidence = extract_patch_evidence(added, false).expect("added patch");
    assert!(evidence.primary.is_empty());
    assert_eq!(evidence.unobservable_added_files, 1);
    assert_eq!(evidence.language_weights[&Language::Rust], 2);
}

#[test]
fn renamed_files_are_labeled_at_the_base_revision_path() {
    let renamed = "diff --git a/src/old.rs b/src/new.rs\nsimilarity index 90%\nrename from src/old.rs\nrename to src/new.rs\n--- a/src/old.rs\n+++ b/src/new.rs\n@@ -3,2 +3,2 @@\n-old\n+new\n keep\n";
    let evidence = extract_patch_evidence(renamed, false).expect("renamed patch");
    assert_eq!(evidence.primary["src/old.rs"], BTreeSet::from([3]));
    assert!(!evidence.primary.contains_key("src/new.rs"));
}

#[test]
fn tests_docs_and_test_patch_are_optional() {
    let test = patch("tests/lib.rs", "@@ -1,2 +1,2 @@\n-old\n+new\n keep\n");
    let evidence = extract_patch_evidence(&test, false).expect("test path");
    assert!(evidence.primary.is_empty());
    assert_eq!(evidence.optional["tests/lib.rs"], BTreeSet::from([1]));

    let source = patch("src/lib.rs", "@@ -1,2 +1,2 @@\n-old\n+new\n keep\n");
    let evidence = extract_patch_evidence(&source, true).expect("forced optional");
    assert!(evidence.primary.is_empty());
    assert_eq!(evidence.optional["src/lib.rs"], BTreeSet::from([1]));
}

#[test]
fn language_inference_splits_mixed_javascript_and_typescript_repositories() {
    let javascript = BTreeMap::from([(Language::Javascript, 4), (Language::Typescript, 1)]);
    assert_eq!(
        infer_task_language("babel/babel", &javascript),
        Some(Language::Javascript)
    );
    let typescript = BTreeMap::from([(Language::Javascript, 1), (Language::Typescript, 4)]);
    assert_eq!(
        infer_task_language("vuejs/core", &typescript),
        Some(Language::Typescript)
    );
    let tied = BTreeMap::from([(Language::Javascript, 1), (Language::Typescript, 1)]);
    assert_eq!(infer_task_language("axios/axios", &tied), None);
}

#[test]
fn exact_identifier_detection_ignores_urls_and_keeps_code_atoms() {
    assert!(!query_contains_exact_identifier(
        "The server returns the wrong response; see https://example.com/issue."
    ));
    assert!(!query_contains_exact_identifier(
        "The server returns the wrong response\n\nReproduction: `parseValue()` returns null"
    ));
    assert!(query_contains_exact_identifier(
        "Fix `Rack::Deflater` handling"
    ));
    assert!(query_contains_exact_identifier(
        "parseValue returns no result"
    ));
    assert!(query_contains_exact_identifier("Update src/lib.rs loading"));
}

#[test]
fn selection_is_seeded_stratified_and_repository_bounded() {
    let config = test_config();
    let candidates = (0..8)
        .map(|index| fixture_candidate(index, index % 2 == 0))
        .collect::<Vec<_>>();
    let selected = select_candidates(candidates, &config).expect("selection");
    assert_eq!(selected.len(), 4);
    assert_eq!(
        selected
            .iter()
            .filter(|candidate| !candidate.exact_identifier)
            .count(),
        2
    );
    let repositories = selected
        .iter()
        .map(|candidate| candidate.repository.as_str())
        .collect::<HashSet<_>>();
    assert_eq!(repositories.len(), 4);
}

#[test]
fn paths_reject_parent_and_absolute_components() {
    assert!(normalize_diff_path("../secret").is_err());
    assert!(normalize_diff_path("/secret").is_err());
    assert!(normalize_diff_path("src/lib.rs").is_ok());
}

#[test]
fn nearest_rank_percentiles_are_deterministic() {
    let values = [1, 2, 3, 4, 100];
    assert_eq!(nearest_rank(&values, 50), 3);
    assert_eq!(nearest_rank(&values, 95), 100);
    assert_eq!(nearest_rank(&values, 100), 100);
    assert_eq!(nearest_rank(&[], 50), 0);
}

#[test]
fn development_budget_rejects_estimated_token_counts() {
    let mut config = test_config();
    config.languages.insert(Language::Javascript);
    config.tokenizer = Tokenizer::Estimate;
    let error = validate_config(&config).expect_err("estimated tokenizer must be rejected");
    assert!(error.to_string().contains("exact tokenizer"));
}

#[test]
fn source_record_identity_is_independent_of_json_field_order() {
    let first = br#"{"repo":"owner/repo","instance_id":"owner__repo-1","base_commit":"0123456789abcdef0123456789abcdef01234567","patch":"patch","test_patch":"test","problem_statement":"problem","hints_text":"","created_at":"date","version":"1","FAIL_TO_PASS":["f"],"PASS_TO_PASS":["p"]}"#;
    let second = br#"{"PASS_TO_PASS":["p"],"FAIL_TO_PASS":["f"],"version":"1","created_at":"date","hints_text":"","problem_statement":"problem","test_patch":"test","patch":"patch","base_commit":"0123456789abcdef0123456789abcdef01234567","instance_id":"owner__repo-1","repo":"owner/repo"}"#;
    let first = parse_source_records(first).expect("first record");
    let second = parse_source_records(second).expect("second record");
    assert_eq!(first[0].1, second[0].1);
    assert_eq!(
        canonical_records_blake3(&first).unwrap(),
        canonical_records_blake3(&second).unwrap()
    );
}

#[test]
fn required_license_audit_rejects_unresolved_spdx_and_missing_revisions() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("licenses.json");
    let candidate = fixture_candidate(0, true);
    let mut license = RepositoryLicense {
        repository: candidate.repository.clone(),
        spdx_id: "NOASSERTION".into(),
        source_revision: candidate.task.repository.revision.clone(),
        license_path: "LICENSE".into(),
        license_file_blake3: "0".repeat(HASH_HEX_LEN),
        source_url: format!(
            "https://example.invalid/blob/{}/LICENSE",
            candidate.task.repository.revision
        ),
    };
    fs::write(&path, serde_json::to_vec(&vec![license.clone()]).unwrap()).unwrap();
    assert!(load_license_audit(Some(&path), std::slice::from_ref(&candidate)).is_err());

    license.spdx_id = "MIT".into();
    fs::write(&path, serde_json::to_vec(&vec![license.clone()]).unwrap()).unwrap();
    assert!(
        load_license_audit(Some(&path), std::slice::from_ref(&candidate))
            .unwrap()
            .is_some()
    );

    let mut second = fixture_candidate(1, false);
    second.repository.clone_from(&candidate.repository);
    second.task.repository.revision = "f".repeat(40);
    assert!(load_license_audit(Some(&path), &[candidate.clone(), second.clone()]).is_err());

    let mut second_license = license.clone();
    second_license.source_revision = second.task.repository.revision.clone();
    second_license.source_url = format!(
        "https://example.invalid/blob/{}/LICENSE",
        second_license.source_revision
    );
    fs::write(
        &path,
        serde_json::to_vec(&vec![second_license, license]).unwrap(),
    )
    .unwrap();
    let audited = load_license_audit(Some(&path), &[candidate, second])
        .unwrap()
        .expect("complete audit");
    assert_eq!(audited.len(), 2);
    assert!(audited[0].source_revision < audited[1].source_revision);
}

#[cfg(unix)]
#[test]
fn private_output_is_created_with_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("labels.jsonl");
    write_new(&path, b"{}\n", true).expect("private output");
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

fn test_config() -> PrepareConfig<'static> {
    PrepareConfig {
        dataset_jsonl: Path::new("dataset.jsonl"),
        source_artifact: Path::new("source.parquet"),
        source_revision: "0123456789abcdef0123456789abcdef01234567",
        source_url: "https://example.invalid/0123456789abcdef0123456789abcdef01234567/dataset",
        seed: "fixture",
        harness_revision: "0123456789abcdef0123456789abcdef01234567",
        harness_binary: Path::new("harness"),
        languages: BTreeSet::from([Language::Rust]),
        tasks_per_language: 4,
        non_exact_per_language: 2,
        max_tasks_per_repository: 1,
        source_token_budget: 2_000,
        tokenizer: Tokenizer::Cl100kBase,
        repository_license_map: None,
        require_license_audit: false,
        tasks_output: Path::new("tasks.jsonl"),
        labels_output: Path::new("labels.jsonl"),
        receipt_output: Path::new("receipt.json"),
    }
}

fn fixture_candidate(index: usize, exact_identifier: bool) -> Candidate {
    let task_id = format!("owner__repo-{index}");
    let repository = format!("owner/repo-{index}");
    let source_record_blake3 = "0".repeat(HASH_HEX_LEN);
    Candidate {
        task: DevelopmentTask {
            schema_version: SCHEMA_VERSION,
            dataset_kind: DATASET_KIND.into(),
            task_id: task_id.clone(),
            repository: RepositorySpec {
                url: format!("https://github.com/{repository}.git"),
                revision: "0123456789abcdef0123456789abcdef01234567".into(),
                path_style: "posix".into(),
            },
            query: "fixture".into(),
            language: Language::Rust,
            strata: TaskStrata {
                exact_identifier,
                task_shape: if exact_identifier {
                    "exact_identifier"
                } else {
                    "behavioral"
                }
                .into(),
                tags: vec![],
            },
            budget: Budget {
                kind: "source_tokens".into(),
                amount: 2_000,
                tokenizer: Tokenizer::Cl100kBase,
                token_count_exact: true,
            },
            source_record_blake3: source_record_blake3.clone(),
        },
        label: SealedLabel {
            schema_version: SCHEMA_VERSION,
            task_id,
            source_record_blake3,
            label_method: "fixture".into(),
            core_regions: vec![Region {
                path: "src/lib.rs".into(),
                start_line: 1,
                end_line: 1,
            }],
            optional_regions: vec![],
            gold_patch_blake3: "1".repeat(HASH_HEX_LEN),
            test_patch_blake3: "2".repeat(HASH_HEX_LEN),
            unobservable_added_files: 0,
        },
        repository,
        language: Language::Rust,
        exact_identifier,
        selection_key: selection_key("fixture", Language::Rust, &format!("task-{index}")),
    }
}
