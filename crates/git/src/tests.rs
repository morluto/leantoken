use std::{
    fs,
    io::Cursor,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

use super::*;

#[test]
fn git_status_parser_stops_after_collecting_max_paths() {
    let first = b"M  first.rs\0";
    let mut input = Cursor::new([first.as_slice(), b"M  second.rs\0"].concat());

    let changed = parse_git_status_observation(&mut input, 1, "").changed_paths;

    assert_eq!(changed, HashSet::from(["first.rs".to_string()]));
    assert_eq!(input.position(), first.len() as u64);
}

#[test]
fn diff_name_parser_stops_after_collecting_max_paths() {
    let first = b"first.rs\0";
    let mut input = Cursor::new([first.as_slice(), b"second.rs\0"].concat());

    let changed = parse_diff_names(&mut input, 1, "").expect("valid paths");

    assert_eq!(changed, vec!["first.rs".to_string()]);
    assert_eq!(input.position(), first.len() as u64);
}

#[test]
fn git_status_with_non_utf8_path_marks_the_signal_unavailable() {
    let input = Cursor::new(b"?? \x80.rs\0".to_vec());

    let observation = parse_git_status_observation(input, 10, "");

    assert!(observation.changed_paths.is_empty());
    assert!(!observation.is_available());
    assert!(!observation.has_modified());
    assert!(observation.has_untracked());
}

#[test]
fn diff_name_parser_rejects_non_utf8_paths_without_lossy_aliases() {
    let error =
        parse_diff_names(Cursor::new(b"\x80.rs\0"), 10, "").expect_err("non-UTF-8 path must fail");

    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "git diff path",
            reason: "must be valid UTF-8",
        }
    ));
}

#[test]
fn revision_resolution_preserves_the_callers_field_for_empty_output() {
    let directory = tempfile::tempdir().expect("directory");
    let error = resolve_revision_sha_for_field(
        directory.path(),
        Path::new("true"),
        "HEAD",
        Duration::from_secs(1),
        "head revision",
    )
    .expect_err("successful command without output must fail");

    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "head revision",
            reason: "resolved to an empty SHA",
        }
    ));
}

#[test]
fn diff_hunk_parser_reads_complete_records_beyond_the_old_byte_cap() {
    let mut diff = String::from("+++ b/first.rs\n@@ -1 +1 @@\n");
    diff.push_str(&format!(" {}\n", "x".repeat(8 * 1024 * 1024)));
    diff.push_str("+++ b/second.rs\n@@ -9,2 +10,3 @@\n");

    let ranges = parse_git_diff_hunks(Cursor::new(diff), 10, "").expect("diff hunks");

    assert_eq!(
        ranges,
        vec![
            GitHunkRange {
                path: "first.rs".into(),
                start_line: 1,
                end_line: 1,
            },
            GitHunkRange {
                path: "second.rs".into(),
                start_line: 10,
                end_line: 12,
            },
        ]
    );
}

#[test]
fn diff_hunk_parser_preserves_empty_target_boundaries() {
    let diff = "+++ b/first.rs\n@@ -1 +0,0 @@\n+++ b/later.rs\n@@ -4 +3,0 @@\n";

    let ranges = parse_git_diff_hunks(Cursor::new(diff), 10, "").expect("diff hunks");

    assert_eq!(
        ranges,
        vec![
            GitHunkRange {
                path: "first.rs".into(),
                start_line: 1,
                end_line: 0,
            },
            GitHunkRange {
                path: "later.rs".into(),
                start_line: 4,
                end_line: 3,
            },
        ]
    );
}

#[test]
fn git_changed_paths_kills_a_timed_out_process() {
    let root = tempfile::tempdir().expect("root");
    let program = root.path().join("slow-git");
    fs::write(&program, "#!/bin/sh\nexec sleep 5\n").expect("script");
    let mut permissions = fs::metadata(&program).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&program, permissions).expect("executable");
    let started = Instant::now();

    let observation =
        git_working_tree_status_with(root.path(), 64, &program, Duration::from_millis(50));

    assert!(observation.changed_paths.is_empty());
    assert!(!observation.is_available());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(unix)]
#[test]
fn git_capture_kills_the_producer_when_output_crosses_the_budget() {
    let root = tempfile::tempdir().expect("root");
    let program = root.path().join("large-git");
    let marker = root.path().join("producer-finished");
    fs::write(
        &program,
        format!(
            "#!/bin/sh\nhead -c 1048576 /dev/zero\ntouch '{}'\n",
            marker.display()
        ),
    )
    .expect("script");
    let mut permissions = fs::metadata(&program).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&program, permissions).expect("executable");

    let error = run_git_capture(
        root.path(),
        &program,
        &[],
        GitCaptureOptions {
            timeout: Duration::from_secs(2),
            field: "test",
            timeout_reason: "timed out",
            failure_reason: "failed",
            max_output_bytes: 1_024,
        },
    )
    .expect_err("capture must stop at its byte budget");

    assert!(
        matches!(
            &error,
            Error::RequestLimitExceeded {
                field: "git output bytes",
                requested: 1_025,
                limit: 1_024,
            }
        ),
        "unexpected capture error: {error:?}"
    );
    assert!(
        !marker.exists(),
        "producer ran after its output was rejected"
    );
}

#[cfg(unix)]
#[test]
fn name_only_probe_preserves_best_effort_failure_semantics() {
    let root = tempfile::tempdir().expect("root");
    let program = root.path().join("large-git");
    fs::write(&program, "#!/bin/sh\nhead -c 1048576 /dev/zero\n").expect("script");
    let mut permissions = fs::metadata(&program).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&program, permissions).expect("executable");

    let changed = diff_name_only(
        root.path(),
        &program,
        "base",
        None,
        1,
        Duration::from_secs(2),
        "",
    )
    .expect("name-only probe remains best effort");

    assert!(changed.is_empty());
}

#[cfg(unix)]
#[test]
fn git_capture_releases_an_oversized_reader_after_the_child_exits() {
    let root = tempfile::tempdir().expect("root");
    let program = root.path().join("fast-git");
    fs::write(&program, "#!/bin/sh\nprintf xx\n").expect("script");
    let mut permissions = fs::metadata(&program).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&program, permissions).expect("executable");

    let (result_sender, result_receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = run_git_capture(
            root.path(),
            &program,
            &[],
            GitCaptureOptions {
                timeout: Duration::from_secs(2),
                field: "test",
                timeout_reason: "timed out",
                failure_reason: "failed",
                max_output_bytes: 1,
            },
        );
        let _ = result_sender.send(result);
    });

    let error = result_receiver
        .recv_timeout(Duration::from_secs(3))
        .expect("oversized reader must be released after child exit")
        .expect_err("capture must reject output above its byte budget");
    assert!(matches!(
        error,
        Error::RequestLimitExceeded {
            field: "git output bytes",
            requested: 2,
            limit: 1,
        }
    ));
}

#[cfg(unix)]
#[test]
fn git_capture_terminates_descendants_that_inherit_stdout() {
    let root = tempfile::tempdir().expect("root");
    let program = root.path().join("forking-git");
    fs::write(&program, "#!/bin/sh\nsleep 30 &\nexit 0\n").expect("script");
    let mut permissions = fs::metadata(&program).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&program, permissions).expect("executable");

    let started = Instant::now();
    let output = run_git_capture(
        root.path(),
        &program,
        &[],
        GitCaptureOptions {
            timeout: Duration::from_secs(2),
            field: "test",
            timeout_reason: "timed out",
            failure_reason: "failed",
            max_output_bytes: 1_024,
        },
    )
    .expect("capture");

    assert!(output.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "inherited stdout kept the capture alive"
    );
}
