use super::*;
use std::time::Instant;

impl CacheManager {
    pub(super) fn compact(&self, request: &CacheCompactRequest) -> Result<CacheCompactReport> {
        validate_compact_request(request)?;
        let mode = MutationMode::parse(
            request.dry_run,
            request.yes,
            "cache compact requires --yes or --dry-run",
        )?;
        let mut results = Vec::with_capacity(request.ids.len());
        for id in request.ids.iter().collect::<BTreeSet<_>>() {
            let started = Instant::now();
            let mut row = CacheCompactResult {
                id: id.clone(),
                path: self.root.join(id),
                outcome: CacheCompactOutcome::SkippedUnsafe {
                    detail: "not inspected".into(),
                },
                size_bytes_before: None,
                size_bytes_after: None,
                database_bytes: None,
                reusable_bytes: None,
                reclaimed_bytes: 0,
                vacuum_committed: false,
                elapsed_millis: 0,
            };
            row.outcome = match self.compact_one(id, request, mode, &mut row) {
                Ok(outcome) => outcome,
                Err(error) => CacheCompactOutcome::Failed {
                    error: error.to_string(),
                },
            };
            row.elapsed_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            results.push(row);
        }
        Ok(CacheCompactReport {
            cache_root: self.root.clone(),
            dry_run: mode.is_dry_run(),
            reclaimed_bytes: results.iter().map(|row| row.reclaimed_bytes).sum(),
            results,
        })
    }

    fn compact_one(
        &self,
        id: &str,
        request: &CacheCompactRequest,
        mode: MutationMode,
        row: &mut CacheCompactResult,
    ) -> Result<CacheCompactOutcome> {
        let identity = parse_managed_cache_id(id).expect("validated cache identity");
        // Hold the no-follow directory handle throughout SQLite maintenance.
        let (directory, artifacts) = match open_managed_artifacts(&row.path) {
            Ok(opened) => opened,
            Err(detail) => return Ok(CacheCompactOutcome::SkippedUnsafe { detail }),
        };
        row.size_bytes_before = Some(artifacts.iter().map(|(_, bytes)| *bytes).sum());
        let coordination = IndexCoordination::for_database(&row.path.join(DATABASE_NAME));
        let Some(_lease) = coordination.try_acquire_prune_lease()? else {
            return Ok(CacheCompactOutcome::SkippedActive {
                detail: "cache lease is held by a running process".into(),
            });
        };
        // Pin the validated inode before any path-based SQLite open.
        let expected_database =
            compact_sqlite::pin_database(&directory, &row.path.join(DATABASE_NAME))?;
        if !compact_sqlite::has_wal_header(&expected_database)? {
            return Ok(CacheCompactOutcome::SkippedUnsafe {
                detail: "compaction requires an existing WAL database; rollback journals are not maintained".into(),
            });
        }
        let inspected = self.inspect_managed_cache(id, identity, false)?;
        row.size_bytes_before = Some(inspected.entry.size_bytes);
        if let Some(outcome) = compact_eligibility(inspected, request.max_database_bytes) {
            return Ok(outcome);
        }
        let path = fs::canonicalize(&row.path)?.join(DATABASE_NAME);
        let expected = same_file::Handle::from_file(directory.into_std_file())?;
        if let Some(detail) = compact_sqlite::unsafe_artifact_detail(&expected, &path, &row.path)? {
            return Ok(CacheCompactOutcome::SkippedUnsafe { detail });
        }
        let Some(connection) =
            compact_sqlite::open_pinned(&expected_database, &path, mode.is_dry_run())?
        else {
            return Ok(CacheCompactOutcome::SkippedUnsafe {
                detail: "cache database identity or link count could not be verified while opening SQLite".into(),
            });
        };
        if !compact_sqlite::opened_path_matches(&connection, &expected, &path)? {
            return Ok(CacheCompactOutcome::SkippedUnsafe {
                detail: "SQLite database path or cache directory identity could not be verified"
                    .into(),
            });
        }
        let (bytes, reusable) = compact_sqlite::page_space(&connection)?;
        row.database_bytes = Some(bytes);
        row.reusable_bytes = Some(reusable);
        if bytes > request.max_database_bytes {
            return Ok(CacheCompactOutcome::SkippedTooLarge);
        }
        if reusable < request.min_reclaim_bytes
            || reusable.saturating_mul(100)
                < bytes.saturating_mul(u64::from(request.min_reclaim_percent))
        {
            return Ok(CacheCompactOutcome::SkippedLowBenefit);
        }
        if mode.is_dry_run() {
            return Ok(CacheCompactOutcome::WouldCompact);
        }
        if let Some(detail) = compact_sqlite::space_shortage(
            &connection,
            path.parent().expect("database parent"),
            bytes,
        )? {
            return Ok(CacheCompactOutcome::SkippedInsufficientSpace { detail });
        }
        if !compact_sqlite::database_matches(&expected_database, &path)? {
            return Ok(CacheCompactOutcome::SkippedUnsafe {
                detail: "cache database identity or link count changed before VACUUM".into(),
            });
        }
        let result =
            compact_sqlite::vacuum(&connection, request.max_seconds, &mut row.vacuum_committed);
        drop(connection);
        // Observe artifacts after SQLite closes, including removed/recycled WALs.
        let after = scan_artifacts(&row.path)?.size_bytes;
        row.size_bytes_after = Some(after);
        row.reclaimed_bytes = row.size_bytes_before.unwrap_or(after).saturating_sub(after);
        result?;
        Ok(CacheCompactOutcome::Compacted)
    }
}

fn validate_compact_request(request: &CacheCompactRequest) -> Result<()> {
    if request.ids.is_empty() || request.ids.len() > MAX_CACHE_COMPACT_IDS {
        return Err(Error::InvalidRequest(format!(
            "cache compact requires 1–{MAX_CACHE_COMPACT_IDS} explicit --id values"
        )));
    }
    let mut unique = BTreeSet::new();
    for id in &request.ids {
        if parse_managed_cache_id(id).is_none() || !unique.insert(id) {
            return Err(Error::InvalidRequest(
                "cache compact identities must be valid and unique".into(),
            ));
        }
    }
    if request.min_reclaim_bytes == 0
        || !(1..=100).contains(&request.min_reclaim_percent)
        || request.max_database_bytes == 0
        || !(1..=3600).contains(&request.max_seconds)
    {
        return Err(Error::InvalidRequest(
            "cache compact requires positive byte bounds, 1–100 percent, and 1–3600 seconds".into(),
        ));
    }
    Ok(())
}

fn compact_eligibility(
    inspected: InspectedCache,
    max_database_bytes: u64,
) -> Option<CacheCompactOutcome> {
    if !inspected.safe_to_prune || inspected.entry.repository_root.is_none() {
        return Some(CacheCompactOutcome::SkippedUnsafe {
            detail: inspected.entry.detail.unwrap_or_else(|| {
                "cache metadata or repository ownership is not safe to compact".into()
            }),
        });
    }
    if inspected.entry.access_time_source != Some(AccessTimeSource::Database) {
        return Some(CacheCompactOutcome::SkippedUnsafe {
                detail: "cache access age comes from artifact mtimes; compaction would change retention order".into(),
            });
    }
    if inspected.entry.size_bytes > max_database_bytes {
        return Some(CacheCompactOutcome::SkippedTooLarge);
    }
    None
}
