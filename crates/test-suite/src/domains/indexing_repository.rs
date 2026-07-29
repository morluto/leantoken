use leantoken::repository::validate_relative;
use leantoken_test_support::Sandbox;
use std::fs;

#[test]
fn rejects_parent_traversal_at_the_repository_boundary() {
    assert!(validate_relative("../secret").is_err());
    assert!(validate_relative("foo/../../secret").is_err());
}

#[test]
fn rejects_absolute_paths_at_the_repository_boundary() {
    assert!(validate_relative("/tmp/secret").is_err());
    assert!(validate_relative("C:/windows/secret").is_err());
    assert!(validate_relative(r"C:\windows\secret").is_err());
}

#[test]
fn rejects_empty_and_nul_paths() {
    assert!(validate_relative("").is_err());
    assert!(validate_relative("foo\0bar").is_err());
}

#[test]
fn accepts_clean_relative_paths() {
    assert!(validate_relative("src/lib.rs").is_ok());
    assert!(validate_relative("a/b/c.rs").is_ok());
}

use leantoken::repository::{
    DiscoveryPolicy, IndexScope, discover_files, discover_files_with_limits,
    discover_files_with_limits_and_policy, discover_files_with_limits_cancellable,
    git_changed_paths, git_diff_hunks, git_diff_paths, normalize_relative, resolve_existing,
    slash_path,
};
use leantoken::{DiscoveryLimits, Error, IndexLimitKind};
use tokio_util::sync::CancellationToken;

#[test]
fn normalize_relative_uses_repository_key_separators_and_collapses_current_directory() {
    assert_eq!(
        normalize_relative(r".\src\lib.rs").expect("normalized path"),
        "src/lib.rs"
    );
}

#[test]
fn discover_files_honors_gitignore() {
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::create_dir(root.repo().join(".git")).expect("git marker");
    fs::write(root.repo().join(".git/config"), "internal").expect("git config");
    fs::write(root.repo().join(".gitignore"), "ignored.rs\n").expect("gitignore");
    fs::write(root.repo().join(".gitattributes"), "*.rs text\n").expect("gitattributes");
    fs::create_dir_all(root.repo().join(".github/workflows")).expect("github workflows");
    fs::write(root.repo().join(".github/workflows/ci.yml"), "name: ci\n").expect("workflow");
    fs::write(root.repo().join("kept.rs"), "fn kept() {}\n").expect("kept");
    fs::write(root.repo().join("ignored.rs"), "fn ignored() {}\n").expect("ignored");

    let files = discover_files(root.repo(), 1024).expect("walk");
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
fn explicit_index_scope_prunes_excluded_trees_before_limits_and_preserves_selected_files() {
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::create_dir_all(root.repo().join("src/generated/deep")).expect("generated");
    fs::create_dir_all(root.repo().join("third_party/dependency/deep")).expect("dependency");
    fs::create_dir_all(root.repo().join("tests")).expect("tests");
    fs::write(root.repo().join("src/lib.rs"), "pub fn selected() {}\n").expect("source");
    fs::write(
        root.repo().join("src/generated/deep/schema.rs"),
        "pub fn generated() {}\n",
    )
    .expect("generated source");
    fs::write(
        root.repo().join("third_party/dependency/deep/lib.rs"),
        "pub fn dependency() {}\n",
    )
    .expect("dependency source");
    fs::write(root.repo().join("tests/smoke.rs"), "fn smoke() {}\n").expect("test");
    let scope = IndexScope::new(
        vec!["src/**".into(), "tests/**".into()],
        vec!["src/generated/**".into()],
    )
    .expect("scope");
    let policy = DiscoveryPolicy::default().with_index_scope(scope);
    let discovery = discover_files_with_limits_and_policy(
        root.repo(),
        DiscoveryLimits {
            max_walk_entries: 8,
            ..DiscoveryLimits::default()
        },
        policy,
    )
    .expect("scoped discovery remains below the traversal limit");
    let paths = discovery
        .files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();

    assert_eq!(paths, ["src/lib.rs", "tests/smoke.rs"]);
    assert!(discovery.stats.walk_entries <= 8);
}

#[test]
fn discover_files_excludes_generated_trees_without_hiding_repository_dotfiles() {
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    for (path, contents) in [
        (
            "node_modules/pkg/index.js",
            "export const generated = true;\n",
        ),
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
        let absolute = root.repo().join(path);
        fs::create_dir_all(absolute.parent().expect("fixture parent")).expect("fixture directory");
        fs::write(absolute, contents).expect("fixture file");
    }
    let default = discover_files_with_limits(root.repo(), DiscoveryLimits::default())
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
        assert!(
            paths.contains(&included),
            "default policy omitted {included}"
        );
    }
    for excluded in [
        "node_modules/pkg/index.js",
        "target/debug/generated.rs",
        ".venv/lib/site.py",
        ".tox/py/bin/tool.py",
        ".cache/tool/data.rs",
        ".yarn/cache/pkg.zip",
    ] {
        assert!(
            !paths.contains(&excluded),
            "default policy admitted {excluded}"
        );
    }

    let inclusive = discover_files_with_limits_and_policy(
        root.repo(),
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
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::create_dir(root.repo().join(".git")).expect("git marker");
    fs::create_dir_all(root.repo().join("fixtures/nested")).expect("fixtures");
    fs::write(root.repo().join("fixtures/keep.rs"), "fn keep() {}\n").expect("keep");
    fs::write(root.repo().join("fixtures/drop.rs"), "fn drop() {}\n").expect("drop");
    fs::write(
        root.repo().join("fixtures/nested/drop.rs"),
        "fn nested_drop() {}\n",
    )
    .expect("nested drop");
    fs::write(root.repo().join(".gitignore"), "fixtures/\n").expect("gitignore");
    fs::write(
        root.repo().join(".leantokenignore"),
        "!fixtures/\n!fixtures/**\nfixtures/drop.rs\n",
    )
    .expect("leantokenignore");
    fs::write(
        root.repo().join("fixtures/nested/.leantokenignore"),
        "drop.rs\n",
    )
    .expect("nested leantokenignore");

    let files = discover_files(root.repo(), 1024).expect("discovery");
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
    assert!(!policy.includes_path(".git/objects/aa/object", false));
    assert!(!DiscoveryPolicy::new(true).includes_path("nested/.git", true));
    assert!(policy.includes_path("target", false));
    assert_eq!(
        policy.includes_path("NODE_MODULES/pkg/index.js", false),
        !cfg!(windows)
    );
}

#[test]
fn discover_files_excludes_git_pointer_file() {
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::write(
        root.repo().join(".git"),
        "gitdir: /private/worktrees/example\n",
    )
    .expect("git pointer");
    fs::write(root.repo().join(".gitignore"), "").expect("gitignore");
    fs::write(root.repo().join("kept.rs"), "fn kept() {}\n").expect("kept");

    let files = discover_files(root.repo(), 1024).expect("walk");
    let paths = files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"kept.rs"));
    assert!(paths.contains(&".gitignore"));
    assert!(!paths.contains(&".git"));
}

#[test]
fn discover_files_excludes_nested_git_metadata() {
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::create_dir_all(root.repo().join("nested/.git/objects")).expect("nested git metadata");
    fs::write(root.repo().join("nested/.git/config"), "[core]\n").expect("git config");
    fs::write(root.repo().join("nested/.git/objects/object"), "metadata").expect("git object");
    fs::write(root.repo().join("nested/source.rs"), "fn source() {}\n").expect("source");

    let files = discover_files(root.repo(), 1024).expect("discovery");
    let paths = files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();

    assert_eq!(paths, vec!["nested/source.rs"]);
}

#[test]
fn discovery_prunes_git_metadata_before_walking_its_contents() {
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::create_dir_all(root.repo().join(".git/objects/aa")).expect("git objects");
    for index in 0..100 {
        fs::write(
            root.repo().join(format!(".git/objects/aa/{index:038x}")),
            "object",
        )
        .expect("git object");
    }
    fs::write(root.repo().join("source.rs"), "fn source() {}\n").expect("source");

    for policy in [DiscoveryPolicy::default(), DiscoveryPolicy::new(true)] {
        let discovery =
            discover_files_with_limits_and_policy(root.repo(), DiscoveryLimits::default(), policy)
                .expect("discovery");

        assert_eq!(
            discovery
                .files
                .iter()
                .map(|file| file.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["source.rs"]
        );
        assert_eq!(
            discovery.stats.walk_entries, 2,
            "the walker should yield only the root and visible source file"
        );
    }
}

#[test]
fn discover_files_skips_oversized_files() {
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::write(root.repo().join("small.rs"), "fn a() {}\n").expect("small");
    fs::write(root.repo().join("big.rs"), "x".repeat(2048)).expect("big");

    let files = discover_files(root.repo(), 1024).expect("walk");
    let paths = files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"small.rs"));
    assert!(!paths.contains(&"big.rs"));
}

#[test]
fn discovery_walk_entry_limit_accepts_boundary_and_rejects_limit_plus_one() {
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::write(root.repo().join("a.rs"), "a").expect("a");
    fs::write(root.repo().join("b.rs"), "b").expect("b");
    let limits = DiscoveryLimits {
        max_walk_entries: 3,
        ..DiscoveryLimits::default()
    };

    let result = discover_files_with_limits(root.repo(), limits).expect("exact boundary");
    assert_eq!(result.stats.walk_entries, 3);
    assert_eq!(result.stats.files, 2);

    let error = discover_files_with_limits(
        root.repo(),
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
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::write(root.repo().join("a.rs"), "a").expect("a");
    fs::write(root.repo().join("b.rs"), "b").expect("b");
    let limits = DiscoveryLimits {
        max_files: 2,
        ..DiscoveryLimits::default()
    };

    assert_eq!(
        discover_files_with_limits(root.repo(), limits)
            .expect("exact boundary")
            .stats
            .files,
        2
    );
    let error = discover_files_with_limits(
        root.repo(),
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
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::write(root.repo().join("a.rs"), "ab").expect("a");
    fs::write(root.repo().join("b.rs"), "cde").expect("b");
    let limits = DiscoveryLimits {
        max_total_source_bytes: 5,
        ..DiscoveryLimits::default()
    };

    assert_eq!(
        discover_files_with_limits(root.repo(), limits)
            .expect("exact boundary")
            .stats
            .total_source_bytes,
        5
    );
    let error = discover_files_with_limits(
        root.repo(),
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
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::create_dir(root.repo().join("nested")).expect("nested");
    fs::write(root.repo().join("nested/file.rs"), "a").expect("file");
    let limits = DiscoveryLimits {
        max_depth: 2,
        ..DiscoveryLimits::default()
    };

    assert_eq!(
        discover_files_with_limits(root.repo(), limits)
            .expect("exact boundary")
            .stats
            .max_depth,
        2
    );
    let error = discover_files_with_limits(
        root.repo(),
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
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    for index in 0..3 {
        fs::write(root.repo().join(format!("{index}.bin")), "oversized").expect("file");
    }
    let limits = DiscoveryLimits {
        max_walk_entries: 3,
        max_file_bytes: 1,
        ..DiscoveryLimits::default()
    };

    let error = discover_files_with_limits(root.repo(), limits).expect_err("walk bound");
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
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    for index in 0..3 {
        fs::create_dir(root.repo().join(format!("dir-{index}"))).expect("directory");
    }
    let limits = DiscoveryLimits {
        max_walk_entries: 3,
        ..DiscoveryLimits::default()
    };

    let error = discover_files_with_limits(root.repo(), limits).expect_err("walk bound");
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
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::write(root.repo().join("boundary.rs"), "1234").expect("boundary");
    fs::write(root.repo().join("too-large.rs"), "12345").expect("too large");

    let result = discover_files_with_limits(
        root.repo(),
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
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::write(root.repo().join("a.rs"), "a").expect("a");
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = discover_files_with_limits_cancellable(
        root.repo(),
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

    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    let outside = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::write(outside.repo().join("secret"), "secret").expect("secret");
    symlink(outside.repo().join("secret"), root.repo().join("link")).expect("symlink");

    let canonical_root = root.repo().canonicalize().expect("canonical root");
    assert!(resolve_existing(&canonical_root, "link").is_err());
}

#[test]
fn resolve_existing_accepts_contained_file() {
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    fs::write(root.repo().join("file.rs"), "fn a() {}").expect("file");

    let canonical_root = root.repo().canonicalize().expect("canonical root");
    let resolved = resolve_existing(&canonical_root, "file.rs").expect("resolve");
    assert!(resolved.starts_with(&canonical_root));
    assert!(resolved.exists());
}

fn require_git() {
    let output = std::process::Command::new("git")
        .arg("--version")
        .output()
        .expect("git is required to run git-dependent integration tests");
    assert!(
        output.status.success(),
        "git is required to run git-dependent integration tests: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    let changed = git_changed_paths(root.repo(), 64).expect("changed paths");
    assert!(changed.is_empty());
}

#[test]
fn git_changed_paths_detects_modified_and_untracked_files() {
    require_git();

    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    init_git_repo(root.repo());
    fs::write(root.repo().join("tracked.rs"), "fn tracked() {}").expect("write");
    run_git(root.repo(), &["add", "tracked.rs"]);
    run_git(root.repo(), &["commit", "-m", "initial"]);

    fs::write(root.repo().join("tracked.rs"), "fn tracked() { }").expect("modify");
    fs::write(root.repo().join("new.rs"), "fn new() {}").expect("untracked");
    fs::write(root.repo().join("space name.rs"), "fn spaced() {}").expect("untracked space");

    let changed = git_changed_paths(root.repo(), 64).expect("changed paths");
    assert!(changed.contains("tracked.rs"));
    assert!(changed.contains("new.rs"));
    assert!(changed.contains("space name.rs"));
    assert_eq!(changed.len(), 3);
}

#[test]
fn git_changed_paths_are_relative_to_a_nested_index_root() {
    require_git();

    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    let nested = root.repo().join("packages/core");
    fs::create_dir_all(&nested).expect("nested root");
    init_git_repo(root.repo());
    fs::write(nested.join("tracked.rs"), "fn tracked() {}\n").expect("write");
    run_git(root.repo(), &["add", "."]);
    run_git(root.repo(), &["commit", "-m", "initial"]);
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

    require_git();

    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    init_git_repo(root.repo());
    fs::write(root.repo().join("tracked.rs"), "fn tracked() {}\n").expect("write");
    run_git(root.repo(), &["add", "."]);
    run_git(root.repo(), &["commit", "-m", "initial"]);

    let marker = root.repo().join("fsmonitor-ran");
    let hook = root.repo().join("fsmonitor-hook");
    fs::write(
        &hook,
        format!("#!/bin/sh\ntouch \"{}\"\n", marker.display()),
    )
    .expect("hook");
    let mut permissions = fs::metadata(&hook).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&hook, permissions).expect("executable");
    run_git(
        root.repo(),
        &[
            "config",
            "core.fsmonitor",
            hook.to_str().expect("hook path"),
        ],
    );

    let _ = git_changed_paths(root.repo(), 64).expect("changed paths");

    assert!(!marker.exists(), "repository fsmonitor hook was executed");
}

#[test]
fn git_diff_paths_rejects_empty_base_revision() {
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    let error = git_diff_paths(root.repo(), "", 64).expect_err("empty base rejected");
    assert!(matches!(error, Error::InvalidInput { field, .. } if field == "base revision"));
}

#[test]
fn git_diff_paths_returns_error_for_unresolvable_revision() {
    require_git();
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    init_git_repo(root.repo());
    let error = git_diff_paths(root.repo(), "nonexistent-branch", 64)
        .expect_err("unresolvable revision rejected");
    assert!(
        matches!(error, Error::InvalidInput { field, .. } if field == "base revision"),
        "got {error:?}"
    );
}

#[test]
fn git_diff_paths_detects_committed_changes_relative_to_base() {
    require_git();
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    init_git_repo(root.repo());
    fs::write(
        root.repo().join("base.rs"),
        "fn base() {}
",
    )
    .expect("write base");
    run_git(root.repo(), &["add", "."]);
    run_git(root.repo(), &["commit", "-m", "base commit"]);

    let base_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(root.repo())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .expect("resolve base sha");

    fs::write(
        root.repo().join("changed.rs"),
        "fn changed() {}
",
    )
    .expect("write changed");
    run_git(root.repo(), &["add", "."]);
    run_git(root.repo(), &["commit", "-m", "changed commit"]);

    let result = git_diff_paths(root.repo(), &base_sha, 64).expect("diff paths");
    assert_eq!(result.base_revision, base_sha);
    assert!(!result.head_revision.is_empty());
    assert!(result.changed_paths.contains(&"changed.rs".to_owned()));
    assert!(!result.changed_paths.contains(&"base.rs".to_owned()));
}

#[test]
fn git_diff_paths_includes_working_tree_changes() {
    require_git();
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    init_git_repo(root.repo());
    fs::write(
        root.repo().join("committed.rs"),
        "fn committed() {}
",
    )
    .expect("write");
    run_git(root.repo(), &["add", "."]);
    run_git(root.repo(), &["commit", "-m", "initial"]);

    let base_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(root.repo())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .expect("resolve base sha");

    fs::write(
        root.repo().join("uncommitted.rs"),
        "fn uncommitted() {}
",
    )
    .expect("write");
    run_git(root.repo(), &["add", "uncommitted.rs"]);
    let result = git_diff_paths(root.repo(), &base_sha, 64).expect("diff paths");
    assert!(result.changed_paths.contains(&"uncommitted.rs".to_owned()));
}

#[test]
fn git_diff_hunks_reports_target_line_ranges() {
    require_git();
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    init_git_repo(root.repo());
    fs::write(
        root.repo().join("changed.rs"),
        "fn changed() {\n    one();\n}\n",
    )
    .expect("write");
    run_git(root.repo(), &["add", "."]);
    run_git(root.repo(), &["commit", "-m", "initial"]);

    fs::write(
        root.repo().join("changed.rs"),
        "fn changed() {\n    one();\n    two();\n}\n",
    )
    .expect("write");

    let hunks = git_diff_hunks(root.repo(), "HEAD", 64).expect("diff hunks");
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].path, "changed.rs");
    assert_eq!((hunks[0].start_line, hunks[0].end_line), (3, 3));
}

#[test]
fn git_diff_hunks_reports_first_line_deletion_as_empty_target_range() {
    require_git();
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    init_git_repo(root.repo());
    fs::write(root.repo().join("changed.rs"), "removed\n").expect("write");
    run_git(root.repo(), &["add", "."]);
    run_git(root.repo(), &["commit", "-m", "initial"]);

    fs::write(root.repo().join("changed.rs"), "").expect("delete first line");

    let hunks = git_diff_hunks(root.repo(), "HEAD", 64).expect("diff hunks");
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].path, "changed.rs");
    assert_eq!((hunks[0].start_line, hunks[0].end_line), (1, 0));
}

#[test]
fn git_diff_paths_resolves_origin_main_ref_name() {
    require_git();
    let root = Sandbox::new(module_path!(), "repository_case").expect("sandbox");
    init_git_repo(root.repo());
    fs::write(
        root.repo().join("base.rs"),
        "fn base() {}
",
    )
    .expect("write");
    run_git(root.repo(), &["add", "."]);
    run_git(root.repo(), &["commit", "-m", "base"]);

    let result = git_diff_paths(root.repo(), "HEAD", 64).expect("HEAD as base");
    assert!(!result.base_revision.is_empty());
    assert_eq!(result.base_revision, result.head_revision);
    assert!(result.changed_paths.is_empty());
}

use std::sync::Arc;

use leantoken::Config;
use leantoken::indexer::Indexer;
use leantoken::storage::Storage;

#[test]
fn indexer_initial_reconcile_indexes_files_and_advances_generation() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::write(root.repo().join("a.rs"), "fn first() {}\n").expect("write a");
    std::fs::write(root.repo().join("b.txt"), "searchable text\n").expect("write b");

    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    for suffix in [".lease.lock", ".init.lock", ".leader.lock", ".index.lock"] {
        std::fs::write(
            root.repo().join(format!("index.sqlite{suffix}")),
            "configured lock",
        )
        .expect("configured sidecar");
    }
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

    let response = indexer.reconcile(false).expect("first reconcile");
    assert_eq!(response.files_indexed, 2);
    assert_eq!(response.repository_generation, 1);
    assert_eq!(response.files_unchanged, 0);
    assert_eq!(response.files_removed, 0);

    let hits = storage.search_word("first", 10).expect("search");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].content.contains("first"));
}

#[test]
fn full_reconcile_excludes_only_recognized_zero_byte_stale_sidecars() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let database = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let stale = root.repo().join("old-cache");
    std::fs::create_dir(&stale).expect("stale cache directory");
    for suffix in [".lease.lock", ".init.lock", ".leader.lock", ".index.lock"] {
        std::fs::write(stale.join(format!("index.sqlite{suffix}")), [])
            .expect("zero-byte stale sidecar");
    }
    let nonzero = root.repo().join("user/index.sqlite.lease.lock");
    std::fs::create_dir_all(nonzero.parent().expect("user directory")).expect("user directory");
    std::fs::write(&nonzero, "user-owned lock content").expect("non-zero same-name file");
    let arbitrary = root.repo().join("project.lock");
    std::fs::write(&arbitrary, "arbitrary lock content").expect("arbitrary lock");
    let config = Arc::new(
        Config::discover(root.repo(), Some(database.root().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

    let first = indexer.reconcile(false).expect("initial reconcile");

    assert_eq!(first.files_indexed, 2);
    assert!(
        storage
            .find_file("project.lock")
            .expect("arbitrary lookup")
            .is_some()
    );
    assert!(
        storage
            .find_file("user/index.sqlite.lease.lock")
            .expect("non-zero lookup")
            .is_some()
    );
    for suffix in [".lease.lock", ".init.lock", ".leader.lock", ".index.lock"] {
        assert!(
            storage
                .find_file(&format!("old-cache/index.sqlite{suffix}"))
                .expect("stale lookup")
                .is_none()
        );
    }

    std::fs::write(&nonzero, []).expect("turn indexed file into a recognized sidecar");
    let second = indexer.reconcile(false).expect("full sidecar cleanup");
    assert_eq!(second.files_removed, 1);
    assert!(
        storage
            .find_file("user/index.sqlite.lease.lock")
            .expect("cleaned lookup")
            .is_none()
    );
    assert!(
        storage
            .find_file("project.lock")
            .expect("arbitrary lookup")
            .is_some()
    );
}

#[test]
fn watcher_targeted_reconcile_adds_user_locks_and_removes_recognized_sidecars() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let database = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::write(root.repo().join("lib.rs"), "fn owner() {}\n").expect("source");
    let config = Arc::new(
        Config::discover(root.repo(), Some(database.root().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    let reported = "old/index.sqlite.index.lock";
    let sidecar = root.repo().join(reported);
    std::fs::create_dir_all(sidecar.parent().expect("old directory")).expect("old directory");
    std::fs::write(&sidecar, "user data").expect("non-zero watcher add");
    let added = indexer
        .reconcile_paths(&[reported.into()])
        .expect("watcher add");
    assert_eq!(added.files_indexed, 1);
    assert!(storage.find_file(reported).expect("added lookup").is_some());

    std::fs::write(&sidecar, []).expect("coordination sidecar transition");
    let removed = indexer
        .reconcile_paths(&[reported.into()])
        .expect("watcher sidecar transition");
    assert_eq!(removed.files_removed, 1);
    assert!(
        storage
            .find_file(reported)
            .expect("removed lookup")
            .is_none()
    );

    let zero_add = "other/index.sqlite.init.lock";
    let zero_path = root.repo().join(zero_add);
    std::fs::create_dir_all(zero_path.parent().expect("other directory")).expect("other directory");
    std::fs::write(&zero_path, []).expect("zero-byte watcher add");
    let ignored = indexer
        .reconcile_paths(&[zero_add.into()])
        .expect("ignore watcher sidecar");
    assert_eq!(ignored.files_indexed, 0);
    assert!(
        storage
            .find_file(zero_add)
            .expect("ignored lookup")
            .is_none()
    );

    let arbitrary = "other/build.lock";
    std::fs::write(root.repo().join(arbitrary), "owner lock").expect("arbitrary watcher add");
    indexer
        .reconcile_paths(&[arbitrary.into()])
        .expect("index arbitrary watcher file");
    assert!(
        storage
            .find_file(arbitrary)
            .expect("arbitrary lookup")
            .is_some()
    );
    std::fs::remove_file(root.repo().join(arbitrary)).expect("delete arbitrary watcher file");
    let deleted = indexer
        .reconcile_paths(&[arbitrary.into()])
        .expect("watcher delete");
    assert_eq!(deleted.files_removed, 1);
    assert!(
        storage
            .find_file(arbitrary)
            .expect("deleted lookup")
            .is_none()
    );
}

#[test]
fn full_reconcile_limit_error_preserves_the_committed_generation() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let database = root.repo().join("index.sqlite");
    std::fs::write(root.repo().join("a.rs"), "fn old() {}\n").expect("a");
    let first_config =
        Arc::new(Config::discover(root.repo(), Some(database.clone())).expect("config"));
    let storage = Storage::open(&database).expect("storage");
    Indexer::new(first_config, storage.clone())
        .expect("indexer")
        .reconcile(false)
        .expect("initial reconcile");
    std::fs::write(root.repo().join("b.rs"), "fn new() {}\n").expect("b");

    let mut limited = Config::discover(root.repo(), Some(database)).expect("limited config");
    limited.max_files = 1;
    let error = Indexer::new(Arc::new(limited), storage.clone())
        .expect("limited indexer")
        .reconcile(false)
        .expect_err("file limit");

    assert!(matches!(
        error,
        Error::IndexLimitExceeded {
            kind: leantoken::IndexLimitKind::Files,
            observed: 2,
            limit: 1
        }
    ));
    assert_eq!(storage.meta().expect("meta").repository_generation, 1);
    assert!(storage.find_file("a.rs").expect("a").is_some());
    assert!(storage.find_file("b.rs").expect("b").is_none());
}

#[test]
fn targeted_reconcile_enforces_aggregate_bytes_before_publication() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let database = root.repo().join("index.sqlite");
    std::fs::write(root.repo().join("a.rs"), "a").expect("a");
    std::fs::write(root.repo().join("b.rs"), "b").expect("b");
    let mut config = Config::discover(root.repo(), Some(database)).expect("config");
    config.max_total_source_bytes = 2;
    let config = Arc::new(config);
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::write(root.repo().join("a.rs"), "aa").expect("grow a");
    let error = indexer
        .reconcile_paths(&["a.rs".into()])
        .expect_err("aggregate limit");

    assert!(matches!(
        error,
        Error::IndexLimitExceeded {
            kind: leantoken::IndexLimitKind::TotalSourceBytes,
            observed: 3,
            limit: 2
        }
    ));
    assert_eq!(storage.meta().expect("meta").repository_generation, 1);
    assert_eq!(
        storage
            .find_file("a.rs")
            .expect("a")
            .expect("indexed a")
            .size_bytes,
        1
    );
}

#[test]
fn full_and_targeted_reconcile_exclude_metadata_oversized_file_from_files_seen() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let databases = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let path = root.repo().join("oversized.rs");
    std::fs::write(&path, "fn admitted() {}\n").expect("initial source");
    let mut full_config = Config::discover(root.repo(), Some(databases.root().join("full.sqlite")))
        .expect("full config");
    full_config.max_file_bytes = 64;
    let full_storage = Storage::open(&full_config.database_path).expect("full storage");
    let full_indexer =
        Indexer::new(Arc::new(full_config), full_storage.clone()).expect("full indexer");
    full_indexer.reconcile(false).expect("initial full index");
    let mut targeted_config =
        Config::discover(root.repo(), Some(databases.root().join("targeted.sqlite")))
            .expect("targeted config");
    targeted_config.max_file_bytes = 64;
    let targeted_storage = Storage::open(&targeted_config.database_path).expect("targeted storage");
    let targeted_indexer = Indexer::new(Arc::new(targeted_config), targeted_storage.clone())
        .expect("targeted indexer");
    targeted_indexer
        .reconcile(false)
        .expect("initial targeted index");

    std::fs::write(&path, vec![b'x'; 65]).expect("grow beyond admission limit");
    let full = full_indexer
        .reconcile_report(false)
        .expect("full reconcile");
    let targeted = targeted_indexer
        .reconcile_paths_report(&["oversized.rs".into()])
        .expect("targeted reconcile");

    for response in [&full, &targeted] {
        assert_eq!(response.files_seen, 0);
        assert_eq!(response.files_removed, 1);
        assert_eq!(response.files_skipped, 0);
        assert_eq!(
            response
                .skip_reasons
                .as_ref()
                .expect("current skip reasons")
                .total(),
            0
        );
    }
    assert!(
        full_storage
            .find_file("oversized.rs")
            .expect("full lookup")
            .is_none()
    );
    assert!(
        targeted_storage
            .find_file("oversized.rs")
            .expect("targeted lookup")
            .is_none()
    );
}

#[test]
fn visibility_reconcile_excludes_discovery_oversized_file_from_files_seen() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::create_dir(root.repo().join(".git")).expect("git marker");
    let oversized = root.repo().join("oversized.rs");
    std::fs::write(&oversized, "fn admitted() {}\n").expect("initial source");
    std::fs::write(root.repo().join(".gitignore"), "").expect("initial ignore");
    let mut config =
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config");
    config.max_file_bytes = 64;
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(Arc::new(config), storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::write(&oversized, vec![b'x'; 65]).expect("grow beyond discovery limit");
    std::fs::write(root.repo().join(".gitignore"), "# visibility refresh\n")
        .expect("change ignore");
    let response = indexer
        .reconcile_paths_report(&[".gitignore".into()])
        .expect("visibility reconcile");

    assert_eq!(response.files_seen, 1);
    assert_eq!(response.files_indexed, 1);
    assert_eq!(response.files_removed, 1);
    assert_eq!(response.files_skipped, 0);
    assert_eq!(
        response
            .skip_reasons
            .as_ref()
            .expect("current skip reasons")
            .total(),
        0
    );
    assert!(storage.find_file("oversized.rs").expect("lookup").is_none());
}

#[test]
fn visibility_reconcile_matches_full_files_seen_for_deleted_file() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let databases = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::create_dir(root.repo().join(".git")).expect("git marker");
    let removed = root.repo().join("removed.rs");
    std::fs::write(&removed, "fn removed() {}\n").expect("initial source");
    std::fs::write(root.repo().join(".gitignore"), "").expect("initial ignore");

    let full_config = Arc::new(
        Config::discover(root.repo(), Some(databases.root().join("full.sqlite")))
            .expect("full config"),
    );
    let full_storage = Storage::open(&full_config.database_path).expect("full storage");
    let full_indexer = Indexer::new(full_config, full_storage.clone()).expect("full indexer");
    full_indexer.reconcile(false).expect("initial full index");

    let visibility_config = Arc::new(
        Config::discover(
            root.repo(),
            Some(databases.root().join("visibility.sqlite")),
        )
        .expect("visibility config"),
    );
    let visibility_storage =
        Storage::open(&visibility_config.database_path).expect("visibility storage");
    let visibility_indexer =
        Indexer::new(visibility_config, visibility_storage.clone()).expect("visibility indexer");
    visibility_indexer
        .reconcile(false)
        .expect("initial visibility index");

    std::fs::remove_file(&removed).expect("remove source");
    std::fs::write(root.repo().join(".gitignore"), "# visibility refresh\n")
        .expect("change ignore");
    let full = full_indexer.reconcile(false).expect("full reconcile");
    let visibility = visibility_indexer
        .reconcile_paths(&[".gitignore".into()])
        .expect("visibility reconcile");

    assert_eq!(visibility.files_seen, full.files_seen);
    assert_eq!(visibility.files_seen, 1);
    assert_eq!(visibility.files_indexed, full.files_indexed);
    assert_eq!(visibility.files_removed, full.files_removed);
    assert_eq!(visibility.files_removed, 1);
    assert!(
        visibility_storage
            .find_file("removed.rs")
            .expect("visibility lookup")
            .is_none()
    );
}

#[test]
fn targeted_reconcile_existing_directory_does_not_count_unchanged_descendants() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::create_dir(root.repo().join("src")).expect("source directory");
    std::fs::write(root.repo().join("src/stable.rs"), "fn stable() {}\n").expect("source fixture");
    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    let response = indexer
        .reconcile_paths_report(&["src".into()])
        .expect("directory reconcile");

    assert_eq!(response.files_seen, 0);
    assert_eq!(response.files_indexed, 0);
    assert_eq!(response.files_removed, 0);
    assert!(
        storage
            .find_file("src/stable.rs")
            .expect("lookup")
            .is_some()
    );
}

#[test]
fn targeted_reconcile_existing_directory_excludes_oversized_descendants_from_files_seen() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::create_dir(root.repo().join("src")).expect("source directory");
    let source = root.repo().join("src/oversized.rs");
    std::fs::write(&source, "fn admitted() {}\n").expect("source fixture");
    let mut config =
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config");
    config.max_file_bytes = 64;
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(Arc::new(config), storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::write(&source, vec![b'x'; 65]).expect("grow beyond discovery limit");
    let response = indexer
        .reconcile_paths_report(&["src".into()])
        .expect("directory reconcile");

    assert_eq!(response.files_seen, 0);
    assert_eq!(response.files_removed, 1);
    assert_eq!(response.files_skipped, 0);
    assert_eq!(
        response
            .skip_reasons
            .as_ref()
            .expect("current skip reasons")
            .total(),
        0
    );
    assert!(
        storage
            .find_file("src/oversized.rs")
            .expect("lookup")
            .is_none()
    );
}

#[test]
fn changing_discovery_limits_invalidates_the_index_configuration_hash() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let database = root.repo().join("index.sqlite");
    std::fs::write(root.repo().join("a.rs"), "fn stable() {}\n").expect("a");
    let first_config =
        Arc::new(Config::discover(root.repo(), Some(database.clone())).expect("config"));
    let storage = Storage::open(&database).expect("storage");
    Indexer::new(first_config, storage.clone())
        .expect("indexer")
        .reconcile(false)
        .expect("initial reconcile");

    let mut changed = Config::discover(root.repo(), Some(database)).expect("changed config");
    changed.max_files -= 1;
    let response = Indexer::new(Arc::new(changed), storage.clone())
        .expect("changed indexer")
        .reconcile(false)
        .expect("configuration rebuild");

    assert_eq!(response.repository_generation, 2);
    assert_eq!(response.files_indexed, 1);
    assert_eq!(storage.meta().expect("meta").repository_generation, 2);
}

#[test]
fn indexer_rejects_invalid_chunk_configuration_at_construction() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let mut config =
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config");
    config.chunk_lines = 0;
    let storage = Storage::open(&config.database_path).expect("storage");

    let error = Indexer::new(Arc::new(config), storage).expect_err("invalid chunk configuration");

    assert!(matches!(error, Error::InvalidConfiguration(_)));
}

#[test]
fn indexer_rejects_zero_discovery_limits_at_construction() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let mut config =
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config");
    config.max_walk_entries = 0;
    let storage = Storage::open(&config.database_path).expect("storage");

    let error = Indexer::new(Arc::new(config), storage).expect_err("invalid discovery limits");

    assert!(matches!(error, Error::InvalidConfiguration(_)));
}

#[test]
fn indexer_reopen_leaves_unchanged_files_and_generation() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::write(root.repo().join("a.rs"), "fn stable() {}\n").expect("write a");

    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config.clone(), storage.clone()).expect("indexer");

    let first = indexer.reconcile(false).expect("first reconcile");
    assert_eq!(first.repository_generation, 1);

    let second = indexer.reconcile(false).expect("second reconcile");
    assert_eq!(second.files_unchanged, 1);
    assert_eq!(second.files_indexed, 0);
    assert_eq!(second.repository_generation, 1);

    let meta = storage.meta().expect("meta");
    assert_eq!(meta.repository_generation, 1);
}

#[test]
fn indexer_change_updates_generation_and_search_index() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::write(root.repo().join("a.rs"), "fn old() {}\n").expect("write a");

    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

    let first = indexer.reconcile(false).expect("first reconcile");
    assert_eq!(first.repository_generation, 1);

    std::fs::write(root.repo().join("a.rs"), "fn new_name() {}\n").expect("change a");

    let second = indexer.reconcile(false).expect("second reconcile");
    assert_eq!(second.files_indexed, 1);
    assert_eq!(second.files_unchanged, 0);
    assert_eq!(second.repository_generation, 2);

    let old_hits = storage.search_word("old", 10).expect("search old");
    assert_eq!(old_hits.len(), 0);

    let new_hits = storage.search_word("new_name", 10).expect("search new");
    assert_eq!(new_hits.len(), 1);
}

#[test]
fn targeted_reconcile_updates_only_reported_existing_file() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::write(root.repo().join("a.rs"), "fn old() {}\n").expect("write a");
    std::fs::write(root.repo().join("b.rs"), "fn stable() {}\n").expect("write b");
    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");
    let stable_generation = storage
        .find_file("b.rs")
        .expect("find stable")
        .expect("stable file")
        .generation;

    std::fs::write(root.repo().join("a.rs"), "fn replacement() {}\n").expect("modify a");
    let response = indexer
        .reconcile_paths(&["a.rs".into()])
        .expect("targeted reconcile");

    assert_eq!(response.files_seen, 1);
    assert_eq!(response.files_indexed, 1);
    assert_eq!(response.files_removed, 0);
    assert!(
        storage
            .search_word("old", 10)
            .expect("old search")
            .is_empty()
    );
    assert_eq!(
        storage
            .search_word("replacement", 10)
            .expect("replacement search")
            .len(),
        1
    );
    assert_eq!(
        storage
            .find_file("b.rs")
            .expect("find stable")
            .expect("stable file")
            .generation,
        stable_generation
    );
}

#[test]
fn targeted_reconcile_hashes_reported_files_even_when_metadata_is_unchanged() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let path = root.repo().join("a.rs");
    std::fs::write(&path, "fn old() {}\n").expect("write old");
    let original_modified = std::fs::metadata(&path)
        .expect("metadata")
        .modified()
        .expect("modified");
    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::write(&path, "fn neo() {}\n").expect("same-size replacement");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_times(std::fs::FileTimes::new().set_modified(original_modified))
        .expect("restore mtime");
    let response = indexer
        .reconcile_paths(&["a.rs".into()])
        .expect("targeted reconcile");

    assert_eq!(response.files_indexed, 1);
    assert!(storage.search_word("old", 10).expect("old").is_empty());
    assert_eq!(storage.search_word("neo", 10).expect("new").len(), 1);
}

#[test]
fn targeted_reconcile_deletes_existing_file() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::write(root.repo().join("gone.rs"), "fn gone() {}\n").expect("write");
    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::remove_file(root.repo().join("gone.rs")).expect("remove");
    let response = indexer
        .reconcile_paths(&["gone.rs".into()])
        .expect("targeted reconcile");

    assert_eq!(response.files_seen, 1);
    assert_eq!(response.files_removed, 1);
    assert!(storage.find_file("gone.rs").expect("find").is_none());
}

#[test]
fn targeted_reconcile_clears_imports_resolved_to_deleted_file() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::write(root.repo().join("target.rs"), "pub fn item() {}\n").expect("target");
    std::fs::write(
        root.repo().join("consumer.rs"),
        "use target::item;\nfn consumer() { item(); }\n",
    )
    .expect("consumer");
    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");
    let consumer = storage
        .find_file("consumer.rs")
        .expect("find consumer")
        .expect("consumer");
    assert_eq!(
        storage
            .get_imports_for_file(consumer.id, 10)
            .expect("imports")[0]
            .resolved_path
            .as_deref(),
        Some("target.rs")
    );

    std::fs::remove_file(root.repo().join("target.rs")).expect("remove target");
    indexer
        .reconcile_paths(&["target.rs".into()])
        .expect("targeted delete");

    assert_eq!(
        storage
            .get_imports_for_file(consumer.id, 10)
            .expect("imports")[0]
            .resolved_path,
        None
    );
}

#[test]
fn targeted_reconcile_applies_deleted_directory_delta() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::create_dir(root.repo().join("removed")).expect("directory");
    std::fs::write(root.repo().join("removed/a.rs"), "fn gone_a() {}\n").expect("a");
    std::fs::write(root.repo().join("removed/b.rs"), "fn gone_b() {}\n").expect("b");
    std::fs::write(root.repo().join("keep.rs"), "fn keep() {}\n").expect("keep");
    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::remove_dir_all(root.repo().join("removed")).expect("remove directory");
    let response = indexer
        .reconcile_paths(&["removed".into()])
        .expect("directory fallback");

    assert_eq!(response.files_removed, 2);
    assert!(storage.find_file("removed/a.rs").expect("find a").is_none());
    assert!(storage.find_file("removed/b.rs").expect("find b").is_none());
    assert!(storage.find_file("keep.rs").expect("find keep").is_some());
}

#[test]
fn targeted_directory_rename_preserves_content_rows_and_refreshes_import_paths() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::create_dir_all(root.repo().join("src/moved")).expect("directory");
    std::fs::write(
        root.repo().join("src/moved/mod.rs"),
        "use self::child::item;\npub fn call() { item(); }\n",
    )
    .expect("module");
    std::fs::write(root.repo().join("src/moved/child.rs"), "pub fn item() {}\n").expect("child");
    std::fs::write(
        root.repo().join("src/consumer.rs"),
        "use moved::child::item;\npub fn consume() { item(); }\n",
    )
    .expect("consumer");
    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    let module = storage
        .find_file("src/moved/mod.rs")
        .expect("module lookup")
        .expect("module");
    let module_chunk = storage
        .get_chunks_for_file(module.id, 10)
        .expect("module chunks")[0]
        .id;
    let module_symbol = storage
        .get_symbols_for_file(module.id, 10)
        .expect("module symbols")
        .into_iter()
        .find(|symbol| symbol.name == "call")
        .expect("call symbol")
        .id;
    let consumer = storage
        .find_file("src/consumer.rs")
        .expect("consumer lookup")
        .expect("consumer");
    let consumer_chunk = storage
        .get_chunks_for_file(consumer.id, 10)
        .expect("consumer chunks")[0]
        .id;

    std::fs::rename(
        root.repo().join("src/moved"),
        root.repo().join("src/renamed"),
    )
    .expect("rename directory");
    let response = indexer
        .reconcile_paths(&["src/moved".into(), "src/renamed".into()])
        .expect("targeted rename");

    assert_eq!(response.files_removed, 2);
    assert_eq!(response.files_indexed, 3);
    let relocated = storage
        .find_file("src/renamed/mod.rs")
        .expect("relocated lookup")
        .expect("relocated module");
    assert_eq!(relocated.id, module.id);
    assert_eq!(
        storage
            .get_chunks_for_file(relocated.id, 10)
            .expect("relocated chunks")[0]
            .id,
        module_chunk
    );
    assert_eq!(
        storage
            .get_symbols_for_file(relocated.id, 10)
            .expect("relocated symbols")
            .into_iter()
            .find(|symbol| symbol.name == "call")
            .expect("relocated call symbol")
            .id,
        module_symbol
    );
    assert_eq!(
        storage.search_word("call", 10).expect("relocated search")[0].path,
        "src/renamed/mod.rs"
    );
    assert_eq!(
        storage
            .get_imports_for_file(relocated.id, 10)
            .expect("relocated imports")[0]
            .resolved_path
            .as_deref(),
        Some("src/renamed/child.rs")
    );
    let refreshed_consumer = storage
        .find_file("src/consumer.rs")
        .expect("refreshed consumer lookup")
        .expect("refreshed consumer");
    assert_eq!(refreshed_consumer.id, consumer.id);
    assert_eq!(
        storage
            .get_chunks_for_file(refreshed_consumer.id, 10)
            .expect("refreshed consumer chunks")[0]
            .id,
        consumer_chunk
    );
    assert_eq!(
        storage
            .get_imports_for_file(refreshed_consumer.id, 10)
            .expect("refreshed consumer imports")[0]
            .resolved_path,
        None
    );
}

#[test]
fn targeted_rename_with_ambiguous_duplicate_content_uses_replacement_path() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::create_dir(root.repo().join("old")).expect("old directory");
    for name in ["first.rs", "second.rs"] {
        std::fs::write(
            root.repo().join("old").join(name),
            "pub fn duplicate() {}\n",
        )
        .expect("fixture");
    }
    std::fs::write(root.repo().join("zz_keep.rs"), "pub fn keep() {}\n").expect("keep fixture");
    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");
    let old_ids = ["old/first.rs", "old/second.rs"]
        .into_iter()
        .map(|path| {
            storage
                .find_file(path)
                .expect("old lookup")
                .expect("old file")
                .id
        })
        .collect::<Vec<_>>();
    let keep_id = storage
        .find_file("zz_keep.rs")
        .expect("keep lookup")
        .expect("keep file")
        .id;
    assert!(old_ids.iter().all(|id| *id < keep_id));

    std::fs::rename(root.repo().join("old"), root.repo().join("new")).expect("rename directory");
    let response = indexer
        .reconcile_paths(&["old".into(), "new".into()])
        .expect("targeted rename");

    assert_eq!(response.files_indexed, 2);
    assert_eq!(response.files_removed, 2);
    for path in ["new/first.rs", "new/second.rs"] {
        let new_id = storage
            .find_file(path)
            .expect("new lookup")
            .expect("new file")
            .id;
        assert!(!old_ids.contains(&new_id));
        assert!(new_id > keep_id);
    }
}

#[test]
fn targeted_reconcile_applies_new_file_and_ignore_deltas() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::create_dir(root.repo().join(".git")).expect("git marker");
    std::fs::write(root.repo().join("keep.rs"), "fn keep() {}\n").expect("write keep");
    std::fs::write(root.repo().join("hide.rs"), "fn hide() {}\n").expect("write hide");
    std::fs::write(root.repo().join(".gitignore"), "").expect("write ignore");
    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::write(root.repo().join("new.rs"), "fn new_file() {}\n").expect("new file");
    let added = indexer
        .reconcile_paths(&["new.rs".into()])
        .expect("new path delta");
    assert_eq!(added.files_seen, 1);
    assert!(storage.find_file("new.rs").expect("find new").is_some());

    std::fs::write(root.repo().join(".gitignore"), "hide.rs\n").expect("change ignore");
    let ignored = indexer
        .reconcile_paths_report(&[".gitignore".into()])
        .expect("ignore delta");
    assert_eq!(ignored.files_seen, 1);
    assert_eq!(ignored.files_indexed, 1);
    assert_eq!(ignored.files_removed, 1);
    assert_eq!(ignored.files_skipped, 0);
    assert_eq!(
        ignored
            .skip_reasons
            .as_ref()
            .expect("current skip reasons")
            .total(),
        0
    );
    assert!(storage.find_file("hide.rs").expect("find hidden").is_none());
}

#[test]
fn targeted_reconcile_applies_leantokenignore_add_change_and_removal() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::write(root.repo().join("first.rs"), "fn first() {}\n").expect("first");
    std::fs::write(root.repo().join("second.rs"), "fn second() {}\n").expect("second");
    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::write(root.repo().join(".leantokenignore"), "first.rs\n").expect("add ignore");
    indexer
        .reconcile_paths(&[".leantokenignore".into()])
        .expect("apply added ignore");
    assert!(
        storage
            .find_file("first.rs")
            .expect("first lookup")
            .is_none()
    );
    assert!(
        storage
            .find_file("second.rs")
            .expect("second lookup")
            .is_some()
    );

    std::fs::write(root.repo().join(".leantokenignore"), "second.rs\n").expect("change ignore");
    indexer
        .reconcile_paths(&[".leantokenignore".into()])
        .expect("apply changed ignore");
    assert!(
        storage
            .find_file("first.rs")
            .expect("first lookup")
            .is_some()
    );
    assert!(
        storage
            .find_file("second.rs")
            .expect("second lookup")
            .is_none()
    );

    std::fs::remove_file(root.repo().join(".leantokenignore")).expect("remove ignore");
    let removed = indexer
        .reconcile_paths(&[".leantokenignore".into()])
        .expect("apply removed ignore");
    assert_eq!(removed.files_seen, 2);
    assert_eq!(removed.files_indexed, 1);
    assert_eq!(removed.files_removed, 1);
    assert_eq!(removed.files_skipped, 0);
    assert!(
        storage
            .find_file("first.rs")
            .expect("first lookup")
            .is_some()
    );
    assert!(
        storage
            .find_file("second.rs")
            .expect("second lookup")
            .is_some()
    );
}

#[test]
fn changing_generated_policy_invalidates_the_index_configuration_hash() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let database = root.repo().join("index.sqlite");
    std::fs::create_dir(root.repo().join("target")).expect("target");
    std::fs::write(
        root.repo().join("target/generated.rs"),
        "fn generated() {}\n",
    )
    .expect("generated");
    let first_config =
        Arc::new(Config::discover(root.repo(), Some(database.clone())).expect("config"));
    let storage = Storage::open(&database).expect("storage");
    Indexer::new(first_config, storage.clone())
        .expect("indexer")
        .reconcile(false)
        .expect("initial reconcile");
    assert!(
        storage
            .find_file("target/generated.rs")
            .expect("lookup")
            .is_none()
    );

    let mut changed = Config::discover(root.repo(), Some(database)).expect("changed config");
    changed.include_generated = true;
    let response = Indexer::new(Arc::new(changed), storage.clone())
        .expect("changed indexer")
        .reconcile(false)
        .expect("configuration rebuild");

    assert_eq!(response.repository_generation, 2);
    assert!(
        storage
            .find_file("target/generated.rs")
            .expect("lookup")
            .is_some()
    );
}

#[test]
fn changing_index_scope_forces_a_complete_membership_rebuild() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let database = root.repo().join("index.sqlite");
    std::fs::create_dir(root.repo().join("src")).expect("src");
    std::fs::create_dir(root.repo().join("third_party")).expect("third party");
    std::fs::write(root.repo().join("src/lib.rs"), "fn selected() {}\n").expect("source");
    std::fs::write(
        root.repo().join("third_party/lib.rs"),
        "fn dependency() {}\n",
    )
    .expect("dependency");
    let storage = Storage::open(&database).expect("storage");
    let source_scope =
        leantoken::IndexScope::new(vec!["src/**".into()], Vec::new()).expect("source scope");
    let first_config = Arc::new(
        Config::discover_scoped(root.repo(), Some(database.clone()), source_scope)
            .expect("scoped config"),
    );
    let first = Indexer::new(first_config, storage.clone()).expect("scoped indexer");
    first.reconcile(false).expect("scoped reconcile");
    assert!(
        storage
            .find_file("src/lib.rs")
            .expect("source lookup")
            .is_some()
    );
    assert!(
        storage
            .find_file("third_party/lib.rs")
            .expect("dependency lookup")
            .is_none()
    );

    let full_config = Arc::new(Config::discover(root.repo(), Some(database)).expect("full config"));
    let full = Indexer::new(full_config, storage.clone()).expect("full indexer");
    let rebuilt = full.reconcile(false).expect("scope rebuild");

    assert_eq!(rebuilt.repository_generation, 2);
    assert!(
        storage
            .find_file("third_party/lib.rs")
            .expect("dependency lookup")
            .is_some()
    );
}

#[test]
fn preparation_batch_size_does_not_change_the_logical_index() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    for index in 0..6 {
        std::fs::write(
            root.repo().join(format!("file_{index}.rs")),
            format!("pub fn item_{index}() {{ item_{}(); }}\n", (index + 1) % 6),
        )
        .expect("fixture");
    }
    let databases = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let mut small =
        Config::discover(root.repo(), Some(databases.root().join("small.sqlite"))).expect("small");
    small.max_prepare_batch_files = 1;
    let small_storage = Storage::open(&small.database_path).expect("small storage");
    Indexer::new(Arc::new(small), small_storage.clone())
        .expect("small indexer")
        .reconcile(false)
        .expect("small index");

    let mut large =
        Config::discover(root.repo(), Some(databases.root().join("large.sqlite"))).expect("large");
    large.max_prepare_batch_files = 64;
    let large_storage = Storage::open(&large.database_path).expect("large storage");
    Indexer::new(Arc::new(large), large_storage.clone())
        .expect("large indexer")
        .reconcile(false)
        .expect("large index");

    let project = |storage: &Storage| {
        storage
            .list_files(100, None)
            .expect("files")
            .into_iter()
            .map(|file| (file.path, file.content_hash, file.size_bytes))
            .collect::<Vec<_>>()
    };
    assert_eq!(project(&small_storage), project(&large_storage));
    assert_eq!(
        small_storage.counts().expect("small counts").files,
        large_storage.counts().expect("large counts").files
    );
    assert_eq!(
        small_storage
            .search_word("item_3", 100)
            .expect("small search")
            .len(),
        large_storage
            .search_word("item_3", 100)
            .expect("large search")
            .len()
    );
}

#[test]
fn profiled_reconcile_reports_bounded_batch_high_water_and_phases() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let mut total_bytes = 0u64;
    for index in 0..3 {
        let source = format!("fn item_{index}() {{}}\n");
        total_bytes += u64::try_from(source.len()).expect("source length");
        std::fs::write(root.repo().join(format!("file_{index}.rs")), source).expect("fixture");
    }
    let database = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let mut config =
        Config::discover(root.repo(), Some(database.root().join("index.sqlite"))).expect("config");
    config.max_prepare_batch_files = 2;
    let storage = Storage::open(&config.database_path).expect("storage");
    let profiled = Indexer::new(Arc::new(config), storage)
        .expect("indexer")
        .reconcile_profiled(false)
        .expect("profiled reconcile");

    assert_eq!(profiled.response.files_indexed, 3);
    assert_eq!(profiled.diagnostics.discovered_files, 3);
    assert_eq!(profiled.diagnostics.discovered_source_bytes, total_bytes);
    assert_eq!(profiled.diagnostics.preparation_batches, 2);
    assert_eq!(profiled.diagnostics.max_batch_files, 2);
    assert!(profiled.diagnostics.max_batch_source_bytes <= total_bytes);
    assert_eq!(
        profiled.diagnostics.preparation_detail.files_profiled,
        profiled.response.files_indexed
    );
    assert_eq!(
        profiled
            .diagnostics
            .preparation_by_language
            .get("rust")
            .map(|detail| detail.files_profiled),
        Some(3)
    );
    assert_eq!(
        profiled
            .diagnostics
            .preparation_by_language
            .values()
            .map(|detail| detail.files_profiled)
            .sum::<usize>(),
        profiled.diagnostics.preparation_detail.files_profiled
    );
    assert!(profiled.diagnostics.total_ms >= profiled.diagnostics.discovery_ms);
    assert!(profiled.diagnostics.publication_ms >= profiled.diagnostics.preparation_ms);
    let publication = &profiled.diagnostics.publication_detail;
    assert!(publication.post_commit_diagnostics_complete);
    assert!(publication.database_bytes > 0);
    assert!(publication.fts_storage.chunk_word_bytes > 0);
    assert!(publication.fts_storage.chunk_trigram_bytes > 0);
    assert!(publication.fts_storage.symbol_bytes > 0);
    assert!(publication.fts_storage.reference_bytes > 0);
    assert!(
        publication.database_bytes
            >= publication
                .fts_storage
                .chunk_word_bytes
                .saturating_add(publication.fts_storage.chunk_trigram_bytes)
                .saturating_add(publication.fts_storage.symbol_bytes)
                .saturating_add(publication.fts_storage.reference_bytes)
    );
}

#[test]
fn new_file_delta_resolves_existing_importers() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::write(
        root.repo().join("consumer.rs"),
        "use target::item;\nfn consumer() { item(); }\n",
    )
    .expect("consumer");
    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");
    let consumer = storage
        .find_file("consumer.rs")
        .expect("find consumer")
        .expect("consumer");
    let consumer_id = consumer.id;
    let consumer_chunk = storage
        .get_chunks_for_file(consumer.id, 10)
        .expect("consumer chunks")[0]
        .id;
    assert_eq!(
        storage
            .get_imports_for_file(consumer.id, 10)
            .expect("imports")[0]
            .resolved_path,
        None
    );

    std::fs::write(root.repo().join("target.rs"), "pub fn item() {}\n").expect("target");
    let response = indexer
        .reconcile_paths(&["target.rs".into()])
        .expect("new target delta");
    assert_eq!(response.files_indexed, 2);

    let consumer = storage
        .find_file("consumer.rs")
        .expect("find consumer")
        .expect("consumer after rebuild");
    assert_eq!(consumer.id, consumer_id);
    assert_eq!(
        storage
            .get_chunks_for_file(consumer.id, 10)
            .expect("consumer chunks")[0]
            .id,
        consumer_chunk
    );
    assert_eq!(
        storage
            .get_imports_for_file(consumer.id, 10)
            .expect("imports")[0]
            .resolved_path
            .as_deref(),
        Some("target.rs")
    );
}

#[test]
fn indexer_delete_removes_file_and_advances_generation() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::write(root.repo().join("a.rs"), "fn gone() {}\n").expect("write a");
    std::fs::write(root.repo().join("b.rs"), "fn kept() {}\n").expect("write b");

    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

    let first = indexer.reconcile(false).expect("first reconcile");
    assert_eq!(first.repository_generation, 1);
    assert_eq!(first.files_indexed, 2);

    std::fs::remove_file(root.repo().join("a.rs")).expect("remove a");

    let second = indexer.reconcile(false).expect("second reconcile");
    assert_eq!(second.files_removed, 1);
    assert_eq!(second.files_unchanged, 1);
    assert_eq!(second.repository_generation, 2);

    assert!(storage.find_file("a.rs").expect("find").is_none());
    let gone_hits = storage.search_word("gone", 10).expect("search gone");
    assert_eq!(gone_hits.len(), 0);
    let kept_hits = storage.search_word("kept", 10).expect("search kept");
    assert_eq!(kept_hits.len(), 1);
}

#[test]
fn indexer_rebuild_resets_index_and_advances_generation() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    std::fs::write(root.repo().join("a.rs"), "fn only() {}\n").expect("write a");

    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

    let first = indexer.reconcile(false).expect("first reconcile");
    assert_eq!(first.repository_generation, 1);

    let rebuild = indexer.reconcile(true).expect("rebuild");
    assert_eq!(rebuild.files_indexed, 1);
    assert_eq!(rebuild.repository_generation, 2);

    let hits = storage.search_word("only", 10).expect("search");
    assert_eq!(hits.len(), 1);
}

#[test]
fn indexer_respects_chunk_lines_and_bytes() {
    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let content: String = (0..100).map(|i| format!("line {}\n", i)).collect();
    std::fs::write(root.repo().join("big.rs"), &content).expect("write big");

    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

    let response = indexer.reconcile(false).expect("reconcile");
    assert_eq!(response.files_indexed, 1);

    let file = storage.find_file("big.rs").expect("find").expect("exists");
    let chunks = storage.get_chunks_for_file(file.id, 100).expect("chunks");
    assert!(
        chunks.len() > 1,
        "large file should be split into multiple chunks"
    );
    for chunk in &chunks {
        assert!(chunk.end_line - chunk.start_line < 80);
        assert!(chunk.content.len() <= 32 * 1024);
    }
}

#[test]
fn full_reconcile_reindexes_when_content_changes_but_size_and_mtime_match() {
    use std::fs::{File, FileTimes};

    let root = Sandbox::new(module_path!(), "indexer_case").expect("sandbox");
    let path = root.repo().join("twin.rs");
    // Same-length payloads so size_bytes matches after overwrite.
    let original = "fn alpha_v1() {}\n";
    let updated = "fn beta__v2() {}\n";
    assert_eq!(original.len(), updated.len(), "fixture sizes must match");
    std::fs::write(&path, original).expect("write original");

    let config = Arc::new(
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

    let first = indexer.reconcile(false).expect("first reconcile");
    assert_eq!(first.files_indexed, 1);
    assert_eq!(first.repository_generation, 1);
    assert!(
        !storage
            .search_word("alpha_v1", 10)
            .expect("search")
            .is_empty()
    );

    let original_meta = std::fs::metadata(&path).expect("metadata");
    let original_mtime = original_meta.modified().expect("mtime before");
    std::fs::write(&path, updated).expect("overwrite same-size content");
    // Portable mtime restore for Windows/macOS/Linux CI (no external touch -r).
    let file = File::options()
        .write(true)
        .open(&path)
        .expect("open for set_times");
    file.set_times(FileTimes::new().set_modified(original_mtime))
        .expect("restore mtime");
    drop(file);

    let after_meta = std::fs::metadata(&path).expect("metadata after");
    assert_eq!(after_meta.len(), original_meta.len());
    assert_eq!(after_meta.modified().expect("mtime after"), original_mtime);

    let second = indexer.reconcile(false).expect("second reconcile");
    assert_eq!(
        second.files_indexed, 1,
        "content-hash must detect same-size mtime-preserved rewrite"
    );
    assert_eq!(second.repository_generation, 2);
    assert!(storage.search_word("alpha_v1", 10).expect("old").is_empty());
    assert!(!storage.search_word("beta__v2", 10).expect("new").is_empty());
}
