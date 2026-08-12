use std::{env, fs, path::PathBuf};

fn main() {
    let package_root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let repository_root = package_root
        .join("../..")
        .canonicalize()
        .expect("repository root");
    println!(
        "cargo:rustc-env=LEANTOKEN_REPOSITORY_ROOT={}",
        repository_root.display()
    );
    let manifest = fs::read_to_string(repository_root.join("Cargo.toml")).expect("root manifest");
    let version = manifest
        .lines()
        .skip_while(|line| line.trim() != "[package]")
        .skip(1)
        .find_map(|line| line.trim().strip_prefix("version = \"")?.strip_suffix('"'))
        .expect("root package version");
    println!("cargo:rustc-env=LEANTOKEN_PRODUCT_VERSION={version}");
}
