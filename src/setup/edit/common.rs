#[derive(Debug, Clone, Copy)]
enum EditStatus {
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

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_if_changed(path: &Path, original: &str, updated: &str) -> Result<()> {
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
fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::SetupFailure(format!("path has no parent: {}", path.display())))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<()> {
    // Windows does not expose the Unix directory-fsync contract through
    // std::fs. File contents are still synced before atomic replacement.
    Ok(())
}

fn invalid_config(path: &Path, error: impl fmt::Display) -> Error {
    Error::SetupFailure(format!(
        "refusing to overwrite malformed config {}: {error}",
        path.display()
    ))
}
