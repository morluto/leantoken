use super::*;

#[test]
fn unchanged_subjects_do_not_become_preservation_constraints() {
    for task in [
        "watcher periodic polling full reconciliation unchanged file hashing cooldown CPU",
        "Find how unchanged records are refreshed hashed and published",
        "Investigate unchanged entries that remain in the cache",
        "Ensure unchanged records are refreshed, hashed, and published",
        "Ensure cached unchanged records are refreshed",
        "Leave unchanged files out of the scan",
        "Leave cached unchanged files out of the scan",
        "Find where there are unchanged records",
        "Investigate why these are unchanged files",
        "Find where there are entirely unchanged records",
        "Find which files are completely unchanged records",
        "Find unchanged and stale records",
        "Ensure unchanged and stale records are refreshed",
        "Find how unchanged records and stale files are processed",
        "Find why files are not unchanged",
    ] {
        let queries = plan(task, 16).queries;
        assert!(
            queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PrimaryChange))
        );
        assert!(
            queries
                .iter()
                .all(|query| !query.has_facet(FacetKind::PreserveConstraint)),
            "an unchanged subject is not a preservation instruction: {task}"
        );
    }
}

#[test]
fn coordinated_unchanged_subjects_keep_primary_roles() {
    for task in [
        "Ensure cached unchanged and stale records are refreshed",
        "Ensure the cached unchanged and fresh entries are hashed",
        "Ensure the unchanged and stale files are published",
        "Find where there are unchanged and stale records",
    ] {
        let queries = plan(task, 16).queries;
        assert!(
            queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PrimaryChange))
        );
        assert!(
            queries
                .iter()
                .all(|query| !query.has_facet(FacetKind::PreserveConstraint)),
            "coordinated unchanged subjects must stay primary: {task}"
        );
    }
    for task in [
        "Ensure cached records unchanged and clients remain compatible",
        "Ensure output unchanged and clients remain compatible",
        "Ensure the cached output unchanged and error messages stable",
        "The wire format is unchanged and clients remain compatible",
    ] {
        assert!(
            plan(task, 16)
                .queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PreserveConstraint)),
            "a real subject precedes the preservation predicate: {task}"
        );
    }
}

#[test]
fn nominal_requirement_stops_at_the_subject_predicate() {
    for task in [
        "Find how unchanged records are processed when a full scan is required",
        "Find how unchanged records are refreshed because a retry is necessary",
        "Find unchanged entries when a full scan is required",
        "Find unchanged files if a retry is mandatory",
    ] {
        let queries = plan(task, 16).queries;
        assert!(
            queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PrimaryChange))
        );
        assert!(
            queries
                .iter()
                .all(|query| !query.has_facet(FacetKind::PreserveConstraint)),
            "a subordinate requirement does not govern unchanged subjects: {task}"
        );
    }
    for task in [
        "An unchanged API contract is required during the migration",
        "Unchanged response fields and stable error codes are mandatory",
    ] {
        assert!(
            plan(task, 16)
                .queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PreserveConstraint)),
            "a directly governed nominal requirement stays a constraint: {task}"
        );
    }
}

#[test]
fn unchanged_predicates_keep_preservation_constraints() {
    for task in [
        "The output must remain unchanged",
        "The output remains unchanged",
        "The output should stay unchanged",
        "The output stays unchanged",
        "The wire format is unchanged",
        "The serialized fields are unchanged",
        "The response must be unchanged",
        "Ensure the wire format is unchanged",
        "Ensure the wire format unchanged",
        "Ensuring the wire format unchanged",
        "The update ensures the wire format unchanged",
        "Leave the wire format unchanged",
        "Leaving the wire format unchanged",
        "Ensure unchanged records remain unchanged",
        "Leave unchanged files unchanged",
        "The output is unchanged during the refactor",
        "Ensure the output unchanged throughout the refactor",
        "Leave the output unchanged for compatibility",
        "Refactor the serializer while the wire format should remain completely unchanged",
        "The output should remain entirely unchanged during the refactor",
        "The serialized fields are fully unchanged",
        "The output is still unchanged",
        "The output remains otherwise unchanged",
        "The output is exactly unchanged",
        "Leave the wire format completely unchanged",
        "Refactor the serializer while the wire format is unchanged and clients remain compatible",
        "The wire format is unchanged and stable",
        "The output is unchanged unless the input changes",
        "The output is unchanged if the input is identical",
        "The output remains unchanged when the input is identical",
        "The output is unchanged in the new release",
        "The output is unchanged as specified",
        "Refactor the serializer while ensuring the wire format is unchanged",
        "Keep the output unchanged",
        "Preserve unchanged source files",
    ] {
        assert!(
            plan(task, 16)
                .queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PreserveConstraint)),
            "preservation predicate must remain a constraint: {task}"
        );
    }
}

#[test]
fn unchanged_passive_and_inflected_predicates_preserve_constraints() {
    for task in [
        "Refactor the serializer while the wire format should be left unchanged",
        "The wire format has remained unchanged",
        "Refactor the serializer while remaining unchanged",
        "The output remained unchanged",
        "The output stayed unchanged",
        "The output is staying unchanged",
        "The output is being left entirely unchanged",
        "The output must be kept unchanged",
    ] {
        assert!(
            plan(task, 16)
                .queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PreserveConstraint)),
            "passive or inflected preservation predicate must remain a constraint: {task}"
        );
    }
}

#[test]
fn later_unchanged_subjects_retain_primary_change_queries() {
    let queries = plan(
        "Fix watcher polling; unchanged records are refreshed hashed and published",
        16,
    )
    .queries;
    let subject_queries = queries
        .iter()
        .filter(|query| query.value == "records")
        .collect::<Vec<_>>();
    assert!(
        !subject_queries.is_empty(),
        "subject evidence must be queried"
    );
    assert!(subject_queries.iter().all(|query| {
        query.has_facet(FacetKind::PrimaryChange) && !query.has_facet(FacetKind::PreserveConstraint)
    }));
}

#[test]
fn unchanged_subject_prohibitions_keep_preservation_constraints() {
    for task in [
        "Refactor the serializer; ensure unchanged files are not modified",
        "Ensure unchanged records are never altered",
        "Ensure unchanged output is not deliberately rewritten",
        "Ensure unchanged files must not be modified",
    ] {
        assert!(
            plan(task, 16)
                .queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PreserveConstraint)),
            "an explicit prohibition must retain preservation evidence: {task}"
        );
    }
    for task in [
        "Find how unchanged files are not modified",
        "Ensure unchanged records are not processed",
        "Ensure unchanged files are processed when a scan is not modified",
    ] {
        let queries = plan(task, 16).queries;
        assert!(
            queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PrimaryChange))
        );
        assert!(
            queries
                .iter()
                .all(|query| !query.has_facet(FacetKind::PreserveConstraint)),
            "a fact, skipped operation or subordinate predicate stays primary: {task}"
        );
    }
}

#[test]
fn mutation_prohibitions_keep_distinct_query_roles() {
    for prohibition in [
        "do not modify unchanged files",
        "Ensure callers do not change unchanged files",
        "Ensure that callers do not change unchanged files",
        "Ensure callers do not change files when idle",
        "Ensure unchanged files are definitely not modified",
    ] {
        let task = format!("Refactor the serializer; {prohibition}");
        let clauses = task_clauses(&task);
        assert_eq!(clauses.len(), 2);
        assert_eq!(clauses[1].text, prohibition);
        assert!(matches!(
            clause_roles(clauses[0].text, true, false).as_slice(),
            [TaskRole::PrimaryChange]
        ));
        assert!(matches!(
            clause_roles(clauses[1].text, false, false).as_slice(),
            [TaskRole::PreserveConstraint]
        ));
        let queries = plan(&task, 16).queries;
        let serializer = queries
            .iter()
            .find(|query| query.value == "serializer")
            .expect("primary serializer query");
        assert!(
            serializer.has_facet(FacetKind::PrimaryChange),
            "{serializer:?}"
        );
        assert!(
            !serializer.has_facet(FacetKind::PreserveConstraint),
            "{serializer:?}"
        );
        let files = queries
            .iter()
            .find(|query| query.value == "files")
            .expect("preserved files query");
        assert!(files.has_facet(FacetKind::PreserveConstraint), "{files:?}");
        assert!(!files.has_facet(FacetKind::PrimaryChange), "{files:?}");
        assert_eq!(primary_task_text(&task), "Refactor the serializer");
    }
}

#[test]
fn mutation_prohibition_observations_and_skipped_operations_stay_primary() {
    for task in [
        "Find why workers do not modify unchanged files",
        "Find why workers do not change unchanged files",
        "Find that callers do not change unchanged files",
        "Ensure unchanged files are refreshed when workers do not modify records",
        "Ensure callers observe when workers do not change unchanged files",
        "Ensure callers observe that workers do not change unchanged files",
        "Do not process unchanged files",
        "Ensure callers do not process unchanged files",
        "Ensure that callers do not process unchanged files",
        "Ensure unchanged files are definitely not processed",
        "Find how unchanged files are definitely not modified",
        "Ensure unchanged files are refreshed when records are definitely not modified",
    ] {
        assert!(matches!(
            clause_roles(task, true, false).as_slice(),
            [TaskRole::PrimaryChange]
        ));
        assert!(
            plan(task, 16)
                .queries
                .iter()
                .all(|query| !query.has_facet(FacetKind::PreserveConstraint)),
            "an observation, subordinate predicate or skipped operation stays primary: {task}"
        );
        assert_eq!(primary_task_text(task), task);
    }
}

#[test]
fn nominal_unchanged_requirements_keep_preservation_constraints() {
    for task in [
        "Refactor the serializer; unchanged wire format is required",
        "The unchanged response shape is mandatory",
        "An unchanged API contract is necessary",
        "An unchanged output is needed",
    ] {
        assert!(
            plan(task, 16)
                .queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PreserveConstraint)),
            "nominal preservation requirement must remain a constraint: {task}"
        );
    }
}

#[test]
fn coordinated_unchanged_constraints_follow_each_subject() {
    let task = "Ensure unchanged records are refreshed and unchanged files are not modified";
    let queries = plan(task, 16).queries;
    for value in ["records", "refreshed"] {
        let query = queries
            .iter()
            .find(|query| query.value == value)
            .expect("primary subject query");
        assert!(query.has_facet(FacetKind::PrimaryChange), "{query:?}");
        assert!(!query.has_facet(FacetKind::PreserveConstraint), "{query:?}");
    }
    let files = queries
        .iter()
        .find(|query| query.value == "files")
        .expect("preserved subject query");
    assert!(files.has_facet(FacetKind::PreserveConstraint), "{files:?}");
    assert!(!files.has_facet(FacetKind::PrimaryChange), "{files:?}");
    assert_eq!(
        primary_task_text(task),
        "Ensure unchanged records are refreshed"
    );
    assert!(matches!(
        clause_roles("Ensure unchanged files are not modified", true, false).as_slice(),
        [TaskRole::PreserveConstraint]
    ));
    let shared = plan(
        "Ensure unchanged records and unchanged files are not modified",
        16,
    )
    .queries;
    for value in ["records", "files"] {
        let query = shared
            .iter()
            .find(|query| query.value == value)
            .expect("shared-copula subject query");
        assert!(query.has_facet(FacetKind::PreserveConstraint), "{query:?}");
    }
    for task in [
        "Ensure unchanged records are refreshed and unchanged files are not modified",
        "Ensure unchanged records are refreshed and unchanged values are hashed and unchanged files are not modified",
        "Unchanged records are refreshed and unchanged response fields are required",
        "Ensure unchanged records and unchanged files are not modified",
        "Unchanged response fields and unchanged error codes are mandatory",
        "Ensure unchanged files are not modified and unchanged records are refreshed",
    ] {
        assert!(
            plan(task, 16)
                .queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PreserveConstraint)),
            "a coordinated constraint must retain preservation evidence: {task}"
        );
    }
    for task in [
        "Find how unchanged records are refreshed and unchanged files are not modified",
        "Ensure unchanged records are refreshed and unchanged files are not processed",
        "Ensure unchanged records are refreshed when unchanged files are not modified",
        "Find unchanged records when unchanged output is required",
    ] {
        let queries = plan(task, 16).queries;
        assert!(
            queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PrimaryChange))
        );
        assert!(
            queries
                .iter()
                .all(|query| !query.has_facet(FacetKind::PreserveConstraint)),
            "a fact, skipped operation or subordinate subject stays primary: {task}"
        );
    }
}

#[test]
fn coordinated_unchanged_modifiers_preserve_scoped_roles() {
    for modifier in ["the", "the cached", "the entirely cached", "our"] {
        let task = format!(
            "Ensure unchanged records are refreshed and {modifier} unchanged files are not modified"
        );
        let clauses = task_clauses(&task);
        assert_eq!(clauses.len(), 2);
        assert_eq!(
            clauses[1].text,
            format!("{modifier} unchanged files are not modified")
        );
        assert!(clauses[1].inherited_instruction);
        assert!(matches!(
            clause_roles(clauses[1].text, false, clauses[1].inherited_instruction).as_slice(),
            [TaskRole::PreserveConstraint]
        ));
        assert_eq!(
            primary_task_text(&task),
            "Ensure unchanged records are refreshed"
        );
    }
    let task = "Ensure unchanged records are refreshed and the unchanged files are not modified";
    let queries = plan(task, 16).queries;
    for value in ["records", "refreshed"] {
        let query = queries
            .iter()
            .find(|query| query.value == value)
            .expect("primary subject query");
        assert!(query.has_facet(FacetKind::PrimaryChange), "{query:?}");
        assert!(!query.has_facet(FacetKind::PreserveConstraint), "{query:?}");
    }
    let files = queries
        .iter()
        .find(|query| query.value == "files")
        .expect("preserved subject query");
    assert!(files.has_facet(FacetKind::PreserveConstraint), "{files:?}");
    assert!(!files.has_facet(FacetKind::PrimaryChange), "{files:?}");
    for task in [
        "Ensure unchanged records are refreshed and the handler observes unchanged files are not modified",
        "Ensure unchanged records are refreshed and the files are unchanged records",
        "Ensure unchanged records are refreshed when the unchanged files are not modified",
    ] {
        assert_eq!(task_clauses(task).len(), 1);
        assert_eq!(primary_task_text(task), task);
        assert!(
            plan(task, 16)
                .queries
                .iter()
                .all(|query| !query.has_facet(FacetKind::PreserveConstraint)),
            "a different subject, predicate or subordinate scope stays primary: {task}"
        );
    }
    let shared = "Ensure unchanged records and the cached unchanged files are not modified";
    assert_eq!(task_clauses(shared).len(), 1);
    let queries = plan(shared, 16).queries;
    for value in ["records", "files"] {
        let query = queries
            .iter()
            .find(|query| query.value == value)
            .expect("shared-copula subject query");
        assert!(query.has_facet(FacetKind::PreserveConstraint), "{query:?}");
    }
}

#[test]
fn ordinary_ly_subjects_keep_preservation_predicates() {
    for task in ["Leave assembly unchanged", "Ensure family unchanged"] {
        assert!(matches!(
            clause_roles(task, true, false).as_slice(),
            [TaskRole::PreserveConstraint]
        ));
        assert!(
            plan(task, 16)
                .queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PreserveConstraint))
        );
    }
    for task in ["Ensure absolutely unchanged", "Leave entirely unchanged"] {
        assert!(matches!(
            clause_roles(task, true, false).as_slice(),
            [TaskRole::PrimaryChange]
        ));
    }
}

#[test]
fn coordinated_instructions_keep_distinct_query_roles() {
    let task =
        "Find how unchanged records are refreshed and ensure unchanged files are not modified";
    let queries = plan(task, 16).queries;
    for value in ["records", "refreshed"] {
        let query = queries
            .iter()
            .find(|query| query.value == value)
            .expect("primary subject query");
        assert!(query.has_facet(FacetKind::PrimaryChange), "{query:?}");
        assert!(!query.has_facet(FacetKind::PreserveConstraint), "{query:?}");
    }
    let files = queries
        .iter()
        .find(|query| query.value == "files")
        .expect("preserved subject query");
    assert!(files.has_facet(FacetKind::PreserveConstraint), "{files:?}");
    assert!(!files.has_facet(FacetKind::PrimaryChange), "{files:?}");
    assert_eq!(
        primary_task_text(task),
        "Find how unchanged records are refreshed"
    );
    for (task, primary) in [
        (
            "Find unchanged records and leave family unchanged",
            "Find unchanged records",
        ),
        (
            "Find unchanged records and ensure unchanged files and unchanged entries are not modified",
            "Find unchanged records",
        ),
    ] {
        let clauses = task_clauses(task);
        assert_eq!(clauses.len(), 2);
        assert!(matches!(
            clause_roles(clauses[1].text, false, clauses[1].inherited_instruction).as_slice(),
            [TaskRole::PreserveConstraint]
        ));
        assert_eq!(primary_task_text(task), primary);
    }
    for task in [
        "Find how unchanged records are refreshed and the handler ensures unchanged files are not modified",
        "Find how unchanged records are refreshed when a handler ensures unchanged files are not modified",
    ] {
        assert_eq!(task_clauses(task).len(), 1);
        assert_eq!(primary_task_text(task), task);
        assert!(
            plan(task, 16)
                .queries
                .iter()
                .all(|query| !query.has_facet(FacetKind::PreserveConstraint))
        );
    }
}

#[test]
fn nominal_unchanged_requirements_accept_predicate_modifiers() {
    for task in [
        "An unchanged API contract is absolutely required",
        "An unchanged API contract is semantically required",
        "Unchanged response fields are still mandatory",
    ] {
        assert!(matches!(
            clause_roles(task, true, false).as_slice(),
            [TaskRole::PreserveConstraint]
        ));
    }
    for task in [
        "An unchanged API contract is not absolutely required",
        "Ensure unchanged records are absolutely processed",
        "Ensure unchanged records are accidentally processed",
        "Find unchanged records when an API contract is absolutely required",
        "Find unchanged records when an unchanged API contract is absolutely required",
    ] {
        assert!(matches!(
            clause_roles(task, true, false).as_slice(),
            [TaskRole::PrimaryChange]
        ));
    }
}

#[test]
fn bound_predicate_modifiers_keep_preservation_roles() {
    for (task, value) in [
        (
            "Ensure unchanged files are not accidentally modified",
            "files",
        ),
        (
            "Ensure unchanged files are definitely never deliberately altered",
            "files",
        ),
        ("The wire format is semantically unchanged", "format"),
    ] {
        assert!(matches!(
            clause_roles(task, true, false).as_slice(),
            [TaskRole::PreserveConstraint]
        ));
        let queries = plan(task, 16).queries;
        let query = queries
            .iter()
            .find(|query| query.value == value)
            .expect("predicate evidence query");
        assert!(query.has_facet(FacetKind::PreserveConstraint), "{query:?}");
    }
    for task in [
        "Find which files are entirely unchanged records",
        "Find where there are semantically unchanged records",
    ] {
        assert!(matches!(
            clause_roles(task, true, false).as_slice(),
            [TaskRole::PrimaryChange]
        ));
        assert!(
            plan(task, 16)
                .queries
                .iter()
                .all(|query| !query.has_facet(FacetKind::PreserveConstraint))
        );
    }
}
