use super::*;

impl Indexer {
    /// Remove metadata-only watcher events before parse and publication.
    pub(super) fn remove_content_stable_candidates(
        &self,
        existing: &HashMap<String, crate::storage::FileRecord>,
        candidates: &mut HashMap<String, DiscoveredFile>,
        cancellation: &CancellationToken,
    ) -> Result<usize> {
        let mut content_stable = Vec::new();
        for (path, file) in candidates.iter() {
            check_cancelled(cancellation)?;
            if let Some(record) = existing.get(path)
                && record.size_bytes == file.size_bytes
                && content_unchanged(
                    &self.repository_root,
                    path,
                    &record.content_hash,
                    self.config.max_file_bytes,
                )
            {
                content_stable.push(path.clone());
            }
        }
        for path in &content_stable {
            candidates.remove(path);
        }
        Ok(content_stable.len())
    }

    pub(super) fn validate_membership_limits(
        &self,
        existing: &HashMap<String, crate::storage::FileRecord>,
        candidates: &HashMap<String, DiscoveredFile>,
        deletions: &HashSet<String>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let limits = self.config.discovery_limits();
        let mut files = 0u64;
        let mut total_source_bytes = 0u64;
        let mut admit = |size_bytes: u64| -> Result<()> {
            files = files.saturating_add(1);
            enforce_limit(crate::IndexLimitKind::Files, files, limits.max_files)?;
            total_source_bytes = total_source_bytes.saturating_add(size_bytes);
            enforce_limit(
                crate::IndexLimitKind::TotalSourceBytes,
                total_source_bytes,
                limits.max_total_source_bytes,
            )
        };

        for (path, record) in existing {
            check_cancelled(cancellation)?;
            if !deletions.contains(path) && !candidates.contains_key(path) {
                admit(record.size_bytes)?;
            }
        }
        for candidate in candidates.values() {
            check_cancelled(cancellation)?;
            admit(candidate.size_bytes)?;
        }
        Ok(())
    }

    pub(super) fn existing_files(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<HashMap<String, crate::storage::FileRecord>> {
        let mut result = HashMap::new();
        let mut cursor = None;
        loop {
            check_cancelled(cancellation)?;
            let page = self.storage.list_files(1_000, cursor)?;
            if page.is_empty() {
                break;
            }
            cursor = page.last().map(|file| file.id);
            for file in page {
                check_cancelled(cancellation)?;
                result.insert(file.path.clone(), file);
            }
        }
        Ok(result)
    }

    pub(super) fn config_hash(&self) -> String {
        self.config_hash_for_derivation(crate::index_derivation::index_derivation_fingerprint())
    }

    pub(super) fn config_hash_for_derivation(&self, derivation_fingerprint: &str) -> String {
        let input = format!(
            "leantoken-index-config-v2\0{derivation_fingerprint}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
            self.config.max_walk_entries,
            self.config.max_files,
            self.config.max_total_source_bytes,
            self.config.max_depth,
            self.config.max_file_bytes,
            self.config.max_prepare_batch_files,
            self.config.max_prepare_batch_bytes,
            self.config.include_generated,
            self.config.index_scope().identity_material(),
            self.config.chunk_lines,
            self.config.chunk_bytes,
            self.config.tokenizer.name()
        );
        blake3::hash(input.as_bytes()).to_hex().to_string()
    }
}
