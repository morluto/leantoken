use std::{fs, io, path::Path};

/// Fingerprint the linked product's source and build inputs in stable path order.
pub fn product_sources(root: &Path) -> io::Result<String> {
    let mut files = vec![
        root.join("Cargo.toml"),
        root.join("Cargo.lock"),
        root.join(".cargo/config.toml"),
        root.join("crates/benchmarks/Cargo.toml"),
    ];
    let mut pending = vec![root.join("src")];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(io::Error::other("source identity does not follow symlinks"));
            }
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            }
            if files.len() + pending.len() > 10_000 {
                return Err(io::Error::other("source inventory exceeds 10000 entries"));
            }
        }
    }
    files.sort();
    let mut bytes = 0u64;
    let mut hash = blake3::Hasher::new();
    for file in files {
        let metadata = fs::symlink_metadata(&file)?;
        if !metadata.is_file() {
            return Err(io::Error::other("source input must be a regular file"));
        }
        bytes = bytes
            .checked_add(metadata.len())
            .ok_or_else(|| io::Error::other("source byte overflow"))?;
        if bytes > 128 * 1024 * 1024 {
            return Err(io::Error::other("source inventory exceeds 128 MiB"));
        }
        let path = file
            .strip_prefix(root)
            .map_err(io::Error::other)?
            .to_string_lossy()
            .replace('\\', "/");
        hash.update(&(path.len() as u64).to_le_bytes());
        hash.update(path.as_bytes());
        let contents = fs::read(file)?;
        hash.update(&(contents.len() as u64).to_le_bytes());
        hash.update(&contents);
    }
    Ok(hash.finalize().to_hex().to_string())
}
