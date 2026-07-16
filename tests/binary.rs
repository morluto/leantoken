use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use assert_cmd::Command;
use wait_timeout::ChildExt;

#[test]
fn cli_indexes_statuses_and_searches_as_json() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");

    let index = run(root.path(), &database, &["index"]);
    assert!(
        index["files_indexed"]
            .as_u64()
            .is_some_and(|value| value >= 1)
    );

    let status = run(root.path(), &database, &["status"]);
    assert_eq!(status["file_count"], 1);

    let search = run(
        root.path(),
        &database,
        &[
            "search",
            "answer",
            "--mode",
            "identifier",
            "--max-tokens",
            "100",
        ],
    );
    assert_eq!(search["hits"][0]["path"], "lib.rs");
    assert!(
        search["meta"]["emitted_tokens"]
            .as_u64()
            .is_some_and(|value| value <= 100)
    );
}

#[test]
fn mcp_repeatedly_exits_cleanly_on_stdio_eof() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");

    for _ in 0..3 {
        Command::cargo_bin("leantoken")
            .expect("binary")
            .args([
                "--root",
                root.path().to_str().expect("root UTF-8"),
                "--database",
                database.to_str().expect("database UTF-8"),
                "mcp",
            ])
            .write_stdin("")
            // The deadline covers cold indexing and watcher startup as well as
            // transport shutdown, which is materially slower on Windows runners.
            .timeout(std::time::Duration::from_secs(30))
            .assert()
            .success();
    }
}

#[test]
fn mcp_survives_malformed_and_invalid_messages() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");
    let mut process = McpProcess::spawn(root.path(), &database);
    process.initialize();
    process.send_initialized();
    process.wait_until_ready(Duration::from_secs(10));

    // rmcp intentionally ignores unparsable input, but a well-formed value
    // with the wrong JSON-RPC shape receives Invalid Request. Neither may
    // close the stdio transport or poison the next tool call.
    process.send_raw_line("{not json");
    process.send_raw_line(r#"{"foo":"bar"}"#);
    let invalid = process.message(Duration::from_secs(2));
    assert_eq!(invalid["error"]["code"], -32600);

    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 100,
        "method": "tools/call",
        "params": {
            "name": "leantoken_files",
            "arguments": { "operation": "tree", "max_results": 1 }
        }
    }));
    let response = process.response(Duration::from_secs(2));
    assert_eq!(response["id"], 100);
    assert!(response.get("result").is_some(), "{response}");
    assert!(process.child.try_wait().expect("poll process").is_none());
}

#[test]
fn mcp_initialize_precedes_storage_open() {
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
    let response = process.response(Duration::from_secs(2));
    assert_eq!(response["id"], 1);
    assert!(response.get("result").is_some(), "{response}");

    blocker.execute_batch("ROLLBACK").expect("release database");
    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    wait_until(Duration::from_secs(10), || {
        database_state(&database).is_some_and(|(generation, files, _)| {
            generation == 1 && files == 1
        })
    });
}

#[test]
fn mcp_recovers_when_startup_database_contention_clears() {
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

    // The previous one-shot startup became permanently unavailable after the
    // five-second SQLite busy timeout. Keep the lock beyond that boundary,
    // then prove the same MCP process recovers after it is released.
    std::thread::sleep(Duration::from_millis(5_500));
    blocker.execute_batch("ROLLBACK").expect("release database");
    process.wait_until_ready(Duration::from_secs(10));
}

#[test]
fn mcp_eof_cancels_contended_startup_promptly() {
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
    process.stdin.take();

    let status = process
        .child
        .wait_timeout(Duration::from_secs(2))
        .expect("wait for MCP process")
        .expect("MCP process should honor startup cancellation");
    assert!(status.success(), "MCP process exited with {status}");
    blocker.execute_batch("ROLLBACK").expect("release database");
}

#[test]
fn mcp_runtime_failure_transitions_tools_out_of_starting_state() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn answer() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    std::fs::create_dir(database.with_extension("sqlite.leader.lock"))
        .expect("invalid leadership artifact");

    let mut process = McpProcess::spawn(root.path(), &database);
    process.initialize();
    process.send_initialized();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut id = 2;
    loop {
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "leantoken_files",
                "arguments": { "operation": "tree", "max_results": 1 }
            }
        }));
        let response = process.response(deadline.saturating_duration_since(Instant::now()));
        let message = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        if message.contains("unavailable") {
            assert_eq!(response["result"]["isError"], true);
            assert!(process.child.try_wait().expect("poll process").is_none());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "runtime failure remained hidden behind startup state: {response}"
        );
        id += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn concurrent_mcp_startup_initializes_once_and_followers_read() {
    let root = tempfile::tempdir().expect("temporary repository");
    for file in 0..60 {
        let content = (0..200)
            .map(|function| format!("fn item_{file}_{function}() -> usize {{ {function} }}\n"))
            .collect::<String>();
        std::fs::write(root.path().join(format!("file_{file}.rs")), content)
            .expect("write fixture");
    }
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
        let response = process.response(initialize_deadline.saturating_duration_since(Instant::now()));
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
        database_state(&database).is_some_and(|(generation, files, _)| {
            generation == 1 && files == 60
        })
    });
    std::thread::sleep(Duration::from_millis(750));
    assert_eq!(
        database_state(&database).map(|state| state.0),
        Some(1),
        "concurrent MCP followers must not publish duplicate generations"
    );

    for process in &mut processes {
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "leantoken_files",
                "arguments": { "operation": "tree", "max_results": 5 }
            }
        }));
    }
    for process in &processes {
        let response = process.response(Duration::from_secs(5));
        assert_eq!(response["id"], 2);
        assert_ne!(response["result"]["isError"], true);
        assert!(
            response["result"]["structuredContent"]["entries"]
                .as_array()
                .is_some_and(|entries| !entries.is_empty()),
            "follower did not observe the committed generation: {response}"
        );
    }
}

#[test]
fn mcp_follower_takes_over_after_leader_exit() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn before_failover() {}\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");
    let mut leader = McpProcess::spawn(root.path(), &database);
    leader.initialize();
    leader.send_initialized();
    wait_until(Duration::from_secs(10), || {
        database_state(&database).is_some_and(|(generation, files, _)| {
            generation == 1 && files == 1
        })
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
        database_state(&database).is_some_and(|(generation, files, changed)| {
            generation == 2 && files == 1 && changed
        })
    });
}

#[test]
fn setup_and_remove_do_not_require_a_repository() {
    let temp = tempfile::tempdir().expect("temporary home");
    let missing_root = temp.path().join("not-a-repository");

    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .args([
            "--root",
            missing_root.to_str().expect("root UTF-8"),
            "--json",
            "setup",
            "--claude",
            "--yes",
        ])
        .output()
        .expect("run setup");
    assert!(
        setup.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
    assert_eq!(report["results"][0]["status"], "configured");
    let config = std::fs::read_to_string(temp.path().join(".claude.json"))
        .expect("Claude configuration");
    assert!(config.contains("\"leantoken\""));
    assert!(config.contains("\"mcp\""));

    let remove = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .args(["--json", "remove", "--claude", "--yes"])
        .output()
        .expect("run remove");
    assert!(
        remove.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&remove.stderr)
    );
    let config = std::fs::read_to_string(temp.path().join(".claude.json"))
        .expect("Claude configuration after removal");
    assert!(!config.contains("\"leantoken\""));
}

#[test]
fn npx_setup_registers_an_unpinned_launcher_instead_of_its_cache_path() {
    let temp = tempfile::tempdir().expect("temporary home");
    let node = temp.path().join("node");
    let npm = temp.path().join("npm-cli.js");
    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("npm_lifecycle_event", "npx")
        .env("npm_node_execpath", &node)
        .env("npm_execpath", &npm)
        .args(["--json", "setup", "--claude", "--yes"])
        .output()
        .expect("run npx setup");
    assert!(
        setup.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(temp.path().join(".claude.json"))
            .expect("Claude configuration"),
    )
    .expect("Claude JSON");
    assert_eq!(config["mcpServers"]["leantoken"]["command"], node.to_str().unwrap());
    assert_eq!(
        config["mcpServers"]["leantoken"]["args"],
        serde_json::json!([
            npm.to_str().unwrap(),
            "exec",
            "--yes",
            "--package=leantoken",
            "--",
            "leantoken",
            "mcp"
        ])
    );
}

fn run(
    root: &std::path::Path,
    database: &std::path::Path,
    arguments: &[&str],
) -> serde_json::Value {
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--json",
        ])
        .args(arguments)
        .output()
        .expect("run leantoken");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
}

impl McpProcess {
    fn spawn(root: &std::path::Path, database: &std::path::Path) -> Self {
        let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin!("leantoken"))
            .args([
                "--root",
                root.to_str().expect("root UTF-8"),
                "--database",
                database.to_str().expect("database UTF-8"),
                "mcp",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn MCP process");
        let stdin = child.stdin.take().expect("MCP stdin");
        let stdout = child.stdout.take().expect("MCP stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            lines,
        }
    }

    fn initialize(&mut self) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "leantoken-test", "version": "1" }
            }
        }));
        let response = self.response(Duration::from_secs(5));
        assert_eq!(response["id"], 1);
        assert!(response.get("result").is_some(), "{response}");
    }

    fn send_initialized(&mut self) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
    }

    fn wait_until_ready(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut id = 2;
        while Instant::now() < deadline {
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": "leantoken_files",
                    "arguments": { "operation": "tree", "max_results": 1 }
                }
            }));
            let response = self.response(deadline.saturating_duration_since(Instant::now()));
            if response["result"]["isError"] != true {
                return;
            }
            id += 1;
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("MCP process did not become ready within {timeout:?}");
    }

    fn send(&mut self, message: serde_json::Value) {
        let stdin = self.stdin.as_mut().expect("live MCP stdin");
        serde_json::to_writer(&mut *stdin, &message).expect("write MCP message");
        stdin.write_all(b"\n").expect("terminate MCP message");
        stdin.flush().expect("flush MCP message");
    }

    fn send_raw_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("live MCP stdin");
        stdin.write_all(line.as_bytes()).expect("write raw MCP line");
        stdin.write_all(b"\n").expect("terminate raw MCP line");
        stdin.flush().expect("flush raw MCP line");
    }

    fn message(&self, timeout: Duration) -> serde_json::Value {
        let line = self
            .lines
            .recv_timeout(timeout)
            .expect("MCP message before deadline");
        serde_json::from_str(&line).expect("MCP JSON message")
    }

    fn response(&self, timeout: Duration) -> serde_json::Value {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let value = self.message(remaining);
            if value.get("id").is_some() {
                return value;
            }
        }
    }

    fn stop(&mut self) {
        self.stdin.take();
        if self.child.try_wait().expect("poll child").is_none() {
            self.child.kill().expect("kill MCP child");
        }
        self.child.wait().expect("join MCP child");
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.stdin.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("condition not met within {timeout:?}");
}

fn database_state(database: &std::path::Path) -> Option<(u64, u64, bool)> {
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    connection.busy_timeout(Duration::from_millis(50)).ok()?;
    let generation = connection
        .query_row(
            "SELECT repository_generation FROM meta WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .ok()
        .and_then(|value| u64::try_from(value).ok())?;
    let files = connection
        .query_row("SELECT count(*) FROM files", [], |row| row.get::<_, i64>(0))
        .ok()
        .and_then(|value| u64::try_from(value).ok())?;
    let changed = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM chunks WHERE content LIKE '%changed_after_failover%')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .ok()?;
    Some((generation, files, changed))
}
