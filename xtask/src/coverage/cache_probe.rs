use std::{collections::BTreeMap, fs, path::Path};

use super::{clean_workspace, logged};

pub(super) fn run(workspace: &Path) -> Result<(), String> {
    // Build executable fixtures under the workspace target: /tmp can be noexec.
    let scratch = workspace.join("target/coverage-cache-fixtures");
    fs::create_dir_all(&scratch).map_err(|error| error.to_string())?;
    let fixture = tempfile::tempdir_in(&scratch).map_err(|error| error.to_string())?;
    let root = fixture.path().join("workspace");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["coverage", "check-cache-cleanup-worker"])
        .env("LEANTOKEN_COVERAGE_CACHE_FIXTURE", &root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("LLVM_PROFILE_FILE")
        .env_remove("CARGO_LLVM_COV")
        .env_remove("CARGO_LLVM_COV_TARGET_DIR")
        .env_remove("CARGO_LLVM_COV_BUILD_DIR")
        .env_remove("CARGO_BUILD_BUILD_DIR")
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "coverage cache regression failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    println!(
        "coverage cache cleanup: obsolete maps excluded, current coverage retained, dependency artifacts unchanged"
    );
    Ok(())
}

pub(super) fn worker() -> Result<(), String> {
    let root = std::env::var_os("LEANTOKEN_COVERAGE_CACHE_FIXTURE")
        .ok_or("coverage cache worker requires an isolated fixture root")?;
    let root = Path::new(&root);
    let dependency = root.parent().unwrap().join("dependency");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("target/coverage")).unwrap();
    fs::create_dir_all(dependency.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "leantoken_coverage_cache_fixture"
version = "0.1.0"
edition = "2024"
[features]
previous = []
[dependencies]
fixture_dependency = { path = "../dependency" }
[workspace]
"#,
    )
    .unwrap();
    fs::write(
        dependency.join("Cargo.toml"),
        "[package]\nname = \"fixture_dependency\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
    )
    .unwrap();
    fs::write(
        dependency.join("src/lib.rs"),
        "pub fn constant() -> u64 { 4 }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "mod retired;\npub fn old_entry() -> u64 { retired::old_owner() + fixture_dependency::constant() }\n#[test] fn old_test() { assert_eq!(old_entry(), 11); }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/retired.rs"),
        "pub fn old_owner() -> u64 { 7 }\n",
    )
    .unwrap();
    phase(root, "lock", &["cargo", "generate-lockfile", "--offline"]);
    phase(
        root,
        "old-tests",
        &[
            "cargo",
            "llvm-cov",
            "test",
            "--offline",
            "--locked",
            "--features",
            "previous",
            "--no-report",
        ],
    );
    let before = report(root, "before");
    assert!(has_retired_owner(&before));
    assert!(has_executed_function(&before, "old_entry"));
    let dependencies = dependency_objects(root);
    assert!(!dependencies.is_empty(), "fixture must build a dependency");

    fs::write(
        root.join("src/lib.rs"),
        "pub fn current_entry() -> u64 { fixture_dependency::constant() + 9 }\n#[test] fn current_test() { assert_eq!(current_entry(), 13); }\n",
    )
    .unwrap();
    fs::remove_file(root.join("src/retired.rs")).unwrap();
    // Exercise the production cleanup boundary, not a duplicate command.
    clean_workspace(root).expect("clean cached workspace artifacts");
    assert_eq!(
        dependency_objects(root),
        dependencies,
        "workspace cleanup must retain dependency artifacts unchanged"
    );
    phase(
        root,
        "current-tests",
        &[
            "cargo",
            "llvm-cov",
            "test",
            "--offline",
            "--locked",
            "--no-report",
        ],
    );
    let current = report(root, "current");
    assert!(
        !has_retired_owner(&current),
        "obsolete source owner in report"
    );
    assert!(
        !current["data"][0]["functions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|function| { function["name"].as_str().unwrap().contains("old_entry") }),
        "obsolete function in report"
    );
    assert!(has_executed_function(&current, "current_entry"));
    Ok(())
}

fn phase(root: &Path, name: &str, command: &[&str]) {
    let command = command
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect::<Vec<_>>();
    logged(root, &command, name).unwrap_or_else(|error| {
        let output = root.join("target/coverage");
        let stdout = fs::read_to_string(output.join(format!("{name}.stdout.log"))).unwrap();
        let stderr = fs::read_to_string(output.join(format!("{name}.stderr.log"))).unwrap();
        panic!("{error}\nstdout: {stdout}\nstderr: {stderr}");
    });
}

fn report(root: &Path, name: &str) -> serde_json::Value {
    let path = root.join(format!("{name}.json"));
    phase(
        root,
        name,
        &[
            "cargo",
            "llvm-cov",
            "report",
            "--json",
            "--output-path",
            path.to_str().unwrap(),
        ],
    );
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn has_retired_owner(report: &serde_json::Value) -> bool {
    report["data"][0]["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| {
            file["filename"]
                .as_str()
                .unwrap()
                .replace('\\', "/")
                .ends_with("/src/retired.rs")
        })
}

fn has_executed_function(report: &serde_json::Value, name: &str) -> bool {
    report["data"][0]["functions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|function| {
            function["name"].as_str().unwrap().contains(name)
                && function["count"].as_u64().unwrap() > 0
        })
}

fn dependency_objects(root: &Path) -> BTreeMap<String, String> {
    fs::read_dir(root.join("target/llvm-cov-target/debug/deps"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_str().unwrap();
            name.starts_with("libfixture_dependency-") && name.ends_with(".rlib")
        })
        .map(|path| {
            let digest = blake3::hash(&fs::read(&path).unwrap()).to_hex().to_string();
            (path.to_str().unwrap().to_owned(), digest)
        })
        .collect()
}
