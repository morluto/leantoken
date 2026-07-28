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
    pub fn status_without_initializing(config: Config) -> Result<StatusResponse> {
        config.validate()?;
        if !config.database_path.exists() {
            return Self::open(config)?.status_sync();
        }

        let coordination = IndexCoordination::for_database(&config.database_path);
        let operation = coordination.try_acquire_operation()?;
        let freshness = operation.is_none();
        let snapshot = Storage::read_only_status(&config.database_path, &config.root);
        if let Some(operation) = operation {
            operation.release()?;
        }
        let snapshot = snapshot?;
        Ok(status_response(
            &config,
            snapshot.generation,
            snapshot.counts,
            if freshness {
                Freshness::Reconciling
            } else {
                Freshness::Current
            },
        ))
    }

    fn status_sync(&self) -> Result<StatusResponse> {
        self.consistent_allow_empty(|session, generation| {
            let counts = session.counts()?;
            Ok(status_response(
                &self.config,
                generation,
                counts,
                self.freshness(),
            ))
        })
    }
}

fn status_response(
    config: &Config,
    generation: u64,
    counts: StorageCounts,
    freshness: Freshness,
) -> StatusResponse {
    let index_storage_bytes = sqlite_storage_bytes(&config.database_path);
    let index_amplification_ratio =
        (counts.source_bytes > 0).then(|| index_storage_bytes as f64 / counts.source_bytes as f64);
    StatusResponse {
        repository_root: config.root.display().to_string(),
        database_path: config.database_path.display().to_string(),
        index_content_version: INDEX_CONTENT_VERSION,
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
        languages: counts
            .languages
            .into_iter()
            .map(|(language, files)| LanguageCount { language, files })
            .collect(),
        warnings: Vec::new(),
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
