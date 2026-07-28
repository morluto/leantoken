enum PreparedFile {
    Indexed(Box<IndexedFile>, usize, Option<String>),
    Binary(String),
    Oversized(String),
    Failed(String, String),
}

struct PublishedSourceBytes {
    sizes: HashMap<String, u64>,
    total: u64,
    limit: u64,
}

impl PublishedSourceBytes {
    fn new(
        existing: &HashMap<String, crate::storage::FileRecord>,
        deletions: &HashSet<String>,
        limit: u64,
    ) -> Self {
        let sizes = existing
            .iter()
            .filter(|(path, _)| !deletions.contains(*path))
            .map(|(path, record)| (path.clone(), record.size_bytes))
            .collect::<HashMap<_, _>>();
        let total = sizes
            .values()
            .fold(0u64, |total, size| total.saturating_add(*size));
        Self {
            sizes,
            total,
            limit,
        }
    }

    fn replace(&mut self, path: &str, size: u64) {
        let old = self.sizes.get(path).copied().unwrap_or(0);
        self.total = self.total.saturating_sub(old).saturating_add(size);
        self.sizes.insert(path.to_string(), size);
    }

    fn remove(&mut self, path: &str) {
        if let Some(size) = self.sizes.remove(path) {
            self.total = self.total.saturating_sub(size);
        }
    }

    fn enforce(&self) -> Result<()> {
        enforce_limit(
            crate::IndexLimitKind::TotalSourceBytes,
            self.total,
            self.limit,
        )
    }
}
