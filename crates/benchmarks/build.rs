use std::{env, path::PathBuf};

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
}
