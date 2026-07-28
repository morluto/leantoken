impl CacheManager {
    fn inspect_all(&self) -> Result<(Vec<InspectedCache>, usize)> {
        let read_dir = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), 0));
            }
            Err(error) => return Err(error.into()),
        };
        let mut entries = Vec::new();
        let mut ignored = 0usize;
        for entry in read_dir {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                ignored += 1;
                continue;
            };
            if !file_type.is_dir() || !is_cache_id(&id) {
                ignored += 1;
                continue;
            }
            let cache = self.inspect_cache(&id, true)?;
            if cache.entry.size_bytes == 0 && cache.entry.state == CacheState::Incomplete {
                continue;
            }
            entries.push(cache);
        }
        entries.sort_by(|left, right| left.entry.id.cmp(&right.entry.id));
        Ok((entries, ignored))
    }

    fn inspect_cache(&self, id: &str, probe_active: bool) -> Result<InspectedCache> {
        let path = self.root.join(id);
        let database = path.join(DATABASE_NAME);
        let identity = parse_managed_cache_id(id).expect("validated managed cache identity");
        let index_content_version = match identity {
            ManagedCacheIdentity::Legacy => None,
            ManagedCacheIdentity::Versioned(version) => Some(version),
        };
        let initial_scan = scan_artifacts(&path)?;
        let latest_access_mtime = initial_scan.latest_access_mtime;
        let mut unexpected = initial_scan.unexpected;
        let mut metadata_safe = true;

        let lease_path = coordination_sidecar_path(&database, LEASE_LOCK_SUFFIX);
        let active = if probe_active && lease_path.exists() {
            IndexCoordination::for_database(&database)
                .try_acquire_prune_lease()?
                .is_none()
        } else {
            false
        };
        let mut entry = CacheEntry {
            id: id.into(),
            path,
            index_content_version,
            repository_root: None,
            repository_available: None,
            last_access_unix_seconds: latest_access_mtime,
            access_time_source: latest_access_mtime.map(|_| AccessTimeSource::FileMtime),
            age_seconds: latest_access_mtime.map(|accessed| self.now.saturating_sub(accessed)),
            schema_version: None,
            size_bytes: initial_scan.size_bytes,
            active,
            state: CacheState::Incomplete,
            detail: None,
        };

        let database_is_regular =
            fs::symlink_metadata(&database).is_ok_and(|metadata| metadata.file_type().is_file());
        if initial_scan.has_artifacts && database_is_regular {
            match inspect_database(&database) {
                Ok(metadata) => {
                    entry.schema_version = metadata.schema_version;
                    entry.repository_root = metadata.repository_root;
                    entry.repository_available =
                        entry.repository_root.as_deref().and_then(root_available);
                    if let Some(accessed) = metadata.last_access_unix_seconds {
                        entry.last_access_unix_seconds = Some(accessed);
                        entry.access_time_source = Some(AccessTimeSource::Database);
                        entry.age_seconds = Some(self.now.saturating_sub(accessed));
                    }
                    entry.state = if metadata.future_schema {
                        metadata_safe = false;
                        entry.detail = Some("cache uses a newer unsupported schema".into());
                        CacheState::Unsupported
                    } else if metadata.current {
                        CacheState::Current
                    } else {
                        CacheState::Legacy
                    };
                    if let Some(repository_root) = &entry.repository_root
                        && !managed_cache_id_matches_root(id, repository_root)
                    {
                        metadata_safe = false;
                        entry.state = CacheState::Unsupported;
                        entry.detail =
                            Some("cache identity does not match its recorded root".into());
                    }
                }
                Err(error) => {
                    metadata_safe = false;
                    entry.state = CacheState::Corrupt;
                    entry.detail = Some(error.to_string());
                }
            }
        }
        if index_content_version.is_some_and(|version| version > INDEX_CONTENT_VERSION) {
            metadata_safe = false;
            entry.state = CacheState::Unsupported;
            entry.detail = Some("cache uses a newer index-content version".into());
        }
        let final_scan = scan_artifacts(&entry.path)?;
        entry.size_bytes = final_scan.size_bytes;
        unexpected |= final_scan.unexpected;
        if unexpected {
            entry.state = CacheState::Unrecognized;
            entry.detail = Some("cache directory contains unexpected entries".into());
        }
        let compatibility = CacheCompatibility::classify(&entry);

        Ok(InspectedCache {
            safe_to_prune: final_scan.has_artifacts && !unexpected && metadata_safe,
            entry,
            compatibility,
        })
    }
}
