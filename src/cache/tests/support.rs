    use super::*;
    use crate::{Config, IndexScope};
    use crate::config::{managed_cache_id, managed_cache_id_for_scope};
    use crate::services::Services;
    use crate::storage::Storage;

    const FIRST_ID: &str = "0000000000000001";
    const SECOND_ID: &str = "0000000000000002";

    fn request() -> CachePruneRequest {
        CachePruneRequest {
            older_than_days: None,
            max_total_bytes: None,
            remove_missing_roots: false,
            dry_run: true,
            yes: false,
        }
    }

    fn create_current_cache(
        manager: &CacheManager,
        repository: &Path,
        accessed_at: u64,
    ) -> (String, PathBuf) {
        let id = managed_cache_id(repository);
        let directory = manager.root.join(&id);
        fs::create_dir_all(&directory).expect("cache directory");
        let database = directory.join(DATABASE_NAME);
        drop(Storage::open_for_repository(&database, repository).expect("cache database"));
        Connection::open(&database)
            .expect("cache metadata")
            .execute(
                "UPDATE meta SET last_access_unix_seconds = ?1 WHERE id = 1",
                [i64::try_from(accessed_at).expect("test timestamp")],
            )
            .expect("access timestamp");
        (id, database)
    }

    fn create_scoped_cache(
        manager: &CacheManager,
        repository: &Path,
        scope: &IndexScope,
    ) -> (String, PathBuf) {
        let id = managed_cache_id_for_scope(repository, scope);
        let directory = manager.root.join(&id);
        fs::create_dir_all(&directory).expect("cache directory");
        let database = directory.join(DATABASE_NAME);
        drop(
            Storage::open_for_repository_scoped(
                &database,
                repository,
                scope.full_digest(),
            )
            .expect("scoped cache database"),
        );
        (id, database)
    }

    fn create_cache_with_content_identity(
        manager: &CacheManager,
        repository: &Path,
        accessed_at: u64,
        version: Option<u32>,
    ) -> (String, PathBuf) {
        let (current_id, database) = create_current_cache(manager, repository, accessed_at);
        let root_hash = current_id
            .split_once('-')
            .expect("versioned cache identity")
            .1;
        let id = version.map_or_else(
            || root_hash.to_owned(),
            |version| format!("v{version}-{root_hash}"),
        );
        let directory = manager.root.join(&id);
        fs::rename(database.parent().expect("cache directory"), &directory)
            .expect("move cache identity");
        (id, directory.join(DATABASE_NAME))
    }

    fn create_legacy_wal_cache(manager: &CacheManager, id: &str, accessed_at: u64) {
        let directory = manager.root.join(id);
        fs::create_dir_all(&directory).expect("cache directory");
        let source_database = manager
            .root
            .parent()
            .expect("managed cache parent")
            .join("legacy-source.sqlite");
        let connection = Connection::open(&source_database).expect("legacy database");
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE meta (
                     id INTEGER PRIMARY KEY,
                     schema_version INTEGER NOT NULL,
                     repository_root TEXT NOT NULL
                 );
                 INSERT INTO meta VALUES (1, 4, '');",
            )
            .expect("legacy WAL schema");

        for name in [DATABASE_NAME, WAL_NAME, "index.sqlite-shm"] {
            let source = if name == DATABASE_NAME {
                source_database.clone()
            } else {
                source_database.with_file_name(format!(
                    "{}{}",
                    source_database
                        .file_name()
                        .expect("source database name")
                        .to_string_lossy(),
                    &name[DATABASE_NAME.len()..]
                ))
            };
            fs::copy(source, directory.join(name)).expect("copy WAL artifact");
        }
        drop(connection);

        let modified = UNIX_EPOCH + Duration::from_secs(accessed_at);
        for name in [DATABASE_NAME, WAL_NAME, "index.sqlite-shm"] {
            let artifact = directory.join(name);
            assert!(
                artifact.exists(),
                "missing WAL artifact {}",
                artifact.display()
            );
            fs::File::options()
                .read(true)
                .write(true)
                .open(&artifact)
                .expect("open WAL artifact")
                .set_times(fs::FileTimes::new().set_modified(modified))
                .expect("set WAL artifact mtime");
        }
    }
