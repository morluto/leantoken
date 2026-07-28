static NEXT_INDEX_PROGRESS_REGISTRY_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

#[derive(Clone)]
struct IndexProgressRegistry {
    shared: Arc<Mutex<IndexProgressRegistryState>>,
    cache_namespace: Arc<str>,
    registry_id: u64,
}

#[derive(Default)]
struct IndexProgressRegistryState {
    next_attempt: u64,
    current: Option<IndexProgressAttemptState>,
}

struct IndexProgressAttemptState {
    internal_id: u64,
    attempt_id: String,
    active: bool,
    current_generation: u64,
    phase: IndexProgressPhase,
    started: Instant,
    started_unix_ms: u64,
    last_progress_unix_ms: u64,
    update_sequence: u64,
    walk_entries: u64,
    files_discovered: u64,
    discovered_source_bytes: u64,
    files_prepared: u64,
    files_staged: u64,
    preparation_batches: u64,
}

struct IndexProgressAttempt {
    registry: IndexProgressRegistry,
    cancellation: CancellationToken,
    internal_id: u64,
    finished: bool,
}

impl IndexProgressRegistry {
    fn new(cache_namespace: String) -> Self {
        Self {
            shared: Arc::new(Mutex::new(IndexProgressRegistryState::default())),
            cache_namespace: Arc::from(cache_namespace),
            registry_id: NEXT_INDEX_PROGRESS_REGISTRY_ID
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    fn start(
        &self,
        current_generation: u64,
        cancellation: &CancellationToken,
    ) -> IndexProgressAttempt {
        let started = Instant::now();
        let unix_duration = UNIX_EPOCH.elapsed().unwrap_or_default();
        let started_unix_ms = saturating_duration_millis(unix_duration);
        let mut state = self
            .shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.next_attempt = state.next_attempt.saturating_add(1);
        let internal_id = state.next_attempt;

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"leantoken-index-attempt-v1\0");
        hasher.update(&std::process::id().to_le_bytes());
        hasher.update(&unix_duration.as_nanos().to_le_bytes());
        hasher.update(&self.registry_id.to_le_bytes());
        hasher.update(&internal_id.to_le_bytes());
        let attempt_id = hasher.finalize().to_hex()[..32].to_owned();

        state.current = Some(IndexProgressAttemptState {
            internal_id,
            attempt_id,
            active: true,
            current_generation,
            phase: IndexProgressPhase::Discovery,
            started,
            started_unix_ms,
            last_progress_unix_ms: started_unix_ms,
            update_sequence: 1,
            walk_entries: 0,
            files_discovered: 0,
            discovered_source_bytes: 0,
            files_prepared: 0,
            files_staged: 0,
            preparation_batches: 0,
        });
        drop(state);

        IndexProgressAttempt {
            registry: self.clone(),
            cancellation: cancellation.clone(),
            internal_id,
            finished: false,
        }
    }

    fn snapshot(&self) -> Option<IndexProgressSnapshot> {
        let state = self
            .shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = state.current.as_ref()?;
        Some(IndexProgressSnapshot {
            cache_namespace: self.cache_namespace.to_string(),
            detail_available: true,
            active: current.active,
            current_generation: current.current_generation,
            attempt_id: Some(current.attempt_id.clone()),
            phase: Some(current.phase),
            started_unix_ms: Some(current.started_unix_ms),
            elapsed_ms: Some(saturating_duration_millis(current.started.elapsed())),
            last_progress_unix_ms: Some(current.last_progress_unix_ms),
            update_sequence: Some(current.update_sequence),
            walk_entries: Some(current.walk_entries),
            files_discovered: Some(current.files_discovered),
            discovered_source_bytes: Some(current.discovered_source_bytes),
            files_prepared: Some(current.files_prepared),
            files_staged: Some(current.files_staged),
            preparation_batches: Some(current.preparation_batches),
        })
    }

    fn update(
        &self,
        internal_id: u64,
        update: impl FnOnce(&mut IndexProgressAttemptState),
    ) {
        let mut state = self
            .shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(current) = state
            .current
            .as_mut()
            .filter(|current| current.internal_id == internal_id && current.active)
        else {
            return;
        };
        update(current);
        current.update_sequence = current.update_sequence.saturating_add(1);
        current.last_progress_unix_ms =
            saturating_duration_millis(UNIX_EPOCH.elapsed().unwrap_or_default());
    }
}

impl IndexProgressAttempt {
    fn phase(&self, phase: IndexProgressPhase) {
        self.registry
            .update(self.internal_id, |current| current.phase = phase);
    }

    fn discovered(&self, walk_entries: u64, files: u64, source_bytes: u64) {
        self.registry.update(self.internal_id, |current| {
            current.walk_entries = walk_entries;
            current.files_discovered = files;
            current.discovered_source_bytes = source_bytes;
        });
    }

    fn prepared_batch(&self, files: usize) {
        let files = u64::try_from(files).unwrap_or(u64::MAX);
        self.registry.update(self.internal_id, |current| {
            current.files_prepared = current.files_prepared.saturating_add(files);
            current.preparation_batches = current.preparation_batches.saturating_add(1);
        });
    }

    fn staged(&self, files: usize) {
        let files = u64::try_from(files).unwrap_or(u64::MAX);
        self.registry.update(self.internal_id, |current| {
            current.files_staged = current.files_staged.saturating_add(files);
        });
    }

    fn complete(&mut self, generation: u64) {
        self.registry.update(self.internal_id, |current| {
            current.current_generation = generation;
            current.phase = IndexProgressPhase::Completed;
            current.active = false;
        });
        self.finished = true;
    }
}

impl Drop for IndexProgressAttempt {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let phase = if self.cancellation.is_cancelled() {
            IndexProgressPhase::Cancelled
        } else {
            IndexProgressPhase::Failed
        };
        self.registry.update(self.internal_id, |current| {
            current.phase = phase;
            current.active = false;
        });
    }
}

fn saturating_duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) fn index_progress_cache_namespace(config: &Config) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"leantoken-index-progress-cache-v1\0");
    hasher.update(&INDEX_CONTENT_VERSION.to_le_bytes());
    hasher.update(config.database_path.as_os_str().as_encoded_bytes());
    hasher.finalize().to_hex()[..32].to_owned()
}
