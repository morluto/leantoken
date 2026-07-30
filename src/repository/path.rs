pub fn resolve_existing(root: &Path, requested: &str) -> Result<PathBuf> {
    let relative = validate_relative(requested)?;
    let canonical = root.join(relative).canonicalize()?;
    if !canonical.starts_with(root) {
        return Err(Error::PathOutsideRoot(canonical));
    }
    Ok(canonical)
}

pub fn validate_relative(requested: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(normalize_relative(requested)?))
}

/// Validate and normalize a repository-relative request path.
///
/// Repository keys always use forward slashes, independent of the host
/// platform. This helper therefore recognizes both separator styles before
/// applying the relative-path contract.
pub fn normalize_relative(requested: &str) -> Result<String> {
    if requested.is_empty() || requested.contains('\0') {
        return Err(Error::InvalidInput {
            field: "path",
            reason: "must be a non-empty relative path",
        });
    }
    // `Path` only recognizes prefixes for the host platform. Reject common
    // Windows absolute forms explicitly so a request has the same contract on
    // Linux, macOS, and Windows.
    let bytes = requested.as_bytes();
    let has_windows_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let has_windows_root = requested.starts_with('\\');
    if has_windows_drive || has_windows_root {
        return Err(Error::PathOutsideRoot(PathBuf::from(requested)));
    }
    let normalized = requested.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(Error::PathOutsideRoot(PathBuf::from(requested)));
    }
    let path = Path::new(&normalized);
    if path.is_absolute() {
        return Err(Error::PathOutsideRoot(path.to_path_buf()));
    }
    let mut components = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(Error::PathOutsideRoot(path.to_path_buf())),
            component => components.push(component),
        }
    }
    if components.is_empty() {
        return Ok(".".to_owned());
    }
    Ok(components.join("/"))
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

pub(crate) fn checked_slash_path(path: &Path) -> Result<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(
                value
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::UnsupportedPathEncoding(path.to_path_buf())),
            ),
            _ => None,
        })
        .collect::<Result<Vec<_>>>()
        .map(|components| components.join("/"))
}
use super::*;
