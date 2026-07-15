use std::fs;

use leantoken::repository::{discover_files, resolve_existing, slash_path, validate_relative};

#[test]
fn validate_relative_rejects_parent_traversal() {
    assert!(validate_relative("../secret").is_err());
    assert!(validate_relative("foo/../../secret").is_err());
    assert!(validate_relative("foo/../bar").is_err());
}

#[test]
fn validate_relative_rejects_absolute_paths() {
    if cfg!(unix) {
        assert!(validate_relative("/tmp/secret").is_err());
    }
    assert!(validate_relative("C:/windows/secret").is_err());
    assert!(validate_relative(r"C:\windows\secret").is_err());
    assert!(validate_relative(r"\windows\secret").is_err());
}

#[test]
fn validate_relative_rejects_empty_and_nul() {
    assert!(validate_relative("").is_err());
    assert!(validate_relative("foo\0bar").is_err());
}

#[test]
fn validate_relative_accepts_clean_relative_paths() {
    assert!(validate_relative("src/lib.rs").is_ok());
    assert!(validate_relative("a/b/c.rs").is_ok());
}

#[test]
fn discover_files_honors_gitignore() {
    let root = tempfile::tempdir().expect("tempdir");
    fs::write(root.path().join(".gitignore"), "ignored.rs\n").expect("gitignore");
    fs::write(root.path().join("kept.rs"), "fn kept() {}\n").expect("kept");
    fs::write(root.path().join("ignored.rs"), "fn ignored() {}\n").expect("ignored");

    let files = discover_files(root.path(), 1024).expect("walk");
    let paths = files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"kept.rs"));
    assert!(!paths.contains(&"ignored.rs"));
}

#[test]
fn discover_files_skips_oversized_files() {
    let root = tempfile::tempdir().expect("tempdir");
    fs::write(root.path().join("small.rs"), "fn a() {}\n").expect("small");
    fs::write(root.path().join("big.rs"), "x".repeat(2048)).expect("big");

    let files = discover_files(root.path(), 1024).expect("walk");
    let paths = files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"small.rs"));
    assert!(!paths.contains(&"big.rs"));
}

#[test]
fn slash_path_normalizes_to_forward_slashes() {
    let input = std::path::Path::new("foo/bar/baz.rs");
    assert_eq!(slash_path(input), "foo/bar/baz.rs");
}

#[cfg(unix)]
#[test]
fn resolve_existing_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    fs::write(outside.path().join("secret"), "secret").expect("secret");
    symlink(outside.path().join("secret"), root.path().join("link")).expect("symlink");

    let canonical_root = root.path().canonicalize().expect("canonical root");
    assert!(resolve_existing(&canonical_root, "link").is_err());
}

#[test]
fn resolve_existing_accepts_contained_file() {
    let root = tempfile::tempdir().expect("root");
    fs::write(root.path().join("file.rs"), "fn a() {}").expect("file");

    let canonical_root = root.path().canonicalize().expect("canonical root");
    let resolved = resolve_existing(&canonical_root, "file.rs").expect("resolve");
    assert!(resolved.starts_with(&canonical_root));
    assert!(resolved.exists());
}
