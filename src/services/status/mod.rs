use super::*;

impl Services {
    pub async fn status(&self) -> Result<StatusResponse> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |_| this.status_sync())
            .await
    }

    /// Return status without initializing an existing SQLite cache.
    ///
    /// This keeps a read-only status request responsive while another process
    /// is creating, migrating, or indexing the cache. A missing cache still
    /// follows the normal open path so cold status reports an uninitialized
    /// repository and creates the cache as it did previously.
    pub fn status_without_initializing(mut config: Config) -> Result<StatusResponse> {
        config.validate()?;
        if let Some(fallback) = active_repository_cache_fallback(&config)? {
            config = fallback;
        }
        if !config.database_path.exists() {
            return Self::open(config)?.status_sync();
        }

        let coordination = IndexCoordination::for_database(&config.database_path);
        let operation = coordination.try_acquire_operation()?;
        let freshness = operation.is_none();
        let snapshot = Storage::read_only_status_scoped(
            &config.database_path,
            &config.root,
            config.index_scope().full_digest(),
        );
        if let Some(operation) = operation {
            operation.release()?;
        }
        let snapshot = snapshot?;
        let freshness = if freshness {
            Freshness::Reconciling
        } else {
            Freshness::Current
        };
        let index_progress = (snapshot.generation == 0)
            .then(|| unavailable_index_progress(&config, snapshot.generation, &freshness));
        Ok(status_response(
            &config,
            snapshot.generation,
            snapshot.counts,
            freshness,
            index_progress,
        ))
    }

    fn status_sync(&self) -> Result<StatusResponse> {
        self.consistent_allow_empty(|session, generation| {
            let counts = session.counts()?;
            let freshness = self.freshness();
            let index_progress = self.initial_index_progress(generation, &freshness);
            Ok(status_response(
                &self.config,
                generation,
                counts,
                freshness,
                index_progress,
            ))
        })
    }

    pub(crate) fn index_progress_for_retry(&self) -> IndexProgressSnapshot {
        let freshness = self.freshness();
        self.initial_index_progress(0, &freshness)
            .unwrap_or_else(|| unavailable_index_progress(&self.config, 0, &freshness))
    }

    fn initial_index_progress(
        &self,
        generation: u64,
        freshness: &Freshness,
    ) -> Option<IndexProgressSnapshot> {
        if generation > 0 {
            return None;
        }
        if let Some(local) = self.indexer.progress_snapshot()
            && (local.active
                || *freshness == Freshness::Current
                || local.current_generation > generation)
        {
            return Some(local);
        }
        Some(unavailable_index_progress(
            &self.config,
            generation,
            freshness,
        ))
    }
}

fn active_repository_cache_fallback(config: &Config) -> Result<Option<Config>> {
    let Some(fallback) = config.repository_cache_fallback() else {
        return Ok(None);
    };
    if !fallback.database_path.exists() {
        return Ok(None);
    }
    let coordination = IndexCoordination::for_database(&fallback.database_path);
    Ok(coordination
        .try_acquire_prune_lease()?
        .is_none()
        .then_some(fallback))
}

fn status_response(
    config: &Config,
    generation: u64,
    counts: StorageCounts,
    freshness: Freshness,
    index_progress: Option<IndexProgressSnapshot>,
) -> StatusResponse {
    let index_storage_bytes = sqlite_storage_bytes(&config.database_path);
    let index_amplification_ratio =
        (counts.source_bytes > 0).then(|| index_storage_bytes as f64 / counts.source_bytes as f64);
    let repository_cache_fallback = config.uses_repository_cache_fallback();
    StatusResponse {
        repository_root: config.root.display().to_string(),
        database_path: config.database_path.display().to_string(),
        repository_cache_fallback,
        index_content_version: INDEX_CONTENT_VERSION,
        index_scope: if config.index_scope().is_full() {
            IndexScopeMode::Full
        } else {
            IndexScopeMode::Scoped
        },
        index_scope_digest: config.index_scope().digest().map(str::to_owned),
        index_include_paths: config.index_scope().includes().to_vec(),
        index_exclude_paths: config.index_scope().excludes().to_vec(),
        repository_generation: generation,
        index_state: if generation == 0 {
            IndexState::Uninitialized
        } else {
            IndexState::Ready
        },
        working_tree_checked: false,
        freshness,
        file_count: counts.files,
        chunk_count: counts.chunks,
        symbol_count: counts.symbols,
        index_storage_bytes,
        indexed_source_bytes: counts.source_bytes,
        index_amplification_ratio,
        process_rss_bytes: process_rss_bytes(),
        index_progress,
        languages: counts
            .languages
            .into_iter()
            .map(|(language, files)| LanguageCount { language, files })
            .collect(),
        warnings: repository_cache_fallback
            .then(|| {
                "platform cache was not writable; using repository-local `.leantoken` storage"
                    .into()
            })
            .into_iter()
            .collect(),
    }
}

fn unavailable_index_progress(
    config: &Config,
    current_generation: u64,
    freshness: &Freshness,
) -> IndexProgressSnapshot {
    IndexProgressSnapshot {
        cache_namespace: index_progress_cache_namespace(config),
        detail_available: false,
        active: *freshness == Freshness::Reconciling,
        current_generation,
        attempt_id: None,
        phase: None,
        started_unix_ms: None,
        elapsed_ms: None,
        last_progress_unix_ms: None,
        update_sequence: None,
        walk_entries: None,
        files_discovered: None,
        discovered_source_bytes: None,
        files_prepared: None,
        files_staged: None,
        preparation_batches: None,
    }
}

fn sqlite_storage_bytes(path: &std::path::Path) -> u64 {
    ["", "-wal", "-shm"]
        .into_iter()
        .map(|suffix| {
            let mut candidate = path.as_os_str().to_os_string();
            candidate.push(suffix);
            fs::metadata(candidate).map_or(0, |metadata| metadata.len())
        })
        .fold(0, u64::saturating_add)
}

#[cfg(target_os = "linux")]
fn process_rss_bytes() -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix("VmRSS:")?.trim();
            let kibibytes = value.strip_suffix("kB")?.trim().parse::<u64>().ok()?;
            kibibytes.checked_mul(1024)
        })
}

#[cfg(not(target_os = "linux"))]
fn process_rss_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_status_reports_an_active_repository_fallback() {
        let root = tempfile::tempdir().expect("repository");
        fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("source");
        let managed = Config::discover(root.path(), None).expect("managed config");
        let fallback = managed
            .repository_cache_fallback()
            .expect("repository fallback");
        let fallback_path = fallback.database_path.clone();
        let _active_services = Services::open(fallback).expect("active fallback services");

        let status =
            Services::status_without_initializing(managed).expect("read active fallback status");

        assert!(status.repository_cache_fallback);
        assert_eq!(status.database_path, fallback_path.display().to_string());
        assert!(
            status
                .warnings
                .iter()
                .any(|warning| warning.contains("repository-local"))
        );
    }
}
