use super::support::Command;

#[test]
fn setup_dry_run_reports_exact_plan_without_mutation() {
    let temp = tempfile::tempdir().expect("temporary home");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .args(["--json", "setup", "--codex", "--dry-run"])
        .output()
        .expect("run setup dry-run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("dry-run JSON output");
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["plan"][0]["client"], "codex");
    assert_eq!(report["plan"][0]["action"], "create");
    assert_eq!(report["launcher"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["launcher"]["package"], serde_json::Value::Null);
    assert_eq!(report["launcher"]["may_contact_network"], false);
    assert!(!temp.path().join(".codex/config.toml").exists());
}

#[test]
fn malformed_selected_config_blocks_all_setup_writes() {
    let temp = tempfile::tempdir().expect("temporary home");
    std::fs::write(temp.path().join(".claude.json"), "{ broken").expect("write malformed config");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .args(["--json", "setup", "--claude", "--cursor", "--yes"])
        .output()
        .expect("run setup");
    assert!(!output.status.success());
    assert!(!temp.path().join(".cursor/mcp.json").exists());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured setup error");
    assert_eq!(error["category"], "setup_failure");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("refusing to overwrite malformed config"))
    );
    assert_eq!(error.as_object().map(serde_json::Map::len), Some(2));
}

#[test]
fn npx_setup_registers_exact_release_instead_of_its_cache_path() {
    let temp = tempfile::tempdir().expect("temporary home");
    let runtime = temp.path().join("node runtime");
    let node = runtime.join(if cfg!(windows) { "node.exe" } else { "node" });
    let npx = runtime.join("npx cli.js");
    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("npm_lifecycle_event", "npx")
        .env("npm_node_execpath", &node)
        .env("npm_execpath", &npx)
        .args(["--json", "setup", "--claude", "--yes"])
        .output()
        .expect("run npx setup");
    assert!(
        setup.status.success(),
        "ambient lifecycle metadata must not break a persistent executable"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
    assert_eq!(report["verification"]["status"], "passed");
    let executable = assert_cmd::cargo::cargo_bin!("leantoken")
        .canonicalize()
        .expect("canonical executable");
    assert_eq!(report["launcher"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["launcher"]["package"], serde_json::Value::Null);
    assert_eq!(report["launcher"]["may_contact_network"], false);
    assert_eq!(report["launcher"]["command"], executable.to_str().unwrap());
    assert_eq!(
        report["launcher"]["args"],
        serde_json::json!(["--managed-by-setup", "mcp"])
    );

    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(temp.path().join(".claude.json")).expect("Claude configuration"),
    )
    .expect("Claude JSON");
    assert_eq!(
        config["mcpServers"]["leantoken"]["command"],
        executable.to_str().unwrap()
    );
    assert_eq!(
        config["mcpServers"]["leantoken"]["args"],
        serde_json::json!(["--managed-by-setup", "mcp"])
    );
}

#[test]
fn setup_refresh_targets_only_existing_mcp_entries() {
    let temp = tempfile::tempdir().expect("temporary home");
    let node = temp.path().join("node");
    let npm = temp.path().join("npm-cli.js");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("npm_lifecycle_event", "npx")
            .env("npm_node_execpath", &node)
            .env("npm_execpath", &npm);
        command
    };
    let setup = command()
        .args(["--json", "setup", "--claude", "--yes"])
        .output()
        .expect("run initial setup");
    assert!(
        setup.status.success(),
        "ambient lifecycle metadata must not break a persistent executable"
    );
    let setup_report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
    assert_eq!(setup_report["verification"]["status"], "passed");
    std::fs::create_dir_all(temp.path().join(".cursor")).expect("Cursor directory");
    std::fs::write(
        temp.path().join(".cursor/mcp.json"),
        "{\"mcpServers\":{\"other\":{\"command\":\"other\"}}}\n",
    )
    .expect("Cursor config");

    let refresh = command()
        .args(["--json", "setup", "--refresh", "--yes"])
        .output()
        .expect("run setup refresh");
    assert!(refresh.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&refresh.stdout).expect("refresh JSON output");
    assert_eq!(report["verification"]["status"], "passed");
    assert_eq!(report["plan"].as_array().unwrap().len(), 1);
    assert_eq!(report["plan"][0]["client"], "claude");
    assert_eq!(report["plan"][0]["action"], "already_current");
    let cursor = std::fs::read_to_string(temp.path().join(".cursor/mcp.json"))
        .expect("Cursor config after refresh");
    assert!(!cursor.contains("\"leantoken\""));
}

#[test]
fn empty_setup_refresh_reports_unrecognized_clients_without_mutation() {
    let temp = tempfile::tempdir().expect("temporary home");
    let config = temp.path().join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).expect("Codex directory");
    let original_config =
        "[mcp_servers.leantoken]\ncommand = \"/opt/manual-leantoken\"\nargs = [\"mcp\"]\n";
    std::fs::write(&config, original_config).expect("Codex configuration");
    let discovery = temp.path().join(".agents/skills/leantoken/SKILL.md");
    std::fs::create_dir_all(discovery.parent().unwrap()).expect("discovery directory");
    let original_discovery = "<!-- managed by leantoken setup -->\nlegacy discovery\n";
    std::fs::write(&discovery, original_discovery).expect("discovery skill");

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .args(["setup", "--refresh", "--yes"])
        .output()
        .expect("run setup refresh");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(
        "No existing LeanToken client registrations were recognized; no changes were made."
    ));
    assert_eq!(std::fs::read_to_string(config).unwrap(), original_config);
    assert_eq!(
        std::fs::read_to_string(discovery).unwrap(),
        original_discovery
    );
}

#[test]
fn private_runtime_setup_installs_and_registers_the_verified_native_binary() {
    let temp = tempfile::tempdir().expect("temporary home");
    let data_home = temp.path().join("data");
    let runtime = temp.path().join("node runtime");
    std::fs::create_dir(temp.path().join(".codex")).unwrap();
    std::fs::write(
        temp.path().join(".codex/config.toml"),
        "fixture_user_setting = \"preserve\"\n[mcp_servers.fixture_user_tool]\ncommand = \"fixture-user-tool\"\nargs = [\"--fixture\"]\n",
    ).unwrap();
    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("XDG_DATA_HOME", &data_home)
        .env("LOCALAPPDATA", &data_home)
        .env("npm_lifecycle_event", "npx")
        .env("npm_node_execpath", runtime.join("node"))
        .env("npm_execpath", runtime.join("npm-cli.js"))
        .args(["--json", "setup", "--codex", "--private-runtime", "--yes"])
        .output()
        .expect("run private runtime setup");
    assert!(
        setup.status.success(),
        "status: {}\nsetup report: {}\nstderr: {}",
        setup.status,
        String::from_utf8_lossy(&setup.stdout[..setup.stdout.len().min(16 * 1024)]),
        String::from_utf8_lossy(&setup.stderr[..setup.stderr.len().min(16 * 1024)])
    );
    let report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
    let runtime_path = std::path::PathBuf::from(
        report["launcher"]["runtime_path"]
            .as_str()
            .expect("runtime path"),
    );
    assert!(runtime_path.exists());
    assert_eq!(report["launcher"]["package"], serde_json::Value::Null);
    assert_eq!(report["launcher"]["may_contact_network"], false);
    assert_eq!(report["discovery_plan"].as_array().map(Vec::len), Some(1));

    let version = Command::new(&runtime_path)
        .arg("--version")
        .output()
        .expect("run installed native executable");
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
    let repository = temp.path().join("workspace");
    std::fs::create_dir(&repository).expect("workspace");
    std::fs::write(
        repository.join("lib.rs"),
        "pub fn private_runtime_retrieval() -> bool { true }\n",
    )
    .expect("workspace source");
    let doctor = Command::new(&runtime_path)
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("XDG_DATA_HOME", &data_home)
        .env("LOCALAPPDATA", &data_home)
        .args([
            "--root",
            repository.to_str().expect("repository UTF-8"),
            "--database",
            temp.path()
                .join("private-runtime.sqlite")
                .to_str()
                .expect("database UTF-8"),
            "--json",
            "doctor",
            "--client",
            "codex",
        ])
        .output()
        .expect("run installed runtime doctor");
    assert!(
        doctor.status.success(),
        "private runtime doctor stderr: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let doctor_report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("doctor report");
    assert_eq!(doctor_report["integration"]["verified_client"], "codex");
    let codex_registration = doctor_report["integration"]["registrations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|registration| registration["client"] == "codex")
        .expect("private runtime Codex registration");
    assert_eq!(
        codex_registration["configured_version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        codex_registration["configured_version_pin"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        doctor_report["tools"],
        serde_json::json!([
            "context",
            "files",
            "history",
            "json",
            "outline",
            "read",
            "receipt_rebase",
            "savings",
            "search"
        ])
    );
    assert_eq!(doctor_report["first_call"]["status"], "ready");
    let codex = std::fs::read_to_string(temp.path().join(".codex/config.toml"))
        .expect("Codex configuration");
    assert!(codex.contains(runtime_path.to_str().expect("UTF-8 runtime path")));
    assert!(!codex.contains("npm-cli"));
    assert_private_runtime_process_lifetime(
        temp.path(),
        &data_home,
        &runtime_path,
        &repository,
        &doctor_report["server_version"],
    );
}

struct ManagedRuntimeLeaseFixture<'a> {
    home: &'a std::path::Path,
    data_home: &'a std::path::Path,
    artifact: &'a std::path::Path,
    repository: &'a std::path::Path,
}

impl ManagedRuntimeLeaseFixture<'_> {
    fn command(&self, executable: &std::path::Path) -> std::process::Command {
        let mut command = std::process::Command::new(executable);
        command
            .current_dir(self.home)
            .env("HOME", self.home)
            .env("USERPROFILE", self.home)
            .env("XDG_DATA_HOME", self.data_home)
            .env("LOCALAPPDATA", self.data_home)
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("SQLITE_TMPDIR", self.home)
            .env("TMPDIR", self.home)
            .env("TMP", self.home)
            .env("TEMP", self.home)
            .env_remove("npm_lifecycle_event");
        command
    }

    fn cli(&self, arguments: &[&str]) -> serde_json::Value {
        let output = Command::from_std(self.command(assert_cmd::cargo::cargo_bin!("leantoken")))
            .timeout(std::time::Duration::from_secs(30))
            .arg("--json")
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("runtime lifecycle JSON")
    }

    fn spawn(&self, omit_home: bool) -> super::support::McpProcess {
        let mut command = self.command(self.artifact);
        if omit_home {
            command.env_remove("HOME");
        }
        command
            .arg("--root")
            .arg(self.repository)
            .arg("--database")
            .arg(self.home.join("private-runtime.sqlite"))
            .args(["--tokenizer", "estimate", "--managed-by-setup", "mcp"])
            .stderr(std::process::Stdio::piped());
        super::support::McpProcess::spawn_command(command)
    }

    fn remove_registration_preserving_user_entries(&self) {
        let removed = self.cli(&["remove", "--codex", "--yes"]);
        assert_eq!(removed["results"][0]["client"], "codex");
        assert_eq!(removed["results"][0]["status"], "removed");
        let source = std::fs::read_to_string(self.home.join(".codex/config.toml")).unwrap();
        let config = source.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(config["fixture_user_setting"].as_str(), Some("preserve"));
        assert_eq!(
            config["mcp_servers"]["fixture_user_tool"]["command"].as_str(),
            Some("fixture-user-tool")
        );
        assert_eq!(
            config["mcp_servers"]["fixture_user_tool"]["args"][0].as_str(),
            Some("--fixture")
        );
        assert!(
            !config["mcp_servers"]
                .as_table()
                .unwrap()
                .contains_key("leantoken")
        );
    }

    fn assert_active_retention(&self) {
        let inventory = self.cli(&["runtime", "list"]);
        assert_eq!(inventory["total_entries"], 1);
        assert_eq!(
            inventory["entries"][0]["path"],
            self.artifact.to_str().unwrap()
        );
        assert_eq!(inventory["entries"][0]["active"], true);
        assert_eq!(
            inventory["entries"][0]["referenced_by"],
            serde_json::json!([])
        );
        let pruned = self.cli(&["runtime", "prune", "--keep-latest", "0", "--yes"]);
        assert_eq!(pruned["results"][0]["action"], "retained");
        assert_eq!(pruned["results"][0]["reason"], "active_process");
        assert!(self.artifact.exists());
    }

    fn finish_and_assert_inactive(&self, mut process: super::support::McpProcess, crash: bool) {
        assert!(
            process.child.try_wait().unwrap().is_none(),
            "MCP must still be alive before termination"
        );
        if crash {
            process
                .child
                .kill()
                .expect("terminate owned MCP process group");
        } else {
            drop(process.stdin.take());
        }
        let status = process
            .wait_timeout(std::time::Duration::from_secs(15))
            .unwrap()
            .expect("owned MCP process must exit before lease-release assertions");
        assert_eq!(status.success(), !crash, "unexpected MCP exit: {status}");
        let inventory = self.cli(&["runtime", "list"]);
        assert_eq!(inventory["entries"][0]["active"], false);
        assert!(self.artifact.exists());
    }

    fn assert_removed_after_exit(&self) {
        let pruned = self.cli(&["runtime", "prune", "--keep-latest", "0", "--yes"]);
        assert_eq!(pruned["results"][0]["action"], "removed");
        assert_eq!(pruned["total_bytes_after"], 0);
        assert!(!self.artifact.exists());
        let root = self.artifact.parent().unwrap().parent().unwrap();
        let leases = std::fs::read_dir(root.join(".leases"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(
            leases.len(),
            1,
            "one stable lease for the single installed artifact"
        );
        assert_eq!(std::fs::metadata(&leases[0]).unwrap().len(), 0);
    }
}

fn assert_private_runtime_process_lifetime(
    home: &std::path::Path,
    data_home: &std::path::Path,
    artifact: &std::path::Path,
    repository: &std::path::Path,
    expected_mcp_version: &serde_json::Value,
) {
    super::support::assert_runtime_version(expected_mcp_version);
    let fixture = ManagedRuntimeLeaseFixture {
        home,
        data_home,
        artifact,
        repository,
    };
    // One installed copy and database, with at most one live MCP child. Each
    // phase uses the existing five-second initialize and bounded exit helpers.
    // The second launch also exercises Windows' USERPROFILE home fallback when
    // HOME is absent; its platform directory discovery can still be unavailable.
    for (crash, omit_home) in [(false, false), (true, cfg!(windows))] {
        let mut process = fixture.spawn(omit_home);
        let initialized = process.initialize();
        assert_eq!(
            &initialized["result"]["serverInfo"]["version"],
            expected_mcp_version
        );
        process.send_initialized();
        if !crash {
            fixture.remove_registration_preserving_user_entries();
        }
        fixture.assert_active_retention();
        fixture.finish_and_assert_inactive(process, crash);
    }
    fixture.assert_removed_after_exit();
}

#[cfg(target_os = "linux")]
struct PrivateRuntimeFixture {
    _temporary: tempfile::TempDir,
    home: std::path::PathBuf,
    data_home: std::path::PathBuf,
    original: std::path::PathBuf,
    alternate: std::path::PathBuf,
}

#[cfg(target_os = "linux")]
impl PrivateRuntimeFixture {
    fn new() -> Self {
        use std::io::Write;

        let temporary = tempfile::tempdir().expect("private runtime fixture");
        let home = temporary.path().join("home");
        std::fs::create_dir(&home).expect("fixture home");
        let data_home = temporary.path().join("data");
        let original = assert_cmd::cargo::cargo_bin!("leantoken")
            .canonicalize()
            .expect("canonical binary");
        let alternate = temporary.path().join("leantoken");
        std::fs::copy(&original, &alternate).expect("copy same-version ELF artifact");
        // ELF loaders ignore bytes beyond the loadable segments. This produces
        // two real, independently verified artifacts without another Rust build.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&alternate)
            .expect("open alternate ELF")
            .write_all(b"\nLeanToken same-version runtime regression fixture\n")
            .expect("append harmless ELF overlay");
        let fixture = Self {
            _temporary: temporary,
            home,
            data_home,
            original,
            alternate,
        };
        let first = fixture
            .command(&fixture.original)
            .arg("--version")
            .output()
            .unwrap();
        let second = fixture
            .command(&fixture.alternate)
            .arg("--version")
            .output()
            .unwrap();
        assert!(first.status.success() && second.status.success());
        assert_eq!(first.stdout, second.stdout);
        assert_ne!(
            fixture.digest(&fixture.original),
            fixture.digest(&fixture.alternate)
        );
        fixture
    }

    fn command(&self, executable: &std::path::Path) -> std::process::Command {
        let mut command = std::process::Command::new(executable);
        command
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_DATA_HOME", &self.data_home)
            .env("LOCALAPPDATA", &self.data_home)
            .env_remove("npm_lifecycle_event");
        command
    }

    fn json(&self, executable: &std::path::Path, arguments: &[&str]) -> serde_json::Value {
        let output = self
            .command(executable)
            .arg("--json")
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("runtime command JSON")
    }

    fn digest(&self, executable: &std::path::Path) -> String {
        use std::io::Read;

        let mut executable = std::fs::File::open(executable).expect("runtime executable");
        let mut digest = blake3::Hasher::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let read = executable.read(&mut buffer).expect("read executable");
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        digest.finalize().to_hex().to_string()
    }

    fn runtime_path(report: &serde_json::Value) -> std::path::PathBuf {
        std::path::PathBuf::from(
            report["launcher"]["runtime_path"]
                .as_str()
                .expect("runtime path"),
        )
    }

    fn start(&self, executable: &std::path::Path) -> PrivateRuntimeProcess {
        use command_group::CommandGroup;

        let repository = self.home.join("workspace");
        std::fs::create_dir_all(&repository).expect("runtime workspace");
        std::fs::write(repository.join("lib.rs"), "pub fn leased_runtime() {}\n").unwrap();
        let child = self
            .command(executable)
            .arg("--root")
            .arg(&repository)
            .arg("--database")
            .arg(self.home.join("runtime.sqlite"))
            .arg("mcp")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .group_spawn()
            .expect("start private MCP runtime");
        let mut process = PrivateRuntimeProcess { child };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let inventory = self.json(&self.original, &["runtime", "list"]);
            if inventory["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| {
                    entry["path"] == executable.to_str().unwrap() && entry["active"] == true
                })
            {
                break;
            }
            assert!(
                process.child.try_wait().unwrap().is_none(),
                "private runtime exited before obtaining its lease"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "private runtime did not obtain its lease"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        process
    }
}

#[cfg(target_os = "linux")]
struct PrivateRuntimeProcess {
    child: command_group::GroupChild,
}

#[cfg(target_os = "linux")]
impl PrivateRuntimeProcess {
    fn finish(mut self, crash: bool) {
        if crash {
            self.child.kill().expect("kill private runtime fixture");
        } else {
            drop(self.child.inner().stdin.take());
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    crash || status.success(),
                    "private runtime failed: {status}"
                );
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "private runtime did not exit"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for PrivateRuntimeProcess {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn private_runtime_same_version_variants_refresh_and_active_pruning_are_exact() {
    let fixture = PrivateRuntimeFixture::new();
    let first = fixture.json(
        &fixture.original,
        &["setup", "--claude", "--codex", "--private-runtime", "--yes"],
    );
    let first_path = PrivateRuntimeFixture::runtime_path(&first);
    let second = fixture.json(
        &fixture.alternate,
        &["setup", "--codex", "--private-runtime", "--yes"],
    );
    let second_path = PrivateRuntimeFixture::runtime_path(&second);
    assert_ne!(first_path, second_path);
    assert_eq!(first["launcher"]["version"], second["launcher"]["version"]);
    for (report, path) in [(&first, &first_path), (&second, &second_path)] {
        assert_eq!(report["verification"]["status"], "passed");
        let digest = fixture.digest(path);
        assert_eq!(report["launcher"]["runtime_digest"], digest);
        assert_eq!(
            path.parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            format!("{}_{digest}", env!("CARGO_PKG_VERSION"))
        );
    }
    assert!(
        std::fs::read_to_string(fixture.home.join(".claude.json"))
            .unwrap()
            .contains(first_path.to_str().unwrap())
    );
    let repeated = fixture.json(
        &fixture.alternate,
        &["setup", "--codex", "--private-runtime", "--yes"],
    );
    assert_eq!(PrivateRuntimeFixture::runtime_path(&repeated), second_path);
    assert_eq!(repeated["plan"][0]["action"], "already_current");
    let listed = fixture.json(&fixture.original, &["runtime", "list"]);
    let entries = listed["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry["legacy_layout"] == false));
    assert!(entries[0]["digest"].as_str().unwrap() < entries[1]["digest"].as_str().unwrap());
    let protected = fixture.json(
        &fixture.alternate,
        &["runtime", "prune", "--keep-latest", "0", "--yes"],
    );
    assert!(
        protected["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| result["reason"] == "referenced_by_client")
    );

    let active = fixture.start(&first_path);
    let refreshed = fixture.json(
        &fixture.alternate,
        &["setup", "--refresh", "--private-runtime", "--yes"],
    );
    assert_eq!(PrivateRuntimeFixture::runtime_path(&refreshed), second_path);
    assert_eq!(refreshed["verification"]["status"], "passed");
    assert!(
        std::fs::read_to_string(fixture.home.join(".claude.json"))
            .unwrap()
            .contains(second_path.to_str().unwrap())
    );
    let pruned = fixture.json(
        &fixture.alternate,
        &["runtime", "prune", "--keep-latest", "0", "--yes"],
    );
    assert!(pruned["results"].as_array().unwrap().iter().any(|result| {
        result["path"] == first_path.to_str().unwrap() && result["reason"] == "active_process"
    }));
    assert!(first_path.exists() && second_path.exists());
    active.finish(true);
    fixture.json(
        &fixture.alternate,
        &["runtime", "prune", "--keep-latest", "0", "--yes"],
    );
    assert!(!first_path.exists() && second_path.exists());

    let active = fixture.start(&second_path);
    fixture.json(
        &fixture.alternate,
        &["remove", "--claude", "--codex", "--yes"],
    );
    let pruned = fixture.json(
        &fixture.alternate,
        &["runtime", "prune", "--keep-latest", "0", "--yes"],
    );
    assert_eq!(pruned["results"][0]["reason"], "active_process");
    active.finish(false);
    fixture.json(
        &fixture.alternate,
        &["runtime", "prune", "--keep-latest", "0", "--yes"],
    );
    assert!(!second_path.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn private_runtime_concurrent_same_version_installations_preserve_both_artifacts() {
    let fixture = PrivateRuntimeFixture::new();
    let spawn = |executable: &std::path::Path, client: &str| {
        fixture
            .command(executable)
            .args(["--json", "setup", client, "--private-runtime", "--yes"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("concurrent private runtime setup")
    };
    let first = spawn(&fixture.original, "--claude");
    let second = spawn(&fixture.alternate, "--codex");
    for child in [first, second] {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["verification"]["status"], "passed");
    }
    let listed = fixture.json(&fixture.original, &["runtime", "list"]);
    let entries = listed["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|entry| entry["referenced_by"].as_array().unwrap().len() == 1)
    );
    for entry in entries {
        assert_eq!(
            entry["digest"],
            fixture.digest(std::path::Path::new(entry["path"].as_str().unwrap()))
        );
    }
    fixture.json(
        &fixture.alternate,
        &["remove", "--claude", "--codex", "--yes"],
    );
    let planned = fixture.json(
        &fixture.alternate,
        &["runtime", "prune", "--keep-latest", "1"],
    );
    assert_eq!(planned["results"][0]["digest"], entries[0]["digest"]);
    assert_eq!(planned["results"][0]["reason"], "retention");
    assert_eq!(planned["results"][1]["action"], "would_remove");
    fixture.json(
        &fixture.alternate,
        &["runtime", "prune", "--keep-latest", "1", "--yes"],
    );
    assert!(std::path::Path::new(entries[0]["path"].as_str().unwrap()).exists());
    assert!(!std::path::Path::new(entries[1]["path"].as_str().unwrap()).exists());
}

#[cfg(target_os = "linux")]
#[test]
fn private_runtime_legacy_migration_preserves_referenced_paths_and_variant_retention() {
    let fixture = PrivateRuntimeFixture::new();
    let listed = fixture.json(&fixture.original, &["runtime", "list"]);
    let runtime_root = std::path::PathBuf::from(listed["runtime_root"].as_str().unwrap());
    let legacy = runtime_root
        .join(env!("CARGO_PKG_VERSION"))
        .join("leantoken");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::copy(&fixture.original, &legacy).unwrap();
    let config = serde_json::json!({"mcpServers":{"leantoken":{
        "command":legacy,"args":["--managed-by-setup","mcp"]
    }}})
    .to_string();
    std::fs::write(fixture.home.join(".claude.json"), &config).unwrap();

    let setup = fixture.json(
        &fixture.alternate,
        &["setup", "--codex", "--private-runtime", "--yes"],
    );
    let current = PrivateRuntimeFixture::runtime_path(&setup);
    assert_eq!(
        std::fs::read_to_string(fixture.home.join(".claude.json")).unwrap(),
        config
    );
    let listed = fixture.json(&fixture.original, &["runtime", "list"]);
    let entries = listed["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let legacy_entry = entries
        .iter()
        .find(|entry| entry["legacy_layout"] == true)
        .unwrap();
    assert_eq!(legacy_entry["path"], legacy.to_str().unwrap());
    assert_eq!(legacy_entry["digest"], fixture.digest(&legacy));
    let active = fixture.start(&legacy);
    fixture.json(
        &fixture.alternate,
        &["setup", "--refresh", "--private-runtime", "--yes"],
    );
    let pruned = fixture.json(
        &fixture.alternate,
        &["runtime", "prune", "--keep-latest", "0", "--yes"],
    );
    assert!(
        pruned["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == legacy.to_str().unwrap()
                && entry["reason"] == "active_process")
    );
    active.finish(false);
    let retained = fixture.json(
        &fixture.alternate,
        &["runtime", "prune", "--keep-latest", "1", "--yes"],
    );
    assert!(
        retained["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |entry| entry["path"] == legacy.to_str().unwrap() && entry["reason"] == "retention"
            )
    );
    assert!(legacy.exists() && current.exists());
    fixture.json(
        &fixture.alternate,
        &["runtime", "prune", "--keep-latest", "0", "--yes"],
    );
    assert!(!legacy.exists() && current.exists());
}

#[test]
fn npx_setup_explains_that_it_does_not_install_a_global_cli() {
    let temp = tempfile::tempdir().expect("temporary home");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("npm_lifecycle_event", "npx")
        .env("npm_node_execpath", temp.path().join("node"))
        .env("npm_execpath", temp.path().join("npm-cli.js"))
        .args(["setup", "--codex", "--yes"])
        .output()
        .expect("run npx setup");

    assert!(
        output.status.success(),
        "ambient lifecycle metadata must not break a persistent executable"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("LeanToken // Context Distillery"));
    assert!(stdout.contains("LeanToken is configured for 1 client."));
    assert!(stdout.contains("✓ Exact launcher verified: initialize, 9-tool catalog"));
    assert!(stdout.contains(
        "Verify the stored Codex launcher from a repository: leantoken doctor --client codex"
    ));
    assert!(stdout.contains("Update later with: leantoken upgrade"));
    assert!(!stdout.contains("Some selected clients failed"));
    assert!(!stdout.contains("Launcher verification failed"));
}

fn assert_human_runtime_list(output: &[u8]) {
    let output = String::from_utf8_lossy(output);
    assert!(output.contains("Private runtime root:"));
    assert!(output.contains("4 runtime(s)"));
    assert!(output.contains("referenced_by=Claude Code"));
    assert!(output.contains("inactive,unrecognized"));
}

#[cfg(not(windows))]
fn assert_human_runtime_prune(output: &[u8]) {
    let output = String::from_utf8_lossy(output);
    assert!(output.contains("Private runtime prune dry-run:"));
    assert!(output.contains("would_remove  1.0.0  3 bytes  outside_retention"));
    assert!(output.contains("retained  0.9.0  6 bytes  unrecognized_directory_contents"));
}

#[cfg(not(windows))]
fn assert_runtime_inventory(listed: &serde_json::Value) {
    assert_eq!(listed["total_entries"], 4);
    let entries = listed["entries"].as_array().expect("runtime entries");
    let referenced_entry = entries
        .iter()
        .find(|entry| entry["version"] == "1.1.0")
        .expect("referenced runtime");
    assert_eq!(
        referenced_entry["referenced_by"],
        serde_json::json!(["claude"])
    );
    let unsafe_entry = entries
        .iter()
        .find(|entry| entry["version"] == "0.9.0")
        .expect("unsafe runtime");
    assert_eq!(unsafe_entry["safely_prunable"], false);
}

#[cfg(not(windows))]
fn assert_runtime_prune_decisions(planned: &serde_json::Value) {
    let results = planned["results"].as_array().expect("prune decisions");
    assert!(results.iter().any(|result| {
        result["version"] == "1.1.0"
            && result["action"] == "retained"
            && result["reason"] == "referenced_by_client"
    }));
    assert!(results.iter().any(|result| {
        result["version"] == "0.9.0"
            && result["action"] == "retained"
            && result["reason"] == "unrecognized_directory_contents"
    }));
}

#[cfg(unix)]
#[test]
fn runtime_commands_refuse_a_symlinked_runtime_root_without_mutation() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary home");
    let data_home = temp.path().join("data");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("XDG_DATA_HOME", &data_home)
            .env_remove("npm_lifecycle_event");
        command
    };
    let initial = command()
        .args(["--json", "runtime", "list"])
        .output()
        .expect("initial runtime list");
    assert!(initial.status.success());
    let initial: serde_json::Value =
        serde_json::from_slice(&initial.stdout).expect("initial runtime JSON");
    let runtime_root =
        std::path::PathBuf::from(initial["runtime_root"].as_str().expect("runtime root"));
    let external = temp.path().join("external-runtimes");
    let version = external.join("1.0.0");
    std::fs::create_dir_all(&version).expect("external runtime directory");
    let executable = version.join("leantoken");
    std::fs::write(&executable, "external").expect("external runtime");
    std::fs::create_dir_all(runtime_root.parent().expect("runtime root parent"))
        .expect("runtime parent");
    symlink(&external, &runtime_root).expect("symlink runtime root");

    let list = command()
        .args(["runtime", "list"])
        .output()
        .expect("list symlinked runtime root");
    assert!(!list.status.success());
    assert!(String::from_utf8_lossy(&list.stderr).contains("non-symlink directory"));
    let prune = command()
        .args(["runtime", "prune", "--keep-latest", "0", "--yes"])
        .output()
        .expect("prune symlinked runtime root");
    assert!(!prune.status.success());
    assert!(String::from_utf8_lossy(&prune.stderr).contains("non-symlink directory"));
    let setup = command()
        .args(["setup", "--codex", "--private-runtime", "--yes"])
        .output()
        .expect("setup through symlinked runtime root");
    assert!(!setup.status.success());
    assert!(String::from_utf8_lossy(&setup.stderr).contains("non-symlink directory"));
    assert!(executable.exists());
    assert!(!external.join("setup.lock").exists());
    assert!(!temp.path().join(".codex/config.toml").exists());
}

#[cfg(unix)]
#[test]
fn runtime_commands_reject_unsafe_artifact_leases_without_deleting_runtime_or_external_files() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary home");
    let data_home = temp.path().join("data");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("XDG_DATA_HOME", &data_home)
            .env_remove("npm_lifecycle_event");
        command
    };
    let initial = command()
        .args(["--json", "runtime", "list"])
        .output()
        .unwrap();
    assert!(initial.status.success());
    let initial: serde_json::Value = serde_json::from_slice(&initial.stdout).unwrap();
    let runtime_root = std::path::PathBuf::from(initial["runtime_root"].as_str().unwrap());
    let artifact = runtime_root.join("1.2.3");
    std::fs::create_dir_all(&artifact).unwrap();
    let executable = artifact.join("leantoken");
    std::fs::write(&executable, "legacy runtime").unwrap();
    let leases = runtime_root.join(".leases");
    std::fs::create_dir(&leases).unwrap();
    let lease = leases.join(format!("{}.lock", blake3::hash(b"1.2.3").to_hex()));
    let external = temp.path().join("external.lock");
    std::fs::write(&external, []).unwrap();
    for hard_link in [false, true] {
        if hard_link {
            std::fs::hard_link(&external, &lease).unwrap();
        } else {
            symlink(&external, &lease).unwrap();
        }
        for arguments in [
            vec!["--json", "runtime", "list"],
            vec!["--json", "runtime", "prune", "--keep-latest", "0", "--yes"],
        ] {
            let output = command().args(arguments).output().unwrap();
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("lease must be an empty non-symlink file with one link")
            );
            assert!(executable.exists());
            assert_eq!(std::fs::read(&external).unwrap(), b"");
        }
        std::fs::remove_file(&lease).unwrap();
    }
    std::fs::write(&lease, []).unwrap();
    let unrecognized = leases.join("unrecognized-user-content");
    std::fs::write(&unrecognized, "retain").unwrap();
    let output = command()
        .args(["runtime", "prune", "--keep-latest", "0", "--yes"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!executable.exists());
    assert!(lease.exists());
    assert_eq!(std::fs::read(unrecognized).unwrap(), b"retain");
}

#[cfg(not(windows))]
#[test]
fn runtime_list_and_prune_are_bounded_reference_safe_and_dry_run_by_default() {
    let temp = tempfile::tempdir().expect("temporary home");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("XDG_DATA_HOME", temp.path().join("xdg-data"))
            .env_remove("npm_lifecycle_event");
        command
    };
    let initial = command()
        .args(["--json", "runtime", "list"])
        .output()
        .expect("initial runtime list");
    assert!(initial.status.success());
    let initial: serde_json::Value =
        serde_json::from_slice(&initial.stdout).expect("initial runtime JSON");
    let runtime_root =
        std::path::PathBuf::from(initial["runtime_root"].as_str().expect("runtime root"));
    let executable_name = if cfg!(windows) {
        "leantoken.exe"
    } else {
        "leantoken"
    };
    let runtime = |version: &str, bytes: &[u8]| {
        let directory = runtime_root.join(version);
        std::fs::create_dir_all(&directory).expect("runtime directory");
        let executable = directory.join(executable_name);
        std::fs::write(&executable, bytes).expect("runtime executable");
        executable
    };
    let oldest = runtime("1.0.0", b"old");
    let referenced = runtime("1.1.0", b"referenced");
    let newest = runtime("1.2.0", b"newest");
    let unsafe_runtime = runtime("0.9.0", b"unsafe");
    std::fs::write(unsafe_runtime.parent().unwrap().join("notes.txt"), b"keep")
        .expect("unrecognized sibling");
    let referenced_alias = referenced
        .parent()
        .unwrap()
        .join("..")
        .join("1.1.0")
        .join(executable_name);
    assert_eq!(
        referenced_alias.canonicalize().unwrap(),
        referenced.canonicalize().unwrap()
    );
    let claude = serde_json::json!({"mcpServers": {"leantoken": {
        "command": referenced_alias, "args": ["--managed-by-setup", "mcp"]
    }}});
    std::fs::write(
        temp.path().join(".claude.json"),
        serde_json::to_vec(&claude).unwrap(),
    )
    .expect("Claude config");

    let listed = command()
        .args(["--json", "runtime", "list"])
        .output()
        .expect("runtime list");
    assert!(listed.status.success());
    let listed: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("runtime list JSON");
    assert_runtime_inventory(&listed);
    let human_list = command()
        .args(["runtime", "list"])
        .output()
        .expect("human runtime list");
    assert!(human_list.status.success());
    assert_human_runtime_list(&human_list.stdout);

    let planned = command()
        .args(["--json", "runtime", "prune", "--keep-latest", "0"])
        .output()
        .expect("runtime prune plan");
    assert!(planned.status.success());
    let planned: serde_json::Value =
        serde_json::from_slice(&planned.stdout).expect("runtime prune JSON");
    assert_eq!(planned["dry_run"], true);
    assert!(oldest.exists() && newest.exists() && referenced.exists() && unsafe_runtime.exists());
    assert_runtime_prune_decisions(&planned);
    let human_plan = command()
        .args(["runtime", "prune", "--keep-latest", "0"])
        .output()
        .expect("human runtime prune plan");
    assert!(human_plan.status.success());
    assert_human_runtime_prune(&human_plan.stdout);

    let applied = command()
        .args(["--json", "runtime", "prune", "--keep-latest", "0", "--yes"])
        .output()
        .expect("apply runtime prune");
    assert!(
        applied.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(!oldest.exists());
    assert!(!newest.exists());
    assert!(referenced.exists());
    assert!(unsafe_runtime.exists());
    let final_list = command()
        .args(["--json", "runtime", "list"])
        .output()
        .expect("final runtime list");
    assert!(final_list.status.success());
    let final_list: serde_json::Value =
        serde_json::from_slice(&final_list.stdout).expect("final runtime JSON");
    assert_eq!(final_list["ignored_entries"], 0);

    std::fs::create_dir_all(temp.path().join(".cursor")).expect("Cursor config directory");
    let oversized = std::fs::File::create(temp.path().join(".cursor/mcp.json"))
        .expect("oversized Cursor config");
    oversized
        .set_len(8 * 1024 * 1024 + 1)
        .expect("extend Cursor config");
    let bounded = command()
        .args(["runtime", "list"])
        .output()
        .expect("bounded runtime list");
    assert!(!bounded.status.success());
    assert!(String::from_utf8_lossy(&bounded.stderr).contains("byte limit"));
}
