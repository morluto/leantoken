use super::*;

#[test]
fn list_reports_current_metadata_and_ignores_non_cache_directories() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("managed");
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    let manager = CacheManager::new(root.clone(), 10_000);
    create_current_cache(&manager, &repository, 9_000);
    fs::create_dir_all(root.join("not-managed")).expect("unmanaged directory");

    let report = manager
        .list_with(&CacheListRequest::default())
        .expect("cache list");

    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.total_entries, 1);
    assert_eq!(report.matched_entries, 1);
    assert_eq!(report.returned_entries, 1);
    assert_eq!(report.ignored_entries, 1);
    assert_eq!(report.active_entries, 0);
    assert_eq!(report.missing_root_entries, 0);
    assert_eq!(report.state_counts["current"], 1);
    assert!(!report.summary_only);
    assert!(report.next_cursor.is_none());
    assert_eq!(report.entries[0].entry.state, CacheState::Current);
    assert_eq!(report.entries[0].entry.index_scope, IndexScopeMode::Full);
    assert_eq!(report.entries[0].entry.index_scope_digest, None);
    assert_eq!(
        report.entries[0].entry.index_content_version,
        Some(INDEX_CONTENT_VERSION)
    );
    assert_eq!(
        report.entries[0].entry.repository_root.as_deref(),
        Some(repository.as_path())
    );
    assert_eq!(report.entries[0].entry.repository_available, Some(true));
    assert_eq!(
        report.entries[0].entry.last_access_unix_seconds,
        Some(9_000)
    );
    assert_eq!(report.entries[0].entry.age_seconds, Some(1_000));
    assert_eq!(
        report.entries[0].entry.access_time_source,
        Some(AccessTimeSource::Database)
    );
    assert!(report.total_bytes > 0);
    assert_eq!(report.matched_bytes, report.total_bytes);
}

#[test]
fn list_distinguishes_scoped_cache_identity_without_exposing_patterns() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("managed");
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    let repository = fs::canonicalize(repository).expect("canonical repository");
    let manager = CacheManager::new(root, 10_000);
    let scope =
        IndexScope::new(vec!["src/**".into()], vec!["third_party/**".into()]).expect("scope");
    let (id, _) = create_scoped_cache(&manager, &repository, &scope);

    let report = manager
        .list_with(&CacheListRequest::default())
        .expect("cache list");
    let entry = report.entries.first().expect("scoped entry");
    assert_eq!(entry.entry.id, id);
    assert_eq!(entry.entry.index_scope, IndexScopeMode::Scoped);
    assert_eq!(entry.entry.index_scope_digest.as_deref(), scope.digest());
    assert_eq!(
        entry.entry.repository_root.as_deref(),
        Some(repository.as_path())
    );
    assert_eq!(entry.entry.state, CacheState::Current);
}

#[test]
fn list_separates_metadata_state_from_content_compatibility() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let manager = CacheManager::new(temp.path().join("managed"), 10_000);
    let repositories = (0..4)
        .map(|index| {
            let repository = temp.path().join(format!("repository-{index}"));
            fs::create_dir(&repository).expect("repository");
            repository
        })
        .collect::<Vec<_>>();
    let (current_id, _) = create_current_cache(&manager, &repositories[0], 9_000);
    let (older_id, _) = create_cache_with_content_identity(
        &manager,
        &repositories[1],
        8_000,
        Some(INDEX_CONTENT_VERSION - 1),
    );
    let (legacy_id, _) =
        create_cache_with_content_identity(&manager, &repositories[2], 7_000, None);
    let (future_id, _) = create_cache_with_content_identity(
        &manager,
        &repositories[3],
        6_000,
        Some(INDEX_CONTENT_VERSION + 1),
    );
    let corrupt_id = FIRST_ID;
    let corrupt = manager.root.join(corrupt_id);
    fs::create_dir_all(&corrupt).expect("corrupt cache directory");
    fs::write(corrupt.join(DATABASE_NAME), b"not sqlite").expect("corrupt database");

    let report = manager
        .list_with(&CacheListRequest::default())
        .expect("versioned cache list");

    assert_eq!(report.report_version, 2);
    assert_eq!(report.total_entries, 5);
    assert_eq!(report.state_counts["current"], 3);
    assert_eq!(report.state_counts["unsupported"], 1);
    assert_eq!(report.state_counts["corrupt"], 1);
    for compatibility in CacheCompatibility::ALL {
        assert_eq!(
            report.compatibility_counts[compatibility.label()].entries,
            1,
            "{compatibility:?}"
        );
    }
    assert_eq!(report.safely_reclaimable_incompatible_entries, 2);
    assert!(report.safely_reclaimable_incompatible_bytes > 0);
    let project = |id: &str| {
        report
            .entries
            .iter()
            .find(|entry| entry.entry.id == id)
            .map(|entry| (entry.entry.state, entry.compatibility))
            .expect("listed cache")
    };
    assert_eq!(
        project(&current_id),
        (CacheState::Current, CacheCompatibility::CompatibleCurrent)
    );
    assert_eq!(
        project(&older_id),
        (CacheState::Current, CacheCompatibility::ObsoleteOlder)
    );
    assert_eq!(
        project(&legacy_id),
        (CacheState::Current, CacheCompatibility::Unversioned)
    );
    assert_eq!(
        project(&future_id),
        (
            CacheState::Unsupported,
            CacheCompatibility::NewerUnsupported
        )
    );
    assert_eq!(
        project(corrupt_id),
        (CacheState::Corrupt, CacheCompatibility::Unknown)
    );
    let serialized = serde_json::to_value(&report).expect("serialize cache report");
    assert!(
        serialized["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| {
                entry["id"] == legacy_id
                    && entry["state"] == "current"
                    && entry["compatibility"] == "legacy_unversioned"
            })
    );
}

#[test]
fn list_filters_and_cursors_bind_every_compatibility_dimension() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let manager = CacheManager::new(temp.path().join("managed"), 10_000);
    for (index, version) in [
        INDEX_CONTENT_VERSION - 1,
        INDEX_CONTENT_VERSION - 2,
        INDEX_CONTENT_VERSION,
    ]
    .into_iter()
    .enumerate()
    {
        let repository = temp.path().join(format!("repository-{index}"));
        fs::create_dir(&repository).expect("repository");
        if version == INDEX_CONTENT_VERSION {
            create_current_cache(&manager, &repository, 9_000);
        } else {
            create_cache_with_content_identity(&manager, &repository, 9_000, Some(version));
        }
    }

    let first_request = CacheListRequest {
        limit: 1,
        incompatible_with_current: true,
        ..CacheListRequest::default()
    };
    let first = manager
        .list_with(&first_request)
        .expect("first incompatible page");
    assert_eq!(first.matched_entries, 2);
    assert_eq!(first.returned_entries, 1);
    let cursor = first.next_cursor.expect("next cursor");
    let second = manager
        .list_with(&CacheListRequest {
            limit: 1,
            cursor: Some(cursor.clone()),
            incompatible_with_current: true,
            ..CacheListRequest::default()
        })
        .expect("second incompatible page");
    assert_eq!(second.returned_entries, 1);
    assert!(second.next_cursor.is_none());

    for changed in [
        CacheListRequest {
            cursor: Some(cursor.clone()),
            compatibilities: vec![CacheCompatibility::ObsoleteOlder],
            incompatible_with_current: true,
            ..CacheListRequest::default()
        },
        CacheListRequest {
            cursor: Some(cursor.clone()),
            index_content_versions: vec![INDEX_CONTENT_VERSION - 1],
            incompatible_with_current: true,
            ..CacheListRequest::default()
        },
        CacheListRequest {
            cursor: Some(cursor.clone()),
            incompatible_with_current: false,
            ..CacheListRequest::default()
        },
    ] {
        assert!(matches!(
            manager.list_with(&changed),
            Err(Error::InvalidInput {
                field: "cache list cursor",
                reason: "does not match the active cache filters"
            })
        ));
    }

    let exact = manager
        .list_with(&CacheListRequest {
            index_content_versions: vec![INDEX_CONTENT_VERSION],
            ..CacheListRequest::default()
        })
        .expect("exact content-version filter");
    assert_eq!(exact.matched_entries, 1);
    assert_eq!(
        exact.entries[0].compatibility,
        CacheCompatibility::CompatibleCurrent
    );
}

#[test]
fn list_filters_summarizes_and_pages_with_filter_bound_cursors() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("managed");
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    // Config canonicalizes repository roots before binding cache metadata;
    // preserve that contract on macOS, where /var is commonly a symlink.
    let repository = fs::canonicalize(repository).expect("canonical repository");
    let manager = CacheManager::new(root.clone(), 10_000);
    for id in [FIRST_ID, SECOND_ID] {
        let directory = root.join(id);
        fs::create_dir_all(&directory).expect("corrupt cache directory");
        fs::write(directory.join(DATABASE_NAME), id.as_bytes()).expect("corrupt cache");
    }
    create_current_cache(&manager, &repository, 9_000);

    let first = manager
        .list_with(&CacheListRequest {
            limit: 2,
            ..CacheListRequest::default()
        })
        .expect("first cache page");
    assert_eq!(first.total_entries, 3);
    assert_eq!(first.matched_entries, 3);
    assert_eq!(first.returned_entries, 2);
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.entry.id.as_str())
            .collect::<Vec<_>>(),
        vec![FIRST_ID, SECOND_ID]
    );
    let cursor = first.next_cursor.clone().expect("next cache cursor");

    let second = manager
        .list_with(&CacheListRequest {
            limit: 2,
            cursor: Some(cursor.clone()),
            ..CacheListRequest::default()
        })
        .expect("second cache page");
    assert_eq!(second.returned_entries, 1);
    assert_eq!(second.entries[0].entry.state, CacheState::Current);
    assert!(second.next_cursor.is_none());

    let summary = manager
        .list_with(&CacheListRequest {
            summary: true,
            states: vec![CacheState::Corrupt],
            ..CacheListRequest::default()
        })
        .expect("corrupt cache summary");
    assert!(summary.summary_only);
    assert_eq!(summary.total_entries, 3);
    assert_eq!(summary.matched_entries, 2);
    assert_eq!(summary.returned_entries, 0);
    assert_eq!(summary.state_counts["corrupt"], 2);
    assert_eq!(summary.state_counts["current"], 0);
    assert!(summary.matched_bytes > 0);
    assert!(summary.entries.is_empty());
    assert!(summary.next_cursor.is_none());

    let by_root = manager
        .list_with(&CacheListRequest {
            repository_root: Some(repository.clone()),
            ..CacheListRequest::default()
        })
        .expect("repository cache filter");
    assert_eq!(by_root.matched_entries, 1);
    assert_eq!(by_root.entries[0].entry.state, CacheState::Current);
    assert_eq!(
        by_root.entries[0].entry.repository_root.as_deref(),
        Some(repository.as_path())
    );

    let error = manager
        .list_with(&CacheListRequest {
            states: vec![CacheState::Current],
            cursor: Some(cursor),
            ..CacheListRequest::default()
        })
        .expect_err("cursor must be bound to filters");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "cache list cursor",
            reason: "does not match the active cache filters"
        }
    ));
}

#[test]
fn list_rejects_invalid_response_bounds_and_cursors() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let invalid_root = temp.path().join("not-a-directory");
    fs::write(&invalid_root, b"must not be inspected").expect("invalid cache root fixture");
    let manager = CacheManager::new(invalid_root, 10_000);
    let zero = manager
        .list_with(&CacheListRequest {
            limit: 0,
            ..CacheListRequest::default()
        })
        .expect_err("zero cache list limit");
    assert!(matches!(
        zero,
        Error::InvalidInput {
            field: "cache list limit",
            ..
        }
    ));

    let excessive = manager
        .list_with(&CacheListRequest {
            limit: MAX_CACHE_LIST_LIMIT + 1,
            ..CacheListRequest::default()
        })
        .expect_err("excessive cache list limit");
    assert!(matches!(
        excessive,
        Error::RequestLimitExceeded {
            field: "cache list limit",
            requested,
            limit: MAX_CACHE_LIST_LIMIT,
        } if requested == MAX_CACHE_LIST_LIMIT + 1
    ));

    let malformed = manager
        .list_with(&CacheListRequest {
            cursor: Some("not-a-cache-cursor".into()),
            ..CacheListRequest::default()
        })
        .expect_err("malformed cache list cursor");
    assert!(matches!(
        malformed,
        Error::InvalidInput {
            field: "cache list cursor",
            ..
        }
    ));

    let summary_cursor = manager
        .list_with(&CacheListRequest {
            summary: true,
            cursor: Some("not-used".into()),
            ..CacheListRequest::default()
        })
        .expect_err("summary cursor conflict");
    assert!(matches!(
        summary_cursor,
        Error::InvalidInput {
            field: "cache list cursor",
            reason: "cannot be combined with summary mode"
        }
    ));

    let compatibility_limit = manager
        .list_with(&CacheListRequest {
            compatibilities: vec![
                CacheCompatibility::CompatibleCurrent;
                MAX_CACHE_COMPATIBILITY_FILTERS + 1
            ],
            ..CacheListRequest::default()
        })
        .expect_err("compatibility filter fan-out");
    assert!(matches!(
        compatibility_limit,
        Error::RequestLimitExceeded {
            field: "cache compatibility filters",
            ..
        }
    ));
    let version_limit = manager
        .list_with(&CacheListRequest {
            index_content_versions: vec![
                INDEX_CONTENT_VERSION;
                MAX_CACHE_CONTENT_VERSION_FILTERS + 1
            ],
            ..CacheListRequest::default()
        })
        .expect_err("content-version filter fan-out");
    assert!(matches!(
        version_limit,
        Error::RequestLimitExceeded {
            field: "cache content-version filters",
            ..
        }
    ));
    assert!(matches!(
        manager.list_with(&CacheListRequest {
            index_content_versions: vec![0],
            ..CacheListRequest::default()
        }),
        Err(Error::InvalidInput {
            field: "cache content-version filter",
            reason: "must be positive"
        })
    ));
}

#[test]
fn legacy_repository_only_identity_remains_visible_and_prunable() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("managed");
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    let repository = fs::canonicalize(repository).expect("canonical repository");
    let current_id = managed_cache_id(&repository);
    let legacy_id = current_id.split_once('-').expect("versioned identity").1;
    let directory = root.join(legacy_id);
    fs::create_dir_all(&directory).expect("legacy cache directory");
    let database = directory.join(DATABASE_NAME);
    drop(Storage::open_for_repository(&database, &repository).expect("legacy cache database"));
    let manager = CacheManager::new(root, 10_000);

    let listed = manager
        .list_with(&CacheListRequest::default())
        .expect("cache list");

    assert_eq!(listed.entries.len(), 1);
    assert_eq!(listed.entries[0].entry.index_content_version, None);
    assert_eq!(listed.entries[0].entry.state, CacheState::Current);

    let mut request = request();
    request.max_total_bytes = Some(0);
    let pruned = manager.prune(&request).expect("legacy prune plan");
    assert_eq!(pruned.results[0].action, CachePruneAction::WouldDelete);
    assert!(database.exists());
}
