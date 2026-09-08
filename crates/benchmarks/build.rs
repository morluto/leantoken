use std::{env, fs, path::PathBuf};
#[path = "src/build_identity.rs"]
mod build_identity;

fn main() {
    for name in ["TARGET", "PROFILE"] {
        println!(
            "cargo:rustc-env=LEANTOKEN_BUILD_{name}={}",
            env::var(name).expect("build identity")
        );
    }
    let rustc = std::process::Command::new(env::var_os("RUSTC").expect("rustc"))
        .arg("-vV")
        .output()
        .expect("compiler identity");
    assert!(rustc.status.success(), "compiler identity failed");
    println!(
        "cargo:rustc-env=LEANTOKEN_BUILD_RUSTC={}",
        String::from_utf8(rustc.stdout)
            .expect("compiler UTF-8")
            .lines()
            .collect::<Vec<_>>()
            .join("; ")
    );
    let package_root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let repository_root = package_root
        .join("../..")
        .canonicalize()
        .expect("repository root");
    for input in [
        "src",
        "Cargo.toml",
        "Cargo.lock",
        ".cargo/config.toml",
        "crates/benchmarks/Cargo.toml",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            repository_root.join(input).display()
        );
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/build_identity.rs");
    for name in ["HEAD", "refs", "packed-refs"] {
        if let Ok(output) = std::process::Command::new("git")
            .args(["rev-parse", "--git-path", name])
            .current_dir(&repository_root)
            .output()
            && output.status.success()
        {
            let path = repository_root.join(String::from_utf8_lossy(&output.stdout).trim());
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    println!(
        "cargo:rustc-env=LEANTOKEN_BUILD_SOURCE_BLAKE3={}",
        build_identity::product_sources(&repository_root).expect("product source identity")
    );
    let revision = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&repository_root)
        .output()
        .ok()
        .filter(|result| result.status.success())
        .map(|result| String::from_utf8_lossy(&result.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".into());
    println!("cargo:rustc-env=LEANTOKEN_BUILD_REVISION={revision}");
    println!(
        "cargo:rustc-env=LEANTOKEN_BUILD_RUSTFLAGS={}",
        env::var("CARGO_ENCODED_RUSTFLAGS")
            .unwrap_or_default()
            .replace('\u{1f}', " ")
    );
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
