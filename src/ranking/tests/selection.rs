use super::*;

#[test]
fn known_hash_satisfies_must_cover_without_resending_source() {
    let required = Candidate::new("src/required.rs", 1, 1, "required")
        .symbol_name("required_symbol")
        .target_range(CandidateTargetRange::new(1, 1).unwrap())
        .exact(1.0);
    let known_hash = required.content_hash();
    let mut request = request_with_budget(100);
    request.must_include_paths = vec!["src/required.rs".into()];
    request.must_include_symbols = vec!["required_symbol".into()];
    request.known_hashes = vec![known_hash];

    let response = select(vec![required], &request, 1);

    assert!(response.fragments.is_empty());
    assert_eq!(response.omission_summary.known_hash, 1);
    assert_eq!(
        response.coverage.covered_must_include_paths,
        vec!["src/required.rs"]
    );
    assert_eq!(
        response.coverage.covered_must_include_symbols,
        vec!["required_symbol"]
    );
    assert!(response.coverage.uncovered_must_include_paths.is_empty());
    assert!(response.coverage.uncovered_must_include_symbols.is_empty());
}

#[test]
fn broad_allocation_does_not_treat_surface_acronyms_as_exact_owners() {
    let adapter = Candidate::new("src/mcp/runtime.rs", 1, 1, "MCP adapter")
        .concept("mcp", 2.0)
        .facet("primary_change", "mcp")
        .facet("exact_atom", "mcp")
        .facet("exact_atom", "LegacySearchResponse")
        .exact(10.0);
    let owner = Candidate::new("src/services/search.rs", 1, 1, "search projection owner")
        .concept("search projection", 2.0)
        .facet("primary_change", "search projection")
        .exact(1.0);
    let mut request = request_with_budget(100);
    request.max_fragments = Some(1);

    let response = select(vec![adapter, owner], &request, 1);

    assert_eq!(response.fragments[0].path, "src/services/search.rs");
}

#[test]
fn broad_allocation_prefers_the_owner_matching_more_primary_facets() {
    let generic = Candidate::new("src/main/dispatch.rs", 1, 1, "generic projection")
        .concept("projection", 2.0)
        .facet("primary_change", "projection")
        .exact(10.0);
    let owner = Candidate::new("src/services/search.rs", 1, 1, "search projection")
        .concept("search", 2.0)
        .facet("primary_change", "projection")
        .facet("primary_change", "search")
        .exact(1.0);
    let same_path_import = Candidate::new(
        "src/services/search.rs",
        10,
        10,
        "re-exported search surface",
    )
    .representation("import_symbol")
    .facet("primary_change", "projection")
    .facet("primary_change", "search")
    .facet("primary_change", "surface")
    .exact(9.75);
    let unrelated = Candidate::new("src/compiler/parse.rs", 1, 1, "compiler facets")
        .concept("compiler", 2.0)
        .facet("primary_change", "compiler")
        .facet("primary_change", "template")
        .facet("primary_change", "transform")
        .exact(9.0);
    let import_surface = Candidate::new("src/lib.rs", 1, 1, "re-exported projection search")
        .representation("import_symbol")
        .facet("primary_change", "projection")
        .facet("primary_change", "search")
        .facet("primary_change", "surface")
        .exact(9.5);
    let mut request = request_with_budget(100);
    request.max_fragments = Some(1);

    let response = select(
        vec![generic, same_path_import, import_surface, unrelated, owner],
        &request,
        1,
    );

    assert_eq!(response.fragments[0].path, "src/services/search.rs");
    assert_eq!(response.fragments[0].content, "search projection");
}

#[test]
fn broad_allocation_falls_back_to_a_supporting_owner_when_no_source_candidate_exists() {
    let documentation = Candidate::new("README.md", 1, 1, "generic documentation")
        .concept("docs", 2.0)
        .facet("primary_change", "docs")
        .exact(10.0);
    let owner = Candidate::new("Cargo.toml", 1, 1, "feature configuration")
        .concept("feature", 2.0)
        .concept("configuration", 2.0)
        .facet("primary_change", "feature")
        .facet("primary_change", "configuration")
        .exact(1.0);
    let mut request = request_with_budget(100);
    request.max_fragments = Some(1);

    let response = select(vec![documentation, owner], &request, 1);

    assert_eq!(response.fragments[0].path, "Cargo.toml");
}

#[test]
fn broad_allocation_pairs_generic_layout_owners_with_their_tests() {
    let adjacent_definition = Candidate::new("context.go", 1, 1, "Context.Errors")
        .concept("errors", 2.0)
        .facet("primary_change", "errors")
        .facet("exact_atom", "Context.Errors")
        .exact(1.0);
    let owner = Candidate::new("recovery.go", 1, 1, "recover the panic")
        .concept("recovery", 2.0)
        .facet("primary_change", "recovery")
        .exact(10.0);
    let unrelated_test = Candidate::new("benchmarks_test.go", 1, 1, "benchmark errors")
        .concept("errors", 2.0)
        .facet("exact_atom", "Context.Errors")
        .exact(9.0);
    let owner_test = Candidate::new("recovery_test.go", 1, 1, "recovery regression")
        .concept("panic-regression", 2.0)
        .exact(0.5);
    let mut request = request_with_budget(100);
    request.max_fragments = Some(2);

    let response = select(
        vec![adjacent_definition, unrelated_test, owner_test, owner],
        &request,
        1,
    );
    let paths = response
        .fragments
        .iter()
        .map(|fragment| fragment.path.as_str())
        .collect::<Vec<_>>();

    assert_eq!(paths, vec!["recovery.go", "recovery_test.go"]);
}

#[test]
fn broad_allocation_uses_task_atoms_to_choose_between_generic_tests() {
    let owner = Candidate::new("powershell_completions.go", 1, 1, "completion owner")
        .concept("completion", 2.0)
        .facet("primary_change", "completion")
        .exact(10.0);
    let generic_test = Candidate::new("bash_completions_test.go", 1, 1, "generic test")
        .concept("completion", 2.0)
        .exact(9.0);
    let task_test = Candidate::new("completions_test.go", 1, 1, "os.Args test")
        .concept("os.args", 2.0)
        .facet("exact_atom", "os.args")
        .exact(0.5);
    let mut request = request_with_budget(100);
    request.max_fragments = Some(2);

    let response = select(vec![generic_test, task_test, owner], &request, 1);
    let paths = response
        .fragments
        .iter()
        .map(|fragment| fragment.path.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        paths,
        vec!["powershell_completions.go", "completions_test.go"]
    );
}

#[test]
fn broad_allocation_does_not_pair_same_stem_tests_from_another_package() {
    let owner = Candidate::new("packages/a/index.ts", 1, 1, "package entrypoint")
        .concept("entrypoint", 2.0)
        .facet("primary_change", "entrypoint")
        .exact(10.0);
    let unrelated_test =
        Candidate::new("packages/b/index.test.ts", 1, 1, "other package regression")
            .concept("entrypoint", 2.0)
            .exact(9.0);
    let owner_test = Candidate::new("packages/a/index.spec.ts", 1, 1, "owner package regression")
        .concept("entrypoint", 2.0)
        .exact(0.5);
    let mut request = request_with_budget(100);
    request.max_fragments = Some(2);

    let response = select(vec![unrelated_test, owner_test, owner], &request, 1);
    let paths = response
        .fragments
        .iter()
        .map(|fragment| fragment.path.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        paths,
        vec!["packages/a/index.ts", "packages/a/index.spec.ts"]
    );
}

#[test]
fn preservation_exact_atoms_do_not_override_the_primary_owner() {
    let preservation = Candidate::new("src/model/search.rs", 1, 1, "LegacySearchResponse")
        .concept("legacy_search_response", 2.0)
        .facet("preserve_constraint", "legacy_search_response")
        .facet("exact_atom", "legacy_search_response")
        .exact(10.0);
    let owner = Candidate::new("src/services/search.rs", 1, 1, "search owner")
        .concept("search", 2.0)
        .facet("primary_change", "search")
        .exact(1.0);
    let mut request = request_with_budget(100);
    request.max_fragments = Some(1);

    let response = select(vec![preservation, owner], &request, 1);

    assert_eq!(response.fragments[0].path, "src/services/search.rs");
}

#[test]
fn decisive_second_view_prefers_the_definition_path() {
    let definition = Candidate::new("owner.rs", 1, 1, "definition")
        .concept("handle", 2.0)
        .representation("symbol")
        .exact(10.0);
    let owner_source = Candidate::new("owner.rs", 10, 10, "owner_source")
        .concept("handle", 2.0)
        .exact(0.5);
    let unrelated_source = Candidate::new("other.rs", 1, 1, "other ".repeat(3_000))
        .concept("handle", 2.0)
        .exact(1.0);

    let response = select(
        vec![unrelated_source, owner_source, definition],
        &request_with_budget(1_200),
        1,
    );

    assert_eq!(response.fragments.len(), 2);
    assert_eq!(response.fragments[0].path, "owner.rs");
    assert_eq!(response.fragments[1].path, "owner.rs");
}
