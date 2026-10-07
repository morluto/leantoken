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
