pub(crate) fn verify_baseline(
    baseline: &MetaRecord,
    current_generation: i64,
    current_config: &str,
) -> Result<()> {
    let actual = i64_to_u64(current_generation)?;
    if actual != baseline.repository_generation || current_config != baseline.config_hash {
        return Err(Error::StaleReconciliation {
            expected: baseline.repository_generation,
            actual,
        });
    }
    Ok(())
}

pub(crate) fn repository_identity(path: &Path, index_scope_digest: Option<&str>) -> String {
    let mut hasher = blake3::Hasher::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hasher.update(path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in path.as_os_str().encode_wide() {
            hasher.update(&unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    hasher.update(path.to_string_lossy().as_bytes());
    if let Some(scope_digest) = index_scope_digest {
        hasher.update(b"\0leantoken-index-scope-v1\0");
        hasher.update(scope_digest.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

pub(crate) fn u64_to_i64(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| Error::OperationFailure("value exceeds storage integer range".into()))
}

pub(crate) fn usize_to_i64(value: usize) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| Error::OperationFailure("value exceeds storage integer range".into()))
}

pub(crate) fn u128_to_i64(value: u128) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| Error::OperationFailure("value exceeds storage integer range".into()))
}

pub(crate) fn i64_to_u64(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| rusqlite::types::FromSqlError::OutOfRange(value).into())
}

pub(crate) fn i64_to_usize(value: i64) -> rusqlite::Result<usize> {
    usize::try_from(value).map_err(|_| rusqlite::types::FromSqlError::OutOfRange(value).into())
}

pub(crate) fn i64_to_u128(value: i64) -> rusqlite::Result<u128> {
    u128::try_from(value).map_err(|_| rusqlite::types::FromSqlError::OutOfRange(value).into())
}

pub(crate) fn role_to_str(role: ReferenceRole) -> &'static str {
    match role {
        ReferenceRole::Definition => "definition",
        ReferenceRole::Reference => "reference",
    }
}

pub(crate) fn role_from_str(role: &str) -> ReferenceRole {
    match role {
        "definition" => ReferenceRole::Definition,
        _ => ReferenceRole::Reference,
    }
}

use super::*;
