use std::fs;

use leantoken::repository::{
    DiscoveryPolicy, discover_files, discover_files_with_limits,
    discover_files_with_limits_and_policy, discover_files_with_limits_cancellable,
    git_changed_paths, git_diff_paths, resolve_existing, slash_path, validate_relative,
};
use leantoken::{DiscoveryLimits, Error, IndexLimitKind};
use tokio_util::sync::CancellationToken;

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
    fs::create_dir(root.path().join(".git")).expect("git marker");
    fs::write(root.path().join(".git/config"), "internal").expect("git config");
    fs::write(root.path().join(".gitignore"), "ignored.rs\n").expect("gitignore");
    fs::write(root.path().join(".gitattributes"), "*.rs text\n").expect("gitattributes");
    fs::create_dir_all(root.path().join(".github/workflows")).expect("github workflows");
    fs::write(root.path().join(".github/workflows/ci.yml"), "name: ci\n")
        .expect("workflow");
    fs::write(root.path().join("kept.rs"), "fn kept() {}\n").expect("kept");
    fs::write(root.path().join("ignored.rs"), "fn ignored() {}\n").expect("ignored");

    let files = discover_files(root.path(), 1024).expect("walk");
    let paths = files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"kept.rs"));
    assert!(paths.contains(&".gitignore"));
    assert!(paths.contains(&".gitattributes"));
    assert!(paths.contains(&".github/workflows/ci.yml"));
    assert!(!paths.contains(&"ignored.rs"));
    assert!(!paths.contains(&".git/config"));
}

#[test]
fn discover_files_excludes_generated_trees_without_hiding_repository_dotfiles() {
    let root = tempfile::tempdir().expect("tempdir");
    for (path, contents) in [
        ("node_modules/pkg/index.js", "export const generated = true;\n"),
        ("target/debug/generated.rs", "fn generated() {}\n"),
        (".venv/lib/site.py", "generated = True\n"),
        (".tox/py/bin/tool.py", "generated = True\n"),
        (".cache/tool/data.rs", "fn cached() {}\n"),
        (".yarn/cache/pkg.zip", "cache\n"),
        (".github/workflows/ci.yml", "name: ci\n"),
        (".devcontainer/devcontainer.json", "{}\n"),
        (".cargo/config.toml", "[build]\n"),
        (".env.example", "KEY=value\n"),
        ("src/target", "ordinary file\n"),
    ] {
        let absolute = root.path().join(path);
        fs::create_dir_all(absolute.parent().expect("fixture parent")).expect("fixture directory");
        fs::write(absolute, contents).expect("fixture file");
    }
    let default = discover_files_with_limits(root.path(), DiscoveryLimits::default())
        .expect("default discovery");
    let paths = default
        .files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();
    for included in [
        ".github/workflows/ci.yml",
        ".devcontainer/devcontainer.json",
        ".cargo/config.toml",
        ".env.example",
        "src/target",
    ] {
        assert!(paths.contains(&included), "default policy omitted {included}");
    }
    for excluded in [
        "node_modules/pkg/index.js",
        "target/debug/generated.rs",
        ".venv/lib/site.py",
        ".tox/py/bin/tool.py",
        ".cache/tool/data.rs",
        ".yarn/cache/pkg.zip",
    ] {
        assert!(!paths.contains(&excluded), "default policy admitted {excluded}");
    }

    let inclusive = discover_files_with_limits_and_policy(
        root.path(),
        DiscoveryLimits::default(),
        DiscoveryPolicy::new(true),
    )
    .expect("inclusive discovery");
    let inclusive_paths = inclusive
        .files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();
    assert!(inclusive_paths.contains(&"node_modules/pkg/index.js"));
    assert!(inclusive_paths.contains(&"target/debug/generated.rs"));
    assert!(inclusive_paths.contains(&".venv/lib/site.py"));
}

#[test]
fn leantokenignore_has_precedence_over_gitignore_and_applies_when_nested() {
    let root = tempfile::tempdir().expect("tempdir");
    fs::create_dir(root.path().join(".git")).expect("git marker");
    fs::create_dir_all(root.path().join("fixtures/nested")).expect("fixtures");
    fs::write(root.path().join("fixtures/keep.rs"), "fn keep() {}\n").expect("keep");
    fs::write(root.path().join("fixtures/drop.rs"), "fn drop() {}\n").expect("drop");
    fs::write(
        root.path().join("fixtures/nested/drop.rs"),
        "fn nested_drop() {}\n",
    )
    .expect("nested drop");
    fs::write(root.path().join(".gitignore"), "fixtures/\n").expect("gitignore");
    fs::write(
        root.path().join(".leantokenignore"),
        "!fixtures/\n!fixtures/**\nfixtures/drop.rs\n",
    )
    .expect("leantokenignore");
    fs::write(
        root.path().join("fixtures/nested/.leantokenignore"),
        "drop.rs\n",
    )
    .expect("nested leantokenignore");

    let files = discover_files(root.path(), 1024).expect("discovery");
    let paths = files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&".leantokenignore"));
    assert!(paths.contains(&"fixtures/keep.rs"));
    assert!(paths.contains(&"fixtures/nested/.leantokenignore"));
    assert!(!paths.contains(&"fixtures/drop.rs"));
    assert!(!paths.contains(&"fixtures/nested/drop.rs"));
}

#[test]
fn discovery_policy_case_behavior_matches_the_host_platform() {
    let policy = DiscoveryPolicy::default();
    assert!(!policy.includes_path("node_modules/pkg/index.js", false));
    assert!(policy.includes_path("target", false));
    assert_eq!(
        policy.includes_path("NODE_MODULES/pkg/index.js", false),
        !cfg!(windows)
    );
}

#[test]
fn discover_files_excludes_git_pointer_file() {
    let root = tempfile::tempdir().expect("tempdir");
    fs::write(
        root.path().join(".git"),
        "gitdir: /private/worktrees/example\n",
    )
    .expect("git pointer");
    fs::write(root.path().join(".gitignore"), "").expect("gitignore");
    fs::write(root.path().join("kept.rs"), "fn kept() {}\n").expect("kept");

    let files = discover_files(root.path(), 1024).expect("walk");
    let paths = files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"kept.rs"));
    assert!(paths.contains(&".gitignore"));
    assert!(!paths.contains(&".git"));
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
fn discovery_walk_entry_limit_accepts_boundary_and_rejects_limit_plus_one() {
    let root = tempfile::tempdir().expect("tempdir");
    fs::write(root.path().join("a.rs"), "a").expect("a");
    fs::write(root.path().join("b.rs"), "b").expect("b");
    let limits = DiscoveryLimits {
        max_walk_entries: 3,
        ..DiscoveryLimits::default()
    };

    let result = discover_files_with_limits(root.path(), limits).expect("exact boundary");
    assert_eq!(result.stats.walk_entries, 3);
    assert_eq!(result.stats.files, 2);

    let error = discover_files_with_limits(
        root.path(),
        DiscoveryLimits {
            max_walk_entries: 2,
            ..limits
        },
    )
    .expect_err("limit plus one");
    assert!(matches!(
        error,
        Error::IndexLimitExceeded {
            kind: IndexLimitKind::WalkEntries,
            observed: 3,
            limit: 2
        }
    ));
}

#[test]
fn discovery_file_limit_accepts_boundary_and_rejects_limit_plus_one() {
    let root = tempfile::tempdir().expect("tempdir");
    fs::write(root.path().join("a.rs"), "a").expect("a");
    fs::write(root.path().join("b.rs"), "b").expect("b");
    let limits = DiscoveryLimits {
        max_files: 2,
        ..DiscoveryLimits::default()
    };

    assert_eq!(
        discover_files_with_limits(root.path(), limits)
            .expect("exact boundary")
            .stats
            .files,
        2
    );
    let error = discover_files_with_limits(
        root.path(),
        DiscoveryLimits {
            max_files: 1,
            ..limits
        },
    )
    .expect_err("limit plus one");
    assert!(matches!(
        error,
        Error::IndexLimitExceeded {
            kind: IndexLimitKind::Files,
            observed: 2,
            limit: 1
        }
    ));
}

#[test]
fn discovery_source_byte_limit_accepts_boundary_and_rejects_limit_plus_one() {
    let root = tempfile::tempdir().expect("tempdir");
    fs::write(root.path().join("a.rs"), "ab").expect("a");
    fs::write(root.path().join("b.rs"), "cde").expect("b");
    let limits = DiscoveryLimits {
        max_total_source_bytes: 5,
        ..DiscoveryLimits::default()
    };

    assert_eq!(
        discover_files_with_limits(root.path(), limits)
            .expect("exact boundary")
            .stats
            .total_source_bytes,
        5
    );
    let error = discover_files_with_limits(
        root.path(),
        DiscoveryLimits {
            max_total_source_bytes: 4,
            ..limits
        },
    )
    .expect_err("limit plus one");
    assert!(matches!(
        error,
        Error::IndexLimitExceeded {
            kind: IndexLimitKind::TotalSourceBytes,
            observed: 5,
            limit: 4
        }
    ));
}

#[test]
fn discovery_depth_limit_accepts_boundary_and_rejects_deeper_entry() {
    let root = tempfile::tempdir().expect("tempdir");
    fs::create_dir(root.path().join("nested")).expect("nested");
    fs::write(root.path().join("nested/file.rs"), "a").expect("file");
    let limits = DiscoveryLimits {
        max_depth: 2,
        ..DiscoveryLimits::default()
    };

    assert_eq!(
        discover_files_with_limits(root.path(), limits)
            .expect("exact boundary")
            .stats
            .max_depth,
        2
    );
    let error = discover_files_with_limits(
        root.path(),
        DiscoveryLimits {
            max_depth: 1,
            ..limits
        },
    )
    .expect_err("deeper entry");
    assert!(matches!(
        error,
        Error::IndexLimitExceeded {
            kind: IndexLimitKind::Depth,
            observed: 2,
            limit: 1
        }
    ));
}

#[test]
fn oversized_files_still_consume_the_walk_entry_budget() {
    let root = tempfile::tempdir().expect("tempdir");
    for index in 0..3 {
        fs::write(root.path().join(format!("{index}.bin")), "oversized").expect("file");
    }
    let limits = DiscoveryLimits {
        max_walk_entries: 3,
        max_file_bytes: 1,
        ..DiscoveryLimits::default()
    };

    let error = discover_files_with_limits(root.path(), limits).expect_err("walk bound");
    assert!(matches!(
        error,
        Error::IndexLimitExceeded {
            kind: IndexLimitKind::WalkEntries,
            observed: 4,
            limit: 3
        }
    ));
}

#[test]
fn directories_consume_the_walk_entry_budget() {
    let root = tempfile::tempdir().expect("tempdir");
    for index in 0..3 {
        fs::create_dir(root.path().join(format!("dir-{index}"))).expect("directory");
    }
    let limits = DiscoveryLimits {
        max_walk_entries: 3,
        ..DiscoveryLimits::default()
    };

    let error = discover_files_with_limits(root.path(), limits).expect_err("walk bound");
    assert!(matches!(
        error,
        Error::IndexLimitExceeded {
            kind: IndexLimitKind::WalkEntries,
            observed: 4,
            limit: 3
        }
    ));
}

#[test]
fn per_file_limit_admits_the_boundary_and_skips_limit_plus_one() {
    let root = tempfile::tempdir().expect("tempdir");
    fs::write(root.path().join("boundary.rs"), "1234").expect("boundary");
    fs::write(root.path().join("too-large.rs"), "12345").expect("too large");

    let result = discover_files_with_limits(
        root.path(),
        DiscoveryLimits {
            max_file_bytes: 4,
            ..DiscoveryLimits::default()
        },
    )
    .expect("discovery");
    let paths = result
        .files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();

    assert_eq!(paths, ["boundary.rs"]);
    assert_eq!(result.stats.files, 1);
    assert_eq!(result.stats.total_source_bytes, 4);
}

#[test]
fn bounded_discovery_checks_cancellation_before_limits() {
    let root = tempfile::tempdir().expect("tempdir");
    fs::write(root.path().join("a.rs"), "a").expect("a");
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = discover_files_with_limits_cancellable(
        root.path(),
        DiscoveryLimits {
            max_walk_entries: 1,
            ..DiscoveryLimits::default()
        },
        &cancellation,
    )
    .expect_err("cancelled");

    assert!(matches!(error, Error::Cancelled));
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

fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok()
}

fn run_git(root: &std::path::Path, args: &[&str]) {
    std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git command");
}

fn init_git_repo(root: &std::path::Path) {
    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "user.name", "Test"]);
}

#[test]
fn git_changed_paths_is_empty_outside_git_repo() {
    let root = tempfile::tempdir().expect("root");
    let changed = git_changed_paths(root.path(), 64).expect("changed paths");
    assert!(changed.is_empty());
}

#[test]
fn git_changed_paths_detects_modified_and_untracked_files() {
    if !git_available() {
        return;
    }

    let root = tempfile::tempdir().expect("root");
    init_git_repo(root.path());
    fs::write(root.path().join("tracked.rs"), "fn tracked() {}").expect("write");
    run_git(root.path(), &["add", "tracked.rs"]);
    run_git(root.path(), &["commit", "-m", "initial"]);

    fs::write(root.path().join("tracked.rs"), "fn tracked() { }").expect("modify");
    fs::write(root.path().join("new.rs"), "fn new() {}").expect("untracked");
    fs::write(root.path().join("space name.rs"), "fn spaced() {}").expect("untracked space");

    let changed = git_changed_paths(root.path(), 64).expect("changed paths");
    assert!(changed.contains("tracked.rs"));
    assert!(changed.contains("new.rs"));
    assert!(changed.contains("space name.rs"));
    assert_eq!(changed.len(), 3);
}

#[test]
fn git_changed_paths_are_relative_to_a_nested_index_root() {
    if !git_available() {
        return;
    }

    let root = tempfile::tempdir().expect("root");
    let nested = root.path().join("packages/core");
    fs::create_dir_all(&nested).expect("nested root");
    init_git_repo(root.path());
    fs::write(nested.join("tracked.rs"), "fn tracked() {}\n").expect("write");
    run_git(root.path(), &["add", "."]);
    run_git(root.path(), &["commit", "-m", "initial"]);
    fs::write(nested.join("tracked.rs"), "fn tracked() { }\n").expect("modify");

    let changed = git_changed_paths(&nested, 64).expect("changed paths");

    assert_eq!(
        changed,
        std::collections::HashSet::from(["tracked.rs".into()])
    );
}

#[cfg(unix)]
#[test]
fn git_changed_paths_does_not_run_repository_fsmonitor() {
    use std::os::unix::fs::PermissionsExt;

    if !git_available() {
        return;
    }

    let root = tempfile::tempdir().expect("root");
    init_git_repo(root.path());
    fs::write(root.path().join("tracked.rs"), "fn tracked() {}\n").expect("write");
    run_git(root.path(), &["add", "."]);
    run_git(root.path(), &["commit", "-m", "initial"]);

    let marker = root.path().join("fsmonitor-ran");
    let hook = root.path().join("fsmonitor-hook");
    fs::write(
        &hook,
        format!("#!/bin/sh\ntouch \"{}\"\n", marker.display()),
    )
    .expect("hook");
    let mut permissions = fs::metadata(&hook).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&hook, permissions).expect("executable");
    run_git(
        root.path(),
        &[
            "config",
            "core.fsmonitor",
            hook.to_str().expect("hook path"),
        ],
    );

    let _ = git_changed_paths(root.path(), 64).expect("changed paths");

    assert!(!marker.exists(), "repository fsmonitor hook was executed");
}

#[test]
fn git_diff_paths_rejects_empty_base_revision() {
    let root = tempfile::tempdir().expect("root");
    let error = git_diff_paths(root.path(), "", 64).expect_err("empty base rejected");
    assert!(matches!(error, Error::InvalidInput { field, .. } if field == "base revision"));
}

#[test]
fn git_diff_paths_returns_error_for_unresolvable_revision() {
    if !git_available() {
        return;
    }
    let root = tempfile::tempdir().expect("root");
    init_git_repo(root.path());
    let error = git_diff_paths(root.path(), "nonexistent-branch", 64)
        .expect_err("unresolvable revision rejected");
    assert!(
        matches!(error, Error::InvalidInput { field, .. } if field == "base revision"),
        "got {error:?}"
    );
}

#[test]
fn git_diff_paths_detects_committed_changes_relative_to_base() {
    if !git_available() {
        return;
    }
    let root = tempfile::tempdir().expect("root");
    init_git_repo(root.path());
    fs::write(root.path().join("base.rs"), "fn base() {}
").expect("write base");
    run_git(root.path(), &["add", "."]);
    run_git(root.path(), &["commit", "-m", "base commit"]);

    let base_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(root.path())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .expect("resolve base sha");

    fs::write(root.path().join("changed.rs"), "fn changed() {}
").expect("write changed");
    run_git(root.path(), &["add", "."]);
    run_git(root.path(), &["commit", "-m", "changed commit"]);

    let result = git_diff_paths(root.path(), &base_sha, 64).expect("diff paths");
    assert_eq!(result.base_revision, base_sha);
    assert!(!result.head_revision.is_empty());
    assert!(result.changed_paths.contains(&"changed.rs".to_owned()));
    assert!(!result.changed_paths.contains(&"base.rs".to_owned()));
}

#[test]
fn git_diff_paths_includes_working_tree_changes() {
    if !git_available() {
        return;
    }
    let root = tempfile::tempdir().expect("root");
    init_git_repo(root.path());
    fs::write(root.path().join("committed.rs"), "fn committed() {}
").expect("write");
    run_git(root.path(), &["add", "."]);
    run_git(root.path(), &["commit", "-m", "initial"]);

    let base_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(root.path())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .expect("resolve base sha");

    fs::write(root.path().join("uncommitted.rs"), "fn uncommitted() {}
").expect("write");
    let result = git_diff_paths(root.path(), &base_sha, 64).expect("diff paths");
    assert!(result.changed_paths.contains(&"uncommitted.rs".to_owned()));
}

#[test]
fn git_diff_paths_resolves_origin_main_ref_name() {
    if !git_available() {
        return;
    }
    let root = tempfile::tempdir().expect("root");
    init_git_repo(root.path());
    fs::write(root.path().join("base.rs"), "fn base() {}
").expect("write");
    run_git(root.path(), &["add", "."]);
    run_git(root.path(), &["commit", "-m", "base"]);

    let result = git_diff_paths(root.path(), "HEAD", 64).expect("HEAD as base");
    assert!(!result.base_revision.is_empty());
    assert_eq!(result.base_revision, result.head_revision);
    assert!(result.changed_paths.is_empty());
}
