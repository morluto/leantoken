use super::*;

#[derive(Debug)]
pub(super) struct ResolvedSetupPlan {
    pub(super) operation: SetupOperation,
    pub(super) persistent_cli: bool,
    pub(super) launcher: Option<LauncherPlan>,
    pub(super) runtime: Option<RuntimeInstallPlan>,
    pub(super) edits: Vec<PlannedClientEdit>,
    pub(super) discovery_edits: Vec<PlannedDiscoveryEdit>,
    pub(super) configuration_snapshots: Vec<PlannedConfigurationSnapshot>,
    pub(super) ownership_override: bool,
    pub(super) transaction_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SetupTransactionJournal {
    pub(super) schema_version: u32,
    #[serde(default)]
    pub(super) state: SetupTransactionState,
    pub(super) entries: Vec<SetupTransactionEntry>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SetupTransactionState {
    #[default]
    Pending,
    Committed,
}

#[derive(Debug, Clone)]
pub(super) struct SetupTransactionEntry {
    pub(super) path: PathBuf,
    pub(super) original: Option<String>,
    pub(super) updated: SetupTransactionUpdate,
}

#[derive(Debug, Clone)]
pub(super) enum SetupTransactionUpdate {
    Present { content_hash: String },
    Absent,
}

impl Serialize for SetupTransactionEntry {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct WireEntry<'a> {
            path: &'a Path,
            original: &'a Option<String>,
            updated_hash: Option<&'a str>,
            updated_exists: bool,
        }

        let (updated_hash, updated_exists) = match &self.updated {
            SetupTransactionUpdate::Present { content_hash } => (Some(content_hash.as_str()), true),
            SetupTransactionUpdate::Absent => (None, false),
        };
        WireEntry {
            path: &self.path,
            original: &self.original,
            updated_hash,
            updated_exists,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SetupTransactionEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireEntry {
            path: PathBuf,
            original: Option<String>,
            updated_hash: Option<String>,
            updated_exists: bool,
        }

        let wire = WireEntry::deserialize(deserializer)?;
        let updated = match (wire.updated_exists, wire.updated_hash) {
            (true, Some(content_hash)) => SetupTransactionUpdate::Present { content_hash },
            (false, None) => SetupTransactionUpdate::Absent,
            (true, None) => {
                return Err(serde::de::Error::custom(
                    "updated_hash is required when updated_exists is true",
                ));
            }
            (false, Some(_)) => {
                return Err(serde::de::Error::custom(
                    "updated_hash must be absent when updated_exists is false",
                ));
            }
        };
        Ok(Self {
            path: wire.path,
            original: wire.original,
            updated,
        })
    }
}

pub(super) struct SetupTransaction {
    path: PathBuf,
    journal: SetupTransactionJournal,
}

pub(super) struct SetupLock {
    _file: fs::File,
    runtime_root: cap_std::fs::Dir,
}

pub(super) fn acquire_setup_lock(runtime_root: &Path) -> Result<SetupLock> {
    fs::create_dir_all(runtime_root)?;
    let runtime_root = open_runtime_root(runtime_root)?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    let file = runtime_root.open_with("setup.lock", &options)?.into_std();
    file.lock()?;
    Ok(SetupLock {
        _file: file,
        runtime_root,
    })
}

impl SetupLock {
    pub(super) fn runtime_root(&self) -> &cap_std::fs::Dir {
        &self.runtime_root
    }
}

impl SetupTransaction {
    pub(super) fn commit(&self) -> Result<()> {
        let mut committed = self.journal.clone();
        committed.state = SetupTransactionState::Committed;
        replace_journal(&self.path, &committed)?;
        if let Err(error) = fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(path = %self.path.display(), %error, "setup transaction cleanup will retry after a durable commit");
            return Ok(());
        }
        if let Err(error) = sync_parent_directory(&self.path) {
            tracing::debug!(path = %self.path.display(), %error, "setup transaction cleanup directory sync will retry after a durable commit");
        }
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
    let Some(serialized) = read_optional_with_limit(&path, MAX_SETUP_JOURNAL_BYTES)? else {
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
    if journal.state == SetupTransactionState::Committed {
        remove_journal(&path)?;
        return Ok(());
    }
    for entry in &journal.entries {
        let current = read_optional(&entry.path)?;
        let still_original = current == entry.original;
        let matches_applied = match &entry.updated {
            SetupTransactionUpdate::Present { content_hash: hash } => current
                .as_ref()
                .is_some_and(|value| content_hash(value) == *hash),
            SetupTransactionUpdate::Absent => current.is_none(),
        };
        if !still_original && !matches_applied {
            return Err(Error::SetupFailure(format!(
                "cannot recover interrupted setup because {} changed afterward",
                entry.path.display()
            )));
        }
        restore_path(&entry.path, entry.original.as_deref())?;
    }
    remove_journal(&path)?;
    Ok(())
}

pub(super) fn begin_setup_transaction(
    plan: &ResolvedSetupPlan,
) -> Result<Option<SetupTransaction>> {
    let mut entries = Vec::new();
    for edit in &plan.edits {
        if let Some(updated) = edit.updated() {
            entries.push(SetupTransactionEntry {
                path: edit.public.path.clone(),
                original: edit.original().map(str::to_owned),
                updated: SetupTransactionUpdate::Present {
                    content_hash: content_hash(updated),
                },
            });
        }
    }
    for edit in &plan.discovery_edits {
        let updated = match edit.public.action {
            ClientPlanAction::Create | ClientPlanAction::Update => {
                SetupTransactionUpdate::Present {
                    content_hash: content_hash(edit.updated.as_deref().ok_or_else(|| {
                        Error::SetupFailure(format!(
                            "setup plan omitted updated content for {}",
                            edit.public.path.display()
                        ))
                    })?),
                }
            }
            ClientPlanAction::Remove => SetupTransactionUpdate::Absent,
            ClientPlanAction::AlreadyCurrent | ClientPlanAction::NotConfigured => continue,
        };
        entries.push(SetupTransactionEntry {
            path: edit.public.path.clone(),
            original: edit.original.clone(),
            updated,
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
        state: SetupTransactionState::Pending,
        entries,
    };
    let serialized = serialize_journal(&journal)?;
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
    Ok(Some(SetupTransaction { path, journal }))
}

fn serialize_journal(journal: &SetupTransactionJournal) -> Result<String> {
    let serialized = serde_json::to_string(journal)?;
    if serialized.len() as u64 > MAX_SETUP_JOURNAL_BYTES {
        return Err(Error::SetupFailure(format!(
            "setup recovery journal exceeds the {MAX_SETUP_JOURNAL_BYTES}-byte aggregate limit"
        )));
    }
    Ok(serialized)
}

fn replace_journal(path: &Path, journal: &SetupTransactionJournal) -> Result<()> {
    reject_symlink_target(path)?;
    let parent = path.parent().ok_or_else(|| {
        Error::SetupFailure(format!(
            "setup transaction path has no parent: {}",
            path.display()
        ))
    })?;
    let serialized = serialize_journal(journal)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(serialized.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| Error::Io(error.error))?;
    sync_parent_directory(path)
}

fn remove_journal(path: &Path) -> Result<()> {
    reject_symlink_target(path)?;
    match fs::remove_file(path) {
        Ok(()) => sync_parent_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn restore_path(path: &Path, original: Option<&str>) -> Result<()> {
    match original {
        Some(original) => {
            let current = read_optional(path)?;
            // A missing file with a recorded original means the entry was
            // never persisted; recreate it as the original content.
            write_if_changed(path, current.as_deref(), original)
        }
        None => {
            reject_symlink_target(path)?;
            if path.exists() {
                fs::remove_file(path)?;
                sync_parent_directory(path)?;
            }
            Ok(())
        }
    }
}
