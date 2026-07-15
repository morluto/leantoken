use std::{
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

use ignore::WalkBuilder;

use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub absolute_path: PathBuf,
    pub relative_path: String,
    pub size_bytes: u64,
    pub modified_ns: Option<u128>,
}

pub fn discover_files(root: &Path, max_file_bytes: u64) -> Result<Vec<DiscoveredFile>> {
    let mut files = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .follow_links(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(%error, "repository walk entry skipped");
                continue;
            }
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::warn!(path = %entry.path().display(), %error, "file metadata skipped");
                continue;
            }
        };
        if metadata.len() > max_file_bytes {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| Error::PathOutsideRoot(entry.path().to_path_buf()))?;
        let relative_path = slash_path(relative);
        if relative_path.is_empty() || relative_path.starts_with(".git/") {
            continue;
        }
        let modified_ns = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        files.push(DiscoveredFile {
            absolute_path: entry.into_path(),
            relative_path,
            size_bytes: metadata.len(),
            modified_ns,
        });
    }
    files.sort_unstable_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

pub fn resolve_existing(root: &Path, requested: &str) -> Result<PathBuf> {
    let relative = validate_relative(requested)?;
    let canonical = root.join(relative).canonicalize()?;
    if !canonical.starts_with(root) {
        return Err(Error::PathOutsideRoot(canonical));
    }
    Ok(canonical)
}

pub fn validate_relative(requested: &str) -> Result<PathBuf> {
    if requested.is_empty() || requested.contains('\0') {
        return Err(Error::InvalidRequest(
            "path must be a non-empty relative path".into(),
        ));
    }
    // `Path` only recognizes prefixes for the host platform. Reject common
    // Windows absolute forms explicitly so a request has the same contract on
    // Linux, macOS, and Windows.
    let bytes = requested.as_bytes();
    let has_windows_drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    let has_windows_root = requested.starts_with('\\');
    if has_windows_drive || has_windows_root {
        return Err(Error::PathOutsideRoot(PathBuf::from(requested)));
    }
    let path = Path::new(requested);
    if path.is_absolute() {
        return Err(Error::PathOutsideRoot(path.to_path_buf()));
    }
    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(Error::PathOutsideRoot(path.to_path_buf()));
        }
    }
    Ok(path.to_path_buf())
}

pub fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}
