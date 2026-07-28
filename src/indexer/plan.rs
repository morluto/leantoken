impl Indexer {
    fn validate_membership_limits(
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

    fn existing_files(
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

    fn config_hash(&self) -> String {
        self.config_hash_for_content_marker(&format!(
            "leantoken-index-content-v{INDEX_CONTENT_VERSION}"
        ))
    }

    fn config_hash_for_content_marker(&self, index_content_marker: &str) -> String {
        let input = format!(
            "{index_content_marker}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
            env!("CARGO_PKG_VERSION"),
            self.config.max_walk_entries,
            self.config.max_files,
            self.config.max_total_source_bytes,
            self.config.max_depth,
            self.config.max_file_bytes,
            self.config.max_prepare_batch_files,
            self.config.max_prepare_batch_bytes,
            self.config.include_generated,
            self.config.chunk_lines,
            self.config.chunk_bytes,
            self.config.tokenizer.name()
        );
        blake3::hash(input.as_bytes()).to_hex().to_string()
    }
}
