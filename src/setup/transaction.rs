use super::*;

#[derive(Debug)]
pub(super) struct ResolvedSetupPlan {
    pub(super) operation: SetupOperation,
    pub(super) persistent_cli: bool,
    pub(super) launcher: Option<LauncherPlan>,
    pub(super) runtime: Option<RuntimeInstallPlan>,
    pub(super) edits: Vec<PlannedClientEdit>,
    pub(super) discovery_edits: Vec<PlannedDiscoveryEdit>,
    pub(super) ownership_override: bool,
    pub(super) transaction_root: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SetupTransactionJournal {
    pub(super) schema_version: u32,
    pub(super) entries: Vec<SetupTransactionEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SetupTransactionEntry {
    pub(super) path: PathBuf,
    pub(super) original: Option<String>,
    pub(super) updated_hash: Option<String>,
    pub(super) updated_exists: bool,
}

pub(super) struct SetupTransaction {
    path: PathBuf,
}

pub(super) struct SetupLock {
    _file: fs::File,
}

pub(super) fn acquire_setup_lock(runtime_root: &Path) -> Result<SetupLock> {
    fs::create_dir_all(runtime_root)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(runtime_root.join("setup.lock"))?;
    file.lock()?;
    Ok(SetupLock { _file: file })
}

impl SetupTransaction {
    pub(super) fn commit(self) -> Result<()> {
        fs::remove_file(&self.path)?;
        sync_parent_directory(&self.path)?;
        Ok(())
    }
}

pub(super) fn transaction_path(runtime_root: &Path) -> PathBuf {
    runtime_root.join("setup-transaction-v1.json")
}

pub(super) fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

pub(super) fn recover_interrupted_transaction(runtime_root: &Path) -> Result<()> {
    let path = transaction_path(runtime_root);
    let Some(serialized) = read_optional(&path)? else {
        return Ok(());
    };
    let journal: SetupTransactionJournal = serde_json::from_str(&serialized).map_err(|error| {
        Error::SetupFailure(format!(
            "invalid setup recovery journal {}: {error}",
            path.display()
        ))
    })?;
    if journal.schema_version != 1 {
        return Err(Error::SetupFailure(format!(
            "unsupported setup recovery journal version at {}",
            path.display()
        )));
    }
    for entry in &journal.entries {
        let current = read_optional(&entry.path)?;
        let still_original = current == entry.original;
        let matches_applied = current.as_ref().is_some_and(|value| {
            entry.updated_exists
                && entry
                    .updated_hash
                    .as_deref()
                    .is_some_and(|hash| content_hash(value) == hash)
        }) || (!entry.updated_exists && current.is_none());
        if !still_original && !matches_applied {
            return Err(Error::SetupFailure(format!(
                "cannot recover interrupted setup because {} changed afterward",
                entry.path.display()
            )));
        }
        restore_path(&entry.path, entry.original.as_deref())?;
    }
    fs::remove_file(&path)?;
    sync_parent_directory(&path)?;
    Ok(())
}

pub(super) fn begin_setup_transaction(
    plan: &ResolvedSetupPlan,
) -> Result<Option<SetupTransaction>> {
    let mut entries = Vec::new();
    for edit in &plan.edits {
        if let Some(updated) = &edit.updated {
            entries.push(SetupTransactionEntry {
                path: edit.public.path.clone(),
                original: edit.original.clone(),
                updated_hash: Some(content_hash(updated)),
                updated_exists: true,
            });
        }
    }
    for edit in &plan.discovery_edits {
        let (updated_hash, updated_exists) = match edit.public.action {
            ClientPlanAction::Create | ClientPlanAction::Update => {
                (edit.updated.as_deref().map(content_hash), true)
            }
            ClientPlanAction::Remove => (None, false),
            ClientPlanAction::AlreadyCurrent | ClientPlanAction::NotConfigured => continue,
        };
        entries.push(SetupTransactionEntry {
            path: edit.public.path.clone(),
            original: edit.original.clone(),
            updated_hash,
            updated_exists,
        });
    }
    if entries.is_empty() {
        return Ok(None);
    }
    fs::create_dir_all(&plan.transaction_root)?;
    let path = transaction_path(&plan.transaction_root);
    if path.exists() {
        return Err(Error::SetupFailure(format!(
            "setup recovery journal already exists at {}",
            path.display()
        )));
    }
    let journal = SetupTransactionJournal {
        schema_version: 1,
        entries,
    };
    let serialized = serde_json::to_string(&journal)?;
    let mut temporary = NamedTempFile::new_in(&plan.transaction_root)?;
    temporary.write_all(serialized.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist_noclobber(&path).map_err(|error| {
        Error::SetupFailure(format!(
            "another setup transaction became active at {}: {}",
            path.display(),
            error.error
        ))
    })?;
    sync_parent_directory(&path)?;
    Ok(Some(SetupTransaction { path }))
}

pub(super) fn restore_path(path: &Path, original: Option<&str>) -> Result<()> {
    match original {
        Some(original) => {
            let current = read_optional(path)?.unwrap_or_default();
            write_if_changed(path, &current, original)
        }
        None => {
            if path.exists() {
                fs::remove_file(path)?;
                sync_parent_directory(path)?;
            }
            Ok(())
        }
    }
}
