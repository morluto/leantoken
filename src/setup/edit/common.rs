use super::*;

pub(super) const MAX_SETUP_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const MAX_SETUP_JOURNAL_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub(super) enum EditStatus {
    Configured,
    Updated,
    AlreadyConfigured,
    Removed,
    NotConfigured,
}

impl fmt::Display for EditStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Configured => "configured",
            Self::Updated => "updated",
            Self::AlreadyConfigured => "already configured",
            Self::Removed => "removed",
            Self::NotConfigured => "not configured",
        };
        formatter.write_str(value)
    }
}

impl From<EditStatus> for ClientSetupOutcome {
    fn from(status: EditStatus) -> Self {
        match status {
            EditStatus::Configured => Self::Configured,
            EditStatus::Updated => Self::Updated,
            EditStatus::AlreadyConfigured => Self::AlreadyConfigured,
            EditStatus::Removed => Self::Removed,
            EditStatus::NotConfigured => Self::NotConfigured,
        }
    }
}

#[derive(Debug)]
pub(super) enum ResolvedEdit {
    Configured {
        original: Option<String>,
        updated: String,
    },
    Updated {
        original: Option<String>,
        updated: String,
    },
    AlreadyConfigured {
        original: Option<String>,
    },
    Removed {
        original: String,
        updated: String,
    },
    NotConfigured {
        original: Option<String>,
    },
}

impl ResolvedEdit {
    pub(super) const fn status(&self) -> EditStatus {
        match self {
            Self::Configured { .. } => EditStatus::Configured,
            Self::Updated { .. } => EditStatus::Updated,
            Self::AlreadyConfigured { .. } => EditStatus::AlreadyConfigured,
            Self::Removed { .. } => EditStatus::Removed,
            Self::NotConfigured { .. } => EditStatus::NotConfigured,
        }
    }

    pub(super) fn original(&self) -> Option<&str> {
        match self {
            Self::Configured { original, .. }
            | Self::Updated { original, .. }
            | Self::AlreadyConfigured { original }
            | Self::NotConfigured { original } => original.as_deref(),
            Self::Removed { original, .. } => Some(original),
        }
    }

    pub(super) fn updated(&self) -> Option<&str> {
        match self {
            Self::Configured { updated, .. }
            | Self::Updated { updated, .. }
            | Self::Removed { updated, .. } => Some(updated),
            Self::AlreadyConfigured { .. } | Self::NotConfigured { .. } => None,
        }
    }

    pub(super) const fn action(&self) -> ClientPlanAction {
        match self {
            Self::Configured { original: None, .. } => ClientPlanAction::Create,
            Self::Configured { .. } | Self::Updated { .. } => ClientPlanAction::Update,
            Self::AlreadyConfigured { .. } => ClientPlanAction::AlreadyCurrent,
            Self::Removed { .. } => ClientPlanAction::Remove,
            Self::NotConfigured { .. } => ClientPlanAction::NotConfigured,
        }
    }
}

pub(super) fn read_optional(path: &Path) -> Result<Option<String>> {
    read_optional_with_limit(path, MAX_SETUP_FILE_BYTES)
}

pub(super) fn read_optional_with_limit(path: &Path, max_bytes: u64) -> Result<Option<String>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() > max_bytes {
        return Err(setup_file_limit_error(path, max_bytes));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(setup_file_limit_error(path, max_bytes));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| invalid_config(path, error))
}

fn setup_file_limit_error(path: &Path, max_bytes: u64) -> Error {
    Error::SetupFailure(format!(
        "refusing to read setup file above the {max_bytes}-byte limit: {}",
        path.display()
    ))
}

pub(super) fn validate_setup_content_size(path: &Path, content: &str) -> Result<()> {
    if content.len() as u64 > MAX_SETUP_FILE_BYTES {
        return Err(Error::SetupFailure(format!(
            "refusing to write setup file above the {MAX_SETUP_FILE_BYTES}-byte limit: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn write_if_changed(path: &Path, original: &str, updated: &str) -> Result<()> {
    validate_setup_content_size(path, updated)?;
    if original == updated {
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| {
        Error::SetupFailure(format!("config path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(updated.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    if let Ok(metadata) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary
        .persist(path)
        .map_err(|error| Error::Io(error.error))?;
    sync_parent_directory(path)?;
    Ok(())
}

#[cfg(unix)]
pub(super) fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::SetupFailure(format!("path has no parent: {}", path.display())))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn sync_parent_directory(_path: &Path) -> Result<()> {
    // Windows does not expose the Unix directory-fsync contract through
    // std::fs. File contents are still synced before atomic replacement.
    Ok(())
}

pub(super) fn invalid_config(path: &Path, error: impl fmt::Display) -> Error {
    Error::SetupFailure(format!(
        "refusing to overwrite malformed config {}: {error}",
        path.display()
    ))
}

pub(super) fn toml_positive_integer(item: &Item) -> Option<u64> {
    if let Some(value) = item.as_integer() {
        return u64::try_from(value).ok().filter(|value| *value > 0);
    }
    item.as_float()
        .filter(|value| value.is_finite() && *value >= 1.0 && value.fract() == 0.0)
        .and_then(|value| value.to_string().parse().ok())
}
