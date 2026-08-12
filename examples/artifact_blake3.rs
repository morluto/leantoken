//! Print BLAKE3 identities for frozen experiment artifacts.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;

#[derive(Debug, Parser)]
#[command(about = "Hash frozen experiment artifacts with BLAKE3")]
struct Args {
    /// Artifact path to hash (repeatable).
    #[arg(required = true)]
    artifacts: Vec<PathBuf>,
}

fn main() -> Result<(), Box<dyn Error>> {
    for artifact in Args::parse().artifacts {
        println!("{}  {}", hash_file(&artifact)?, artifact.display());
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(blake3::hash(&fs::read(path)?).to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_file_matches_in_memory_blake3() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let artifact = directory.path().join("artifact.bin");
        let content = b"frozen experiment artifact\0with binary data";
        std::fs::write(&artifact, content).expect("write artifact");

        assert_eq!(
            hash_file(&artifact).expect("artifact hash"),
            blake3::hash(content).to_hex().to_string()
        );
    }
}
