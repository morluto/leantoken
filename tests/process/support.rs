use std::{
    ffi::OsString,
    io::{self, BufRead, BufReader, Read, Write},
    path::Path,
    process::{ChildStdin, ExitStatus, Stdio},
    sync::mpsc,
};

use clap::Parser;
use command_group::{CommandGroup, GroupChild};

pub(crate) use assert_cmd::Command;
pub(crate) use std::time::{Duration, Instant};

pub(crate) const EXPECTED_INDEX_CONTENT_VERSION: u64 = 13;

fn process_environment(root: &Path) -> Vec<(String, OsString)> {
    let host_home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    let home = if host_home.as_deref() == Some(root) {
        root.to_path_buf()
    } else {
        root.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir)
    };
    vec![
        ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
        ("HOME".into(), home.clone().into_os_string()),
        ("USERPROFILE".into(), home.clone().into_os_string()),
        ("LC_ALL".into(), "C".into()),
        ("LANG".into(), "C".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        (
            "GIT_CONFIG_GLOBAL".into(),
            home.join("global.gitconfig").into_os_string(),
        ),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_PAGER".into(), "cat".into()),
        ("PAGER".into(), "cat".into()),
    ]
}

fn apply_hermetic_environment(command: &mut Command, root: &Path) {
    command.env_clear();
    for (name, value) in process_environment(root) {
        command.env(name, value);
    }
}

fn apply_hermetic_std_environment(command: &mut std::process::Command, root: &Path) {
    command.env_clear();
    for (name, value) in process_environment(root) {
        command.env(name, value);
    }
}

pub(crate) fn assert_runtime_version(value: &serde_json::Value) {
    let version = value.as_str().expect("runtime version string");
    let fingerprint = version
        .strip_prefix(concat!(env!("CARGO_PKG_VERSION"), "+contract."))
        .expect("runtime version carries the current package version and contract fingerprint");
    assert_eq!(fingerprint.len(), 32);
    assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

pub(crate) fn run(
    root: &std::path::Path,
    database: &std::path::Path,
    arguments: &[&str],
) -> serde_json::Value {
    let mut command = Command::cargo_bin("leantoken").expect("binary");
    apply_hermetic_environment(&mut command, root);
    let output = command
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

pub(crate) fn run_error(
    root: &std::path::Path,
    database: &std::path::Path,
    arguments: &[&str],
) -> serde_json::Value {
    let mut command = Command::cargo_bin("leantoken").expect("binary");
    apply_hermetic_environment(&mut command, root);
    let output = command
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
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    serde_json::from_slice(&output.stderr).expect("structured error")
}

pub(crate) fn assert_cli_parse_error(arguments: &[&str]) {
    let expected = leantoken::cli::Cli::try_parse_from(
        std::iter::once(leantoken_program_name())
            .chain(arguments.iter().map(std::ffi::OsString::from)),
    )
    .expect_err("invalid CLI arguments")
    .to_string();
    let mut command = Command::cargo_bin("leantoken").expect("binary");
    apply_hermetic_environment(
        &mut command,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    );
    let output = command
        .args(arguments)
        .output()
        .expect("run CLI parse failure");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stderr)
            .expect("structured parse error"),
        serde_json::json!({
            "error": expected.trim_end(),
            "category": "invalid_input"
        })
    );
}

pub(crate) fn leantoken_program_name() -> std::ffi::OsString {
    assert_cmd::cargo::cargo_bin!("leantoken")
        .file_name()
        .expect("binary file name")
        .to_os_string()
}

pub(crate) struct McpProcess {
    pub(crate) child: GroupChild,
    pub(crate) stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    stderr_task: Option<std::thread::JoinHandle<Vec<u8>>>,
}

impl McpProcess {
    pub(crate) fn spawn(root: &std::path::Path, database: &std::path::Path) -> Self {
        Self::spawn_with_args(root, database, &[])
    }

    pub(crate) fn spawn_with_args(
        root: &std::path::Path,
        database: &std::path::Path,
        arguments: &[&str],
    ) -> Self {
        Self::spawn_with_options(root, database, arguments, false)
    }

    pub(crate) fn spawn_with_mcp_args(
        root: &std::path::Path,
        database: &std::path::Path,
        arguments: &[&str],
    ) -> Self {
        Self::spawn_with_command_args(root, database, &[], arguments, false)
    }

    pub(crate) fn spawn_with_captured_stderr(
        root: &std::path::Path,
        database: &std::path::Path,
        arguments: &[&str],
    ) -> Self {
        Self::spawn_with_options(root, database, arguments, true)
    }

    pub(crate) fn spawn_with_options(
        root: &std::path::Path,
        database: &std::path::Path,
        arguments: &[&str],
        capture_stderr: bool,
    ) -> Self {
        Self::spawn_with_command_args(root, database, arguments, &[], capture_stderr)
    }

    pub(crate) fn spawn_with_command_args(
        root: &std::path::Path,
        database: &std::path::Path,
        arguments: &[&str],
        mcp_arguments: &[&str],
        capture_stderr: bool,
    ) -> Self {
        let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("leantoken"));
        apply_hermetic_std_environment(&mut command, root);
        command
            .args([
                "--root",
                root.to_str().expect("root UTF-8"),
                "--database",
                database.to_str().expect("database UTF-8"),
            ])
            .args(arguments)
            .arg("mcp")
            .args(mcp_arguments);
        command.stderr(if capture_stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .group_spawn()
            .expect("spawn MCP process");
        let stdin = child.inner().stdin.take().expect("MCP stdin");
        let stdout = child.inner().stdout.take().expect("MCP stdout");
        let stderr_task = child.inner().stderr.take().map(|mut stderr| {
            std::thread::spawn(move || {
                let mut output = Vec::new();
                stderr.read_to_end(&mut output).expect("read MCP stderr");
                output
            })
        });
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
            stderr_task,
        }
    }

    pub(crate) fn take_stderr(&mut self) -> Vec<u8> {
        self.stderr_task
            .take()
            .expect("captured MCP stderr")
            .join()
            .expect("join MCP stderr reader")
    }

    pub(crate) fn initialize(&mut self) -> serde_json::Value {
        self.initialize_as("leantoken-test", "1", "2025-11-25")
    }

    pub(crate) fn initialize_as(
        &mut self,
        client_name: &str,
        client_version: &str,
        protocol_version: &str,
    ) -> serde_json::Value {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": protocol_version,
                "capabilities": {},
                "clientInfo": { "name": client_name, "version": client_version }
            }
        }));
        let response = self.response(Duration::from_secs(5));
        assert_eq!(response["id"], 1);
        assert!(response.get("result").is_some(), "{response}");
        response
    }

    pub(crate) fn send_initialized(&mut self) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
    }

    pub(crate) fn wait_until_ready(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut id = 2;
        while Instant::now() < deadline {
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": "files",
                    "arguments": { "operation": {"kind": "tree", "max_results": 1} }
                }
            }));
            let response = self.response(deadline.saturating_duration_since(Instant::now()));
            if response["result"]["isError"] != true
                && response["result"]["structuredContent"]["status"] != "retryable"
            {
                return;
            }
            id += 1;
        }
        panic!("MCP process did not become ready within {timeout:?}");
    }

    pub(crate) fn wait_until_unavailable(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut id = 2;
        loop {
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": "files",
                    "arguments": { "operation": {"kind": "tree", "max_results": 1} }
                }
            }));
            let response = self.response(deadline.saturating_duration_since(Instant::now()));
            let message = response["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default();
            if message.contains("unavailable") {
                assert_eq!(response["result"]["isError"], true);
                assert!(self.child.try_wait().expect("poll process").is_none());
                return;
            }
            assert!(
                Instant::now() < deadline,
                "runtime failure remained hidden behind startup state: {response}"
            );
            id += 1;
        }
    }

    pub(crate) fn send(&mut self, message: serde_json::Value) {
        let stdin = self.stdin.as_mut().expect("live MCP stdin");
        serde_json::to_writer(&mut *stdin, &message).expect("write MCP message");
        stdin.write_all(b"\n").expect("terminate MCP message");
        stdin.flush().expect("flush MCP message");
    }

    pub(crate) fn send_raw_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("live MCP stdin");
        stdin
            .write_all(line.as_bytes())
            .expect("write raw MCP line");
        stdin.write_all(b"\n").expect("terminate raw MCP line");
        stdin.flush().expect("flush raw MCP line");
    }

    pub(crate) fn send_raw(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("live MCP stdin");
        stdin.write_all(bytes).expect("write raw MCP bytes");
        stdin.flush().expect("flush raw MCP bytes");
    }

    pub(crate) fn message(&self, timeout: Duration) -> serde_json::Value {
        let line = self
            .lines
            .recv_timeout(timeout)
            .expect("MCP message before deadline");
        serde_json::from_str(&line).expect("MCP JSON message")
    }

    pub(crate) fn response(&self, timeout: Duration) -> serde_json::Value {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let value = self.message(remaining);
            if value.get("id").is_some() {
                return value;
            }
        }
    }

    pub(crate) fn wait_timeout(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::yield_now();
        }
    }

    pub(crate) fn stop(&mut self) {
        self.stdin.take();
        if self.child.try_wait().expect("poll child").is_none() {
            self.child.kill().expect("kill MCP child");
        }
        self.child.wait().expect("join MCP child");
    }

    pub(crate) fn kill_now(&mut self) {
        self.child.kill().expect("kill MCP child");
        self.child.wait().expect("join killed MCP child");
        self.stdin.take();
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.stdin.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(task) = self.stderr_task.take() {
            let _ = task.join();
        }
    }
}

pub(crate) fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::yield_now();
    }
    panic!("condition not met within {timeout:?}");
}

pub(crate) fn write_rust_fixture_set(
    root: &std::path::Path,
    prefix: &str,
    file_count: usize,
    functions_per_file: usize,
) {
    for file in 0..file_count {
        let content = (0..functions_per_file)
            .map(|function| format!("fn item_{file}_{function}() -> usize {{ {function} }}\n"))
            .collect::<String>();
        std::fs::write(root.join(format!("{prefix}_{file}.rs")), content)
            .expect("write generated Rust fixture");
    }
}

pub(crate) fn database_state(database: &std::path::Path) -> Option<(u64, u64, bool)> {
    let connection =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
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
