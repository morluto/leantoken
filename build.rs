use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

const SEARCH_SEMANTICS_BUILD_INPUTS: &[&str] = &["Cargo.toml", "build.rs"];

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_directory = env::var("CARGO_MANIFEST_DIR")?;
    let manifest_directory = Path::new(&manifest_directory);
    let implementation_digest = hash_files(
        manifest_directory,
        search_semantics_input_paths(manifest_directory)?,
    )?;
    println!(
        "cargo:rustc-env=LEANTOKEN_SEARCH_SEMANTICS_IMPLEMENTATION_DIGEST={}",
        implementation_digest.to_hex()
    );
    Ok(())
}

fn search_semantics_input_paths(root: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut paths = SEARCH_SEMANTICS_BUILD_INPUTS
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    collect_rust_sources(root, Path::new("src"), &mut paths)?;
    paths.sort();
    Ok(paths)
}

fn collect_rust_sources(
    root: &Path,
    relative_directory: &Path,
    paths: &mut Vec<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(root.join(relative_directory))? {
        let entry = entry?;
        let relative_path = relative_directory.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_rust_sources(root, &relative_path, paths)?;
        } else if file_type.is_file()
            && relative_path
                .extension()
                .is_some_and(|extension| extension == "rs")
        {
            paths.push(relative_path);
        }
    }
    Ok(())
}

fn hash_files(root: &Path, paths: Vec<PathBuf>) -> Result<blake3::Hash, Box<dyn Error>> {
    let mut hasher = blake3::Hasher::new();
    for relative_path in paths {
        let path = root.join(&relative_path);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = fs::read(path)?;
        let relative_path = relative_path.to_string_lossy().replace('\\', "/");
        hasher.update(&(relative_path.len() as u64).to_le_bytes());
        hasher.update(relative_path.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(hasher.finalize())
}
