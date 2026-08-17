use super::support::{
    Duration, Instant, McpProcess, assert_runtime_version, database_state, run, wait_until,
    write_rust_fixture_set,
};

const FAILOVER_LIVENESS_TIMEOUT: Duration = Duration::from_secs(60);
// Index readiness is a liveness contract, not a ten-second latency contract;
// the process lane runs several subprocess tests concurrently on Windows.
const INDEX_READY_TIMEOUT: Duration = Duration::from_secs(30);
const PROCESS_FAILURE_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn mcp_initialize_precedes_storage_open() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn answer() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    let blocker = rusqlite::Connection::open(&database).expect("open blocking connection");
    blocker
        .execute_batch(
            "CREATE TABLE startup_blocker(value INTEGER); \
             BEGIN IMMEDIATE; \
             INSERT INTO startup_blocker(value) VALUES (1);",
        )
        .expect("hold database write lock");

    let mut process = McpProcess::spawn(root.path(), &database);
    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "leantoken-test", "version": "1" }
        }
    }));
    let response = process.response(Duration::from_secs(5));
    assert_eq!(response["id"], 1);
    assert!(response.get("result").is_some(), "{response}");

    blocker.execute_batch("ROLLBACK").expect("release database");
    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    wait_until(INDEX_READY_TIMEOUT, || {
        database_state(&database)
            .is_some_and(|(generation, files, _)| generation == 1 && files == 1)
    });
}

pub(super) fn mcp_cold_first_call_completes_the_public_acceptance_flow() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "pub fn context_distillery_ready() -> bool { true }\n",
    )
    .expect("write fixture");
    let database = root.path().join("index.sqlite");
    let mut process = McpProcess::spawn(root.path(), &database);

    let initialize = process.initialize();
    assert_eq!(initialize["result"]["serverInfo"]["name"], "leantoken");
    assert_runtime_version(&initialize["result"]["serverInfo"]["version"]);
    assert!(
        initialize["result"]["instructions"]
            .as_str()
            .is_some_and(|instructions| {
                instructions.contains("Use savings for token statistics")
                    && instructions.contains("call leantoken.context once")
                    && instructions.contains("plan_only=false")
            })
    );
    process.send_initialized();

    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    let tools = process.response(Duration::from_secs(5));
    let names = tools["result"]["tools"]
        .as_array()
        .expect("tool catalog")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "context",
            "files",
            "history",
            "json",
            "outline",
            "read",
            "receipt_rebase",
            "savings",
            "search",
        ]
        .into_iter()
        .collect()
    );

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut id = 3;
    let mut saw_retryable = false;
    loop {
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "context",
                "arguments": {
                    "task": "find context_distillery_ready",
                    "token_budget": 200
                }
            }
        }));
        let response = process.response(deadline.saturating_duration_since(Instant::now()));
        assert_ne!(response["result"]["isError"], true, "{response}");
        if response["result"]["structuredContent"]["status"] == "retryable" {
            saw_retryable = true;
            id += 1;
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        assert_eq!(
            response["result"]["structuredContent"]["fragments"][0]["path"], "lib.rs",
            "{response}"
        );
        assert!(
            !saw_retryable,
            "short cold index escaped the bounded server-side wait"
        );
        break;
    }
}

pub(super) fn mcp_recovers_when_startup_database_contention_clears() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn answer() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    let blocker = rusqlite::Connection::open(&database).expect("open blocking connection");
    blocker
        .execute_batch(
            "CREATE TABLE startup_blocker(value INTEGER); \
             BEGIN EXCLUSIVE; \
             INSERT INTO startup_blocker(value) VALUES (1);",
        )
        .expect("hold database lock");

    let mut process = McpProcess::spawn(root.path(), &database);
    process.initialize();
    process.send_initialized();

    // Cross more than one startup busy-timeout and retry interval. A one-shot
    // startup would be permanently unavailable before the lock is released.
    std::thread::sleep(Duration::from_millis(750));
    blocker.execute_batch("ROLLBACK").expect("release database");
    process.wait_until_ready(INDEX_READY_TIMEOUT);
}

pub(super) fn mcp_eof_cancels_contended_startup_promptly() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn answer() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    let blocker = rusqlite::Connection::open(&database).expect("open blocking connection");
    blocker
        .execute_batch(
            "CREATE TABLE startup_blocker(value INTEGER); \
             BEGIN EXCLUSIVE; \
             INSERT INTO startup_blocker(value) VALUES (1);",
        )
        .expect("hold database lock");

    let mut process = McpProcess::spawn_with_captured_stderr(root.path(), &database, &[]);
    process.initialize();
    process.send_initialized();

    // Closing stdin before the runtime reaches its cancellable startup loop
    // races the production shutdown budget against config and coordination
    // work that has no cancellation checkpoint. Wait until the runtime
    // demonstrably holds the exclusive `.init.lock` coordination lock, which
    // it keeps while retrying the contended database open, so the EOF
    // cancellation always lands on a checkpoint.
    wait_until_lock_held(
        &std::path::PathBuf::from(format!("{}.init.lock", database.display())),
        Duration::from_secs(10),
        &mut process,
    );
    process.stdin.take();

    // Startup cancellation joins the runtime task under the production
    // shutdown budget; allow that same bounded window on slower CI runners.
    let status = process
        .wait_timeout(Duration::from_secs(15))
        .expect("wait for MCP process")
        .expect("MCP process should honor startup cancellation");
    assert!(
        status.success(),
        "MCP process exited with {status}: {}",
        String::from_utf8_lossy(&process.take_stderr())
    );
    blocker.execute_batch("ROLLBACK").expect("release database");
}

fn wait_until_lock_held(path: &std::path::Path, timeout: Duration, process: &mut McpProcess) {
    let deadline = Instant::now() + timeout;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
        {
            Ok(file) => match file.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return,
                Err(std::fs::TryLockError::Error(error)) => {
                    panic!("probing initialization lock {path:?} failed: {error}")
                }
            },
            Err(error) => panic!("opening initialization lock {path:?} failed: {error}"),
        }
        let mut stderr_diagnostics = || {
            // take_stderr joins a reader blocked on EOF, so stop the child
            // first when the deadline fails and the runtime stays alive.
            process.kill_now();
            String::from_utf8_lossy(&process.take_stderr()).into_owned()
        };
        assert!(
            Instant::now() < deadline,
            "MCP runtime never reached the cancellable startup phase while waiting \
             for {path:?}: {}",
            stderr_diagnostics()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn mcp_runtime_failure_transitions_tools_out_of_starting_state() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn answer() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    std::fs::create_dir(database.with_extension("sqlite.leader.lock"))
        .expect("invalid leadership artifact");

    let mut process = McpProcess::spawn(root.path(), &database);
    process.initialize();
    process.send_initialized();
    process.wait_until_unavailable(PROCESS_FAILURE_TIMEOUT);

    // Cross the former runtime-first shutdown timeout. A failed repository
    // service remains an operational MCP connection until the client closes
    // the stdio transport.
    std::thread::sleep(Duration::from_secs(6));
    assert!(process.child.try_wait().expect("poll process").is_none());
    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 50,
        "method": "tools/list",
        "params": {}
    }));
    let catalog = process.response(Duration::from_secs(2));
    assert_eq!(
        catalog["result"]["tools"].as_array().map(Vec::len),
        Some(9),
        "{catalog}"
    );
}

pub(super) fn cli_json_mcp_failure_is_one_document_after_a_logged_error() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn answer() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    std::fs::create_dir(database.with_extension("sqlite.leader.lock"))
        .expect("invalid leadership artifact");

    let mut process = McpProcess::spawn_with_captured_stderr(root.path(), &database, &["--json"]);
    process.initialize();
    process.send_initialized();
    process.wait_until_unavailable(PROCESS_FAILURE_TIMEOUT);
    process.stdin.take();

    let status = process
        .wait_timeout(PROCESS_FAILURE_TIMEOUT)
        .expect("wait for JSON MCP failure")
        .expect("JSON MCP process should exit after EOF");
    assert!(!status.success());

    let stderr = process.take_stderr();
    let error: serde_json::Value =
        serde_json::from_slice(&stderr).expect("one structured error without tracing records");
    assert_eq!(error["category"], "internal_error");
    assert!(error["error"].is_string());
    assert_eq!(error.as_object().map(serde_json::Map::len), Some(2));
}

pub(super) fn mcp_rejects_home_root_after_initialize_without_opening_storage() {
    let home = directories::BaseDirs::new()
        .expect("home directories")
        .home_dir()
        .canonicalize()
        .expect("canonical home");
    let cache = tempfile::tempdir().expect("cache");
    let database = cache.path().join("index.sqlite");
    let mut process = McpProcess::spawn(&home, &database);

    process.initialize();
    assert!(
        !database.exists(),
        "repository configuration ran before MCP initialization"
    );
    process.send_initialized();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut id = 2;
    loop {
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "files",
                "arguments": { "operation": {"kind": "tree", "max_results": 1} }
            }
        }));
        let response = process.response(deadline.saturating_duration_since(Instant::now()));
        if response["result"]["structuredContent"]["status"] == "unavailable" {
            assert_eq!(response["result"]["isError"], true);
            assert_eq!(
                response["result"]["structuredContent"]["reason"],
                "unsafe_repository_root"
            );
            assert!(
                !response
                    .to_string()
                    .contains(home.to_str().expect("UTF-8 home")),
                "unsafe path leaked in tool response: {response}"
            );
            assert!(!database.exists(), "unsafe root opened its SQLite cache");
            assert!(process.child.try_wait().expect("poll process").is_none());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "unsafe root remained hidden behind startup state: {response}"
        );
        id += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(super) fn mcp_index_limit_failure_is_terminal_and_does_not_retry() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("a.rs"), "fn original() {}\n").expect("fixture");
    std::fs::write(root.path().join("b.rs"), "fn crosses_limit() {}\n").expect("second file");
    let database = root.path().join("index.sqlite");
    let mut process = McpProcess::spawn_with_args(root.path(), &database, &["--max-files", "1"]);
    process.initialize();
    process.send_initialized();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut id = 100;
    loop {
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "files",
                "arguments": { "operation": {"kind": "tree", "max_results": 1} }
            }
        }));
        let response = process.response(deadline.saturating_duration_since(Instant::now()));
        if response["result"]["structuredContent"]["status"] == "unavailable" {
            assert_eq!(response["result"]["isError"], true);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "limit remained retryable: {response}"
        );
        id += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(database_state(&database).map(|state| state.0), Some(0));
    assert_eq!(database_state(&database).map(|state| state.1), Some(0));

    std::fs::remove_file(root.path().join("b.rs")).expect("shrink tree");
    std::thread::sleep(Duration::from_millis(1_250));
    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id + 1,
        "method": "tools/call",
        "params": {
            "name": "files",
            "arguments": { "operation": {"kind": "tree", "max_results": 1} }
        }
    }));
    let response = process.response(Duration::from_secs(5));
    assert_eq!(
        response["result"]["isError"], true,
        "runtime retried: {response}"
    );
    assert_eq!(
        response["result"]["structuredContent"]["status"],
        "unavailable"
    );
    assert_eq!(database_state(&database).map(|state| state.0), Some(0));
    assert_eq!(database_state(&database).map(|state| state.1), Some(0));
    assert!(process.child.try_wait().expect("poll process").is_none());
}

pub(super) fn concurrent_mcp_startup_initializes_once_and_followers_read() {
    let root = tempfile::tempdir().expect("temporary repository");
    write_rust_fixture_set(root.path(), "file", 20, 100);
    let database = root.path().join("index.sqlite");
    let mut processes = (0..3)
        .map(|_| McpProcess::spawn(root.path(), &database))
        .collect::<Vec<_>>();

    for process in &mut processes {
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "leantoken-test", "version": "1" }
            }
        }));
    }
    let initialize_deadline = Instant::now() + Duration::from_secs(5);
    for process in &processes {
        let response =
            process.response(initialize_deadline.saturating_duration_since(Instant::now()));
        assert_eq!(response["id"], 1);
        assert!(response.get("result").is_some(), "{response}");
    }
    for process in &mut processes {
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
    }

    wait_until(Duration::from_secs(15), || {
        database_state(&database)
            .is_some_and(|(generation, files, _)| generation == 1 && files == 20)
    });
    for process in &mut processes {
        process.wait_until_ready(Duration::from_secs(5));
    }
    assert_eq!(
        database_state(&database).map(|state| state.0),
        Some(1),
        "concurrent MCP followers must not publish duplicate generations"
    );
}

pub(super) fn mcp_follower_takes_over_after_leader_exit() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn before_failover() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    let mut leader = McpProcess::spawn(root.path(), &database);
    leader.initialize();
    leader.send_initialized();
    wait_until(INDEX_READY_TIMEOUT, || {
        database_state(&database)
            .is_some_and(|(generation, files, _)| generation == 1 && files == 1)
    });

    let mut follower = McpProcess::spawn(root.path(), &database);
    follower.initialize();
    follower.send_initialized();
    follower.wait_until_ready(Duration::from_secs(5));

    leader.stop();

    std::fs::write(
        root.path().join("lib.rs"),
        "fn changed_after_failover() {}\n",
    )
    .expect("modify repository after leader exit");
    wait_until(Duration::from_secs(15), || {
        database_state(&database)
            .is_some_and(|(generation, files, changed)| generation == 2 && files == 1 && changed)
    });
}

pub(super) fn mcp_follower_does_not_hide_terminal_generation_zero_failover() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("a.rs"), "fn first() {}\n").expect("first fixture");
    std::fs::write(root.path().join("b.rs"), "fn exceeds_limit() {}\n").expect("second fixture");
    let database = root.path().join("index.sqlite");
    let coordination = leantoken::coordination::IndexCoordination::for_database(&database);
    let operation_blocker = coordination
        .acquire_operation(&tokio_util::sync::CancellationToken::new())
        .expect("block leader reconciliation");

    let mut leader = McpProcess::spawn_with_args(root.path(), &database, &["--max-files", "1"]);
    leader.initialize();
    leader.send_initialized();
    wait_until(Duration::from_secs(5), || {
        coordination
            .try_acquire_leadership()
            .expect("probe leadership")
            .is_none()
    });

    let mut follower = McpProcess::spawn_with_args(root.path(), &database, &["--max-files", "1"]);
    follower.initialize();
    follower.send_initialized();

    drop(operation_blocker);

    // The process-level contract is that the follower eventually exposes the
    // terminal generation-zero failure instead of hiding it behind startup.
    // Coverage instrumentation and concurrent process tests can delay that
    // propagation; the one-second leadership grace is verified deterministically
    // in the Services tests, so this deliberately looser bound measures only
    // eventual visibility without making CI scheduling part of the contract.
    follower.wait_until_unavailable(FAILOVER_LIVENESS_TIMEOUT);
    assert_eq!(database_state(&database).map(|state| state.0), Some(0));
}

pub(super) fn mcp_follower_rebuilds_after_leader_is_killed_during_reconciliation() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("old.rs"),
        "fn committed_before_crash() {}\n",
    )
    .expect("old fixture");
    let database = root.path().join("index.sqlite");
    let initial = run(root.path(), &database, &["index"]);
    assert_eq!(initial["repository_generation"], 1);
    assert_eq!(database_state(&database).map(|state| state.1), Some(1));

    // Keep reconciliation large enough to kill the leader mid-flight without
    // making every product-loop run parse thousands of unnecessary symbols.
    write_rust_fixture_set(root.path(), "new", 20, 150);

    let coordination = leantoken::coordination::IndexCoordination::for_database(&database);
    let operation_blocker = coordination
        .acquire_operation(&tokio_util::sync::CancellationToken::new())
        .expect("block reconciliation");

    let mut leader = McpProcess::spawn(root.path(), &database);
    leader.initialize();
    leader.send_initialized();
    wait_until(Duration::from_secs(5), || {
        coordination
            .try_acquire_leadership()
            .expect("probe leadership")
            .is_none()
    });

    let mut follower = McpProcess::spawn(root.path(), &database);
    follower.initialize();
    follower.send_initialized();
    follower.wait_until_ready(Duration::from_secs(5));

    drop(operation_blocker);
    wait_until(Duration::from_secs(5), || {
        coordination.is_reconciling().expect("probe reconciliation")
    });
    leader.kill_now();

    wait_until(Duration::from_secs(5), || {
        database_state(&database)
            .is_some_and(|(generation, files, _)| generation == 1 && files == 1)
    });
    wait_until(Duration::from_secs(20), || {
        database_state(&database)
            .is_some_and(|(generation, files, _)| generation == 2 && files == 21)
    });
    follower.wait_until_ready(Duration::from_secs(5));
}
