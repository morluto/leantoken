use std::{env, error::Error, fs, path::Path};

const SEARCH_SEMANTICS_IMPLEMENTATION_PATHS: &[&str] = &[
    "src/query_receipt.rs",
    "src/services/search/mod.rs",
    "src/services/search/execution.rs",
    "src/services/search/hits.rs",
    "src/services/search/projection.rs",
    "src/services/search/regex_plan.rs",
    "src/services/search/types.rs",
    "src/services/search/validation.rs",
    "src/storage/read/search.rs",
    "src/storage/query_receipts.rs",
];

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_directory = env::var("CARGO_MANIFEST_DIR")?;
    let manifest_directory = Path::new(&manifest_directory);
    let implementation_digest =
        hash_files(manifest_directory, SEARCH_SEMANTICS_IMPLEMENTATION_PATHS)?;
    let dependency_digest = hash_files(manifest_directory, &["Cargo.lock"])?;
    println!(
        "cargo:rustc-env=LEANTOKEN_SEARCH_SEMANTICS_IMPLEMENTATION_DIGEST={}",
        implementation_digest.to_hex()
    );
    println!(
        "cargo:rustc-env=LEANTOKEN_LOCKED_DEPENDENCIES_DIGEST={}",
        dependency_digest.to_hex()
    );
    Ok(())
}

fn hash_files(root: &Path, paths: &[&str]) -> Result<blake3::Hash, Box<dyn Error>> {
    let mut hasher = blake3::Hasher::new();
    for relative_path in paths {
        let path = root.join(relative_path);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = fs::read(path)?;
        hasher.update(&(relative_path.len() as u64).to_le_bytes());
        hasher.update(relative_path.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(hasher.finalize())
}
