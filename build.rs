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
    let output = Path::new(&env::var("OUT_DIR")?).join("search_semantics_identity.rs");
    fs::write(
        output,
        format!(
            "pub const SEARCH_SEMANTICS_IMPLEMENTATION_DIGEST: [u8; 32] = {implementation_digest:?};\n\
             pub const LOCKED_DEPENDENCIES_DIGEST: [u8; 32] = {dependency_digest:?};\n"
        ),
    )?;
    Ok(())
}

fn hash_files(root: &Path, paths: &[&str]) -> Result<[u8; 32], Box<dyn Error>> {
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
    Ok(*hasher.finalize().as_bytes())
}
