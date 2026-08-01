use super::*;

#[test]
fn runtime_root_falls_back_below_the_resolved_home() {
    assert_eq!(
        setup_runtime_root_from(Path::new("/home/agent"), None),
        Path::new("/home/agent/.local/share/leantoken/runtimes")
    );
}

#[test]
fn setup_file_reads_reject_content_above_the_memory_bound() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("oversized.json");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_SETUP_FILE_BYTES + 1).unwrap();

    let error = read_optional(&path).expect_err("oversized setup file must fail closed");
    assert!(error.to_string().contains("byte limit"));
}

#[test]
fn setup_rejects_generated_client_configuration_above_the_read_bound() {
    let temp = tempfile::tempdir().unwrap();
    let environment = environment(&temp);
    let path = environment.home.join(".cursor/mcp.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let prefix = "{\"padding\":\"";
    let suffix = "\"}";
    let padding = "x".repeat(MAX_SETUP_FILE_BYTES as usize - prefix.len() - suffix.len() - 1);
    fs::write(&path, format!("{prefix}{padding}{suffix}")).unwrap();

    let error = run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: vec![SetupClient::Cursor],
            all: false,
            refresh: false,
            private_runtime: false,
            yes: true,
            dry_run: true,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .expect_err("generated oversized config must fail during planning");

    assert!(error.to_string().contains("refusing to write setup file"));
    assert!(fs::metadata(path).unwrap().len() < MAX_SETUP_FILE_BYTES);
}

#[test]
fn failed_launcher_verification_marks_setup_report_failed() {
    let mut report = empty_report(SetupOperation::Setup, true);
    report.cancelled = false;
    report.verification = Some(SetupVerification {
        status: SetupVerificationStatus::Failed,
        stage: Some("handshake".into()),
        message: Some("launcher closed".into()),
        repair_command: Some("leantoken doctor --json".into()),
    });

    assert!(report.has_failures());
}

struct FixedPrompt {
    selected: Option<Vec<SetupClient>>,
    confirmed: bool,
}

impl SetupPrompt for FixedPrompt {
    fn select(
        &self,
        _operation: SetupOperation,
        _detected: &[SetupClient],
        _preferred: &[SetupClient],
    ) -> Result<Option<Vec<SetupClient>>> {
        Ok(self.selected.clone())
    }

    fn confirm(&self, _operation: SetupOperation, _plan: &ResolvedSetupPlan) -> Result<bool> {
        Ok(self.confirmed)
    }
}

fn environment(temp: &tempfile::TempDir) -> SetupEnvironment {
    SetupEnvironment {
        home: temp.path().join("home"),
        runtime_root: temp.path().join("runtime"),
        native_executable: temp.path().join("bin/lean token"),
        launcher: McpLauncher::from_executable(&temp.path().join("bin/lean token")),
        interactive: true,
        persistent_cli: true,
    }
}

fn npx_environment(temp: &tempfile::TempDir, version: &str) -> SetupEnvironment {
    let runtime = temp.path().join("node runtime");
    SetupEnvironment {
        home: temp.path().join("home"),
        runtime_root: temp.path().join("runtime"),
        native_executable: temp.path().join("native/leantoken"),
        launcher: McpLauncher::from_npx_paths_with_version(
            &runtime.join(if cfg!(windows) { "node.exe" } else { "node" }),
            &runtime.join("npx cli.js"),
            version,
        )
        .unwrap(),
        interactive: false,
        persistent_cli: false,
    }
}

#[test]
fn local_npx_setup_stops_before_persisting_a_stale_release() {
    let root = if cfg!(windows) {
        PathBuf::from(r"C:\project")
    } else {
        PathBuf::from("/project")
    };
    let executable = root
        .join("node_modules/leantoken/native/target")
        .join(if cfg!(windows) {
            "leantoken.exe"
        } else {
            "leantoken"
        });

    assert!(npx_resolved_from_local_project(
        &executable,
        &root.join("nested/workspace")
    ));
    let error = require_current_npx_setup("0.1.1", Some("0.1.13")).expect_err("stale release");
    assert!(
        error
            .to_string()
            .contains("npx --yes leantoken@latest setup")
    );
    assert!(require_current_npx_setup("0.1.13", Some("0.1.13")).is_ok());
}

#[test]
fn local_npx_setup_requires_an_explicit_override_when_freshness_is_unknown() {
    let error = require_current_npx_setup("0.1.13", None).expect_err("unknown latest");

    assert!(error.to_string().contains("--allow-outdated"));
}

#[test]
fn json_setup_preserves_comments_and_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("mcp.json");
    fs::write(
            &path,
            "{\n  // keep me\n  \"theme\": \"dark\",\n  \"mcpServers\": {\n    \"other\": { \"command\": \"other\" },\n  },\n}\n",
        )
        .unwrap();
    let launcher = McpLauncher::from_executable(&temp.path().join("bin/léan token"));

    let first = edit_json_config(
        SetupOperation::Setup,
        &path,
        "mcpServers",
        JsonEntryShape::CommandAndArgs,
        &launcher,
    )
    .unwrap();
    assert!(matches!(first, EditStatus::Configured));
    let configured = fs::read_to_string(&path).unwrap();
    assert!(configured.contains("// keep me"));
    assert!(configured.contains("\"other\""));
    assert!(configured.contains("léan token"));

    let second = edit_json_config(
        SetupOperation::Setup,
        &path,
        "mcpServers",
        JsonEntryShape::CommandAndArgs,
        &launcher,
    )
    .unwrap();
    assert!(matches!(second, EditStatus::AlreadyConfigured));
    assert_eq!(fs::read_to_string(path).unwrap(), configured);
}

#[test]
fn json_remove_preserves_sibling_server_and_prunes_empty_section() {
    let temp = tempfile::tempdir().unwrap();
    let launcher = McpLauncher::from_executable(&temp.path().join("leantoken"));
    let with_sibling = temp.path().join("with-sibling.json");
    fs::write(
        &with_sibling,
        "{\n  \"mcpServers\": {\n    \"leantoken\": {},\n    \"other\": {}\n  }\n}\n",
    )
    .unwrap();
    edit_json_config(
        SetupOperation::Remove,
        &with_sibling,
        "mcpServers",
        JsonEntryShape::CommandAndArgs,
        &launcher,
    )
    .unwrap();
    let contents = fs::read_to_string(with_sibling).unwrap();
    assert!(!contents.contains("leantoken"));
    assert!(contents.contains("other"));

    let only = temp.path().join("only.json");
    fs::write(
        &only,
        "{\n  \"mcpServers\": { \"leantoken\": {} },\n  \"x\": 1\n}\n",
    )
    .unwrap();
    edit_json_config(
        SetupOperation::Remove,
        &only,
        "mcpServers",
        JsonEntryShape::CommandAndArgs,
        &launcher,
    )
    .unwrap();
    let contents = fs::read_to_string(only).unwrap();
    assert!(!contents.contains("mcpServers"));
    assert!(contents.contains("\"x\": 1"));
}

#[cfg(unix)]
#[test]
fn json_remove_does_not_require_a_utf8_executable_path() {
    use std::os::unix::ffi::OsStringExt;

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.json");
    fs::write(
        &path,
        "{\"mcpServers\":{\"leantoken\":{\"command\":\"old\",\"args\":[\"mcp\"]}}}\n",
    )
    .unwrap();
    let executable = PathBuf::from(std::ffi::OsString::from_vec(vec![b'l', 0x80]));
    let launcher = McpLauncher::from_executable(&executable);

    assert!(matches!(
        edit_json_config(
            SetupOperation::Remove,
            &path,
            "mcpServers",
            JsonEntryShape::CommandAndArgs,
            &launcher,
        )
        .unwrap(),
        EditStatus::Removed
    ));
    assert_eq!(fs::read_to_string(path).unwrap(), "{}\n");
}

#[test]
fn toml_setup_and_remove_preserve_unrelated_content() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    fs::write(
        &path,
        "# keep me\nmodel = \"test\"\n\n[mcp_servers.other]\ncommand = \"other\"\n",
    )
    .unwrap();
    let launcher = McpLauncher::from_executable(&temp.path().join("bin/leantoken"));
    edit_toml_config(SetupOperation::Setup, &path, &launcher).unwrap();
    let configured = fs::read_to_string(&path).unwrap();
    assert!(configured.contains("# keep me"));
    assert!(configured.contains("[mcp_servers.other]"));
    assert!(configured.contains("[mcp_servers.leantoken]"));
    assert!(matches!(
        edit_toml_config(SetupOperation::Setup, &path, &launcher).unwrap(),
        EditStatus::AlreadyConfigured
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), configured);

    edit_toml_config(SetupOperation::Remove, &path, &launcher).unwrap();
    let removed = fs::read_to_string(path).unwrap();
    assert!(removed.contains("# keep me"));
    assert!(removed.contains("[mcp_servers.other]"));
    assert!(!removed.contains("[mcp_servers.leantoken]"));
}

#[test]
fn malformed_config_is_never_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("broken.json");
    let original = "{ nope";
    fs::write(&path, original).unwrap();
    assert!(
        edit_json_config(
            SetupOperation::Setup,
            &path,
            "mcpServers",
            JsonEntryShape::CommandAndArgs,
            &McpLauncher::from_executable(&temp.path().join("leantoken")),
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(path).unwrap(), original);
}

#[test]
fn interactive_selection_can_cancel_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    let report = run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: Vec::new(),
            all: false,
            refresh: false,
            private_runtime: false,
            yes: false,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment(&temp),
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap();
    assert!(report.cancelled);
    assert!(!temp.path().join("home").exists());
}

#[test]
fn yes_requires_explicit_clients_even_when_a_client_is_detected() {
    let temp = tempfile::tempdir().unwrap();
    let environment = environment(&temp);
    fs::create_dir_all(environment.home.join(".codex")).unwrap();
    let error = run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: Vec::new(),
            all: false,
            refresh: false,
            private_runtime: false,
            yes: true,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("detection is not consent"));
    assert!(!environment.home.join(".codex/config.toml").exists());
}

#[test]
fn all_clients_receive_global_entries_and_second_setup_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let environment = environment(&temp);
    let request = SetupRequest {
        clients: Vec::new(),
        all: true,
        refresh: false,
        private_runtime: false,
        yes: true,
        dry_run: false,
        allow_outdated: false,
        force_unmanaged: false,
    };
    let first = run_with(
        SetupOperation::Setup,
        request.clone(),
        &environment,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap();
    assert_eq!(first.results.len(), SetupClient::ALL.len());
    assert!(first.results.iter().all(|result| result.error.is_none()));
    assert_eq!(
        first.verification.as_ref().map(|result| result.status),
        Some(SetupVerificationStatus::Failed)
    );

    let home = &environment.home;
    for path in [
        home.join(".claude.json"),
        home.join(".cursor/mcp.json"),
        home.join(".config/opencode/opencode.json"),
        home.join(".codex/config.toml"),
        home.join(".gemini/settings.json"),
        home.join(".gemini/config/mcp_config.json"),
    ] {
        assert!(path.exists(), "missing {}", path.display());
    }
    let opencode = fs::read_to_string(home.join(".config/opencode/opencode.json")).unwrap();
    assert!(opencode.contains("\"type\": \"local\""));
    assert!(opencode.contains("\"cwd\": \".\""));
    assert!(opencode.contains("\"enabled\": true"));

    let before = first
        .results
        .iter()
        .map(|result| {
            (
                result.path.clone(),
                fs::read_to_string(&result.path).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let second = run_with(
        SetupOperation::Setup,
        request,
        &environment,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap();
    assert!(
        second
            .results
            .iter()
            .all(|result| result.status == "already configured")
    );
    for (path, contents) in before {
        assert_eq!(fs::read_to_string(path).unwrap(), contents);
    }

    let refreshed = run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: Vec::new(),
            all: false,
            refresh: true,
            private_runtime: false,
            yes: true,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap();
    assert_eq!(refreshed.results.len(), SetupClient::ALL.len());
    assert!(
        refreshed
            .results
            .iter()
            .all(|result| result.status == "already configured")
    );
}

#[test]
fn diagnostic_reports_configured_command_and_stale_release() {
    let temp = tempfile::tempdir().unwrap();
    let original = npx_environment(&temp, "1.2.3");
    run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: vec![SetupClient::Claude, SetupClient::Codex],
            all: false,
            refresh: false,
            private_runtime: false,
            yes: true,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &original,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap();

    let current = npx_environment(&temp, "2.0.0");
    let registrations = configured_registrations(&current.home, &current.launcher).unwrap();
    assert_eq!(registrations.len(), 2);
    assert!(registrations.iter().all(|registration| {
        registration.version.as_deref() == Some("1.2.3")
            && registration.expected_version == "2.0.0"
            && !registration.matches_current
            && registration
                .args
                .iter()
                .any(|argument| argument == "--package=leantoken@1.2.3")
    }));
}

#[test]
fn diagnostic_preserves_unknown_discovery_state_after_config_parse_failure() {
    let temp = tempfile::tempdir().unwrap();
    let environment = environment(&temp);
    let config = environment.home.join(".codex/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(config, "[broken").unwrap();
    let skill = environment.home.join(".agents/skills/leantoken/SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::write(&skill, format!("{DISCOVERY_SKILL_MARKER}\n")).unwrap();

    let diagnostic = diagnostic_state_at(
        &environment.home,
        Some((
            environment.launcher.command().unwrap(),
            &environment.launcher.args,
            environment.launcher.version(),
        )),
    );

    assert_eq!(diagnostic.registration_status, "unknown");
    assert_eq!(diagnostic.discovery_status, "unknown");
    assert_eq!(diagnostic.discovery_paths, vec![skill]);
}

#[test]
fn malformed_client_blocks_the_entire_plan_before_writes() {
    let temp = tempfile::tempdir().unwrap();
    let environment = environment(&temp);
    fs::create_dir_all(&environment.home).unwrap();
    fs::write(environment.home.join(".claude.json"), "{ broken").unwrap();
    let error = run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: vec![SetupClient::Claude, SetupClient::Cursor],
            all: false,
            refresh: false,
            private_runtime: false,
            yes: true,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("refusing to overwrite malformed config")
    );
    assert_eq!(
        fs::read_to_string(environment.home.join(".claude.json")).unwrap(),
        "{ broken"
    );
    assert!(!environment.home.join(".cursor/mcp.json").exists());
}

#[test]
fn non_interactive_explicit_selection_requires_yes() {
    let temp = tempfile::tempdir().unwrap();
    let mut environment = environment(&temp);
    environment.interactive = false;
    let error = run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: vec![SetupClient::Codex],
            all: false,
            refresh: false,
            private_runtime: false,
            yes: false,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("non-interactive setup requires"));
    assert!(!environment.home.join(".codex/config.toml").exists());
}

#[test]
fn dry_run_resolves_exact_plan_without_writes_or_yes() {
    let temp = tempfile::tempdir().unwrap();
    let mut environment = environment(&temp);
    environment.interactive = false;
    let report = run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: vec![SetupClient::Codex],
            all: false,
            refresh: false,
            private_runtime: false,
            yes: false,
            dry_run: true,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap();
    assert!(report.dry_run);
    assert_eq!(report.plan[0].action, ClientPlanAction::Create);
    assert!(report.results.is_empty());
    assert!(!environment.home.join(".codex/config.toml").exists());
}

#[test]
fn explicit_interactive_selection_still_requires_confirmation() {
    let temp = tempfile::tempdir().unwrap();
    let environment = environment(&temp);
    let report = run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: vec![SetupClient::Codex],
            all: false,
            refresh: false,
            private_runtime: false,
            yes: false,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &FixedPrompt {
            selected: None,
            confirmed: false,
        },
    )
    .unwrap();
    assert!(report.cancelled);
    assert!(!environment.home.join(".codex/config.toml").exists());
}

#[test]
fn refresh_updates_only_existing_entries_and_supports_rollback() {
    let temp = tempfile::tempdir().unwrap();
    let original = npx_environment(&temp, "1.2.3");
    fs::create_dir_all(original.home.join(".cursor")).unwrap();
    fs::write(
        original.home.join(".cursor/mcp.json"),
        "{\"mcpServers\":{\"other\":{\"command\":\"other\"}}}\n",
    )
    .unwrap();
    run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: vec![SetupClient::Claude, SetupClient::Codex],
            all: false,
            refresh: false,
            private_runtime: false,
            yes: true,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &original,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap();

    let upgraded = npx_environment(&temp, "2.0.0");
    let refresh = SetupRequest {
        clients: Vec::new(),
        all: false,
        refresh: true,
        private_runtime: false,
        yes: true,
        dry_run: false,
        allow_outdated: false,
        force_unmanaged: false,
    };
    let report = run_with(
        SetupOperation::Setup,
        refresh.clone(),
        &upgraded,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap();
    assert_eq!(report.results.len(), 2);
    assert!(
        report
            .results
            .iter()
            .all(|result| result.status == "updated")
    );
    assert_eq!(report.launcher.unwrap().version, "2.0.0");
    assert!(
        fs::read_to_string(upgraded.home.join(".claude.json"))
            .unwrap()
            .contains("--package=leantoken@2.0.0")
    );
    assert!(
        fs::read_to_string(upgraded.home.join(".codex/config.toml"))
            .unwrap()
            .contains("--package=leantoken@2.0.0")
    );
    assert!(
        !fs::read_to_string(upgraded.home.join(".cursor/mcp.json"))
            .unwrap()
            .contains("leantoken@")
    );

    let rollback = run_with(
        SetupOperation::Setup,
        refresh,
        &original,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap();
    assert_eq!(rollback.results.len(), 2);
    assert!(
        fs::read_to_string(original.home.join(".claude.json"))
            .unwrap()
            .contains("--package=leantoken@1.2.3")
    );
}

#[test]
fn refresh_does_not_create_entries_or_fall_back_to_latest_without_an_npm_cache() {
    let temp = tempfile::tempdir().unwrap();
    let environment = npx_environment(&temp, "1.2.3");
    fs::create_dir_all(&environment.home).unwrap();

    let report = run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: Vec::new(),
            all: false,
            refresh: true,
            private_runtime: false,
            yes: true,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap();

    assert!(report.results.is_empty());
    assert_eq!(
        report.launcher.unwrap().package.as_deref(),
        Some("leantoken@1.2.3")
    );
    assert!(!environment.home.join(".claude.json").exists());
    assert!(
        environment
            .launcher
            .args
            .iter()
            .all(|argument| !argument.contains("@latest"))
    );
}

#[test]
fn refresh_rejects_ambiguous_selection_and_remove_usage() {
    let temp = tempfile::tempdir().unwrap();
    let environment = environment(&temp);
    let prompt = FixedPrompt {
        selected: None,
        confirmed: true,
    };
    let ambiguous = SetupRequest {
        clients: vec![SetupClient::Codex],
        all: false,
        refresh: true,
        private_runtime: false,
        yes: true,
        dry_run: false,
        allow_outdated: false,
        force_unmanaged: false,
    };
    assert!(
        run_with(SetupOperation::Setup, ambiguous, &environment, &prompt)
            .unwrap_err()
            .to_string()
            .contains("cannot be combined")
    );
    let remove = SetupRequest {
        clients: Vec::new(),
        all: false,
        refresh: true,
        private_runtime: false,
        yes: true,
        dry_run: false,
        allow_outdated: false,
        force_unmanaged: false,
    };
    assert!(
        run_with(SetupOperation::Remove, remove, &environment, &prompt)
            .unwrap_err()
            .to_string()
            .contains("only valid with the setup command")
    );
}

#[test]
fn private_runtime_dry_run_install_and_remove_are_pinned_and_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let environment = npx_environment(&temp, "1.2.3");
    fs::create_dir_all(environment.native_executable.parent().unwrap()).unwrap();
    fs::write(
        &environment.native_executable,
        b"verified native executable",
    )
    .unwrap();
    let request = SetupRequest {
        clients: vec![SetupClient::Codex],
        all: false,
        refresh: false,
        private_runtime: true,
        yes: true,
        dry_run: true,
        allow_outdated: false,
        force_unmanaged: false,
    };
    let prompt = FixedPrompt {
        selected: None,
        confirmed: true,
    };

    let dry_run = run_with(
        SetupOperation::Setup,
        request.clone(),
        &environment,
        &prompt,
    )
    .unwrap();
    let launcher = dry_run.launcher.expect("launcher plan");
    let runtime_path = launcher.runtime_path.expect("private runtime path");
    assert_eq!(
        runtime_path,
        environment
            .runtime_root
            .join("1.2.3")
            .join(if cfg!(windows) {
                "leantoken.exe"
            } else {
                "leantoken"
            })
    );
    let expected_digest = file_digest(&environment.native_executable).unwrap();
    assert_eq!(
        launcher.runtime_digest.as_deref(),
        Some(expected_digest.as_str())
    );
    assert!(!runtime_path.exists(), "dry-run must not install");

    let mut apply = request;
    apply.dry_run = false;
    let first = run_with(SetupOperation::Setup, apply.clone(), &environment, &prompt).unwrap();
    assert!(first.results.iter().all(|result| result.error.is_none()));
    assert_eq!(
        first.verification.as_ref().map(|result| result.status),
        Some(SetupVerificationStatus::Failed)
    );
    assert_eq!(
        fs::read(&runtime_path).unwrap(),
        b"verified native executable"
    );
    let codex = fs::read_to_string(environment.home.join(".codex/config.toml")).unwrap();
    assert!(codex.contains(runtime_path.to_str().unwrap()));
    assert!(!codex.contains("npm"));

    let second = run_with(SetupOperation::Setup, apply, &environment, &prompt).unwrap();
    assert!(second.results.iter().all(|result| result.error.is_none()));
    assert_eq!(
        second.verification.as_ref().map(|result| result.status),
        Some(SetupVerificationStatus::Failed)
    );
    assert_eq!(second.plan[0].action, ClientPlanAction::AlreadyCurrent);

    let removal = run_with(
        SetupOperation::Remove,
        SetupRequest {
            clients: vec![SetupClient::Codex],
            all: false,
            refresh: false,
            private_runtime: false,
            yes: true,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &prompt,
    )
    .unwrap();
    assert!(!removal.has_failures());
    assert!(runtime_path.exists(), "removal retains versioned runtimes");
}

#[test]
fn private_runtime_uses_native_executable_names_for_supported_package_layouts() {
    for (platform, windows, expected) in [
        ("linux", false, "leantoken"),
        ("macos", false, "leantoken"),
        ("windows", true, "leantoken.exe"),
    ] {
        assert_eq!(runtime_executable_name(windows), expected, "{platform}");
    }
}

#[test]
fn runtime_removal_reports_when_the_executable_was_removed_but_directory_remains() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("1.2.3");
    fs::create_dir(&directory).unwrap();
    let executable = directory.join(runtime_executable_name(cfg!(windows)));
    fs::write(&executable, "runtime").unwrap();
    let sibling = directory.join("appeared-after-revalidation");
    fs::write(&sibling, "retain").unwrap();

    let removal = remove_runtime_directory(&directory, &executable);

    assert!(matches!(removal, RuntimeRemoval::PartiallyRemoved(_)));
    assert!(!executable.exists());
    assert!(sibling.exists());
    assert!(directory.exists());
}

#[test]
fn setup_transaction_rolls_back_earlier_client_edits() {
    let temp = tempfile::tempdir().unwrap();
    let first_path = temp.path().join("first/config.json");
    let blocked_parent = temp.path().join("blocked");
    fs::write(&blocked_parent, "not a directory").unwrap();
    let edits = vec![
        PlannedClientEdit {
            public: ClientSetupPlan {
                client: SetupClient::Claude,
                path: first_path.clone(),
                action: ClientPlanAction::Create,
                detected: true,
            },
            status: EditStatus::Configured,
            original: None,
            updated: Some("{\"mcpServers\":{}}".into()),
        },
        PlannedClientEdit {
            public: ClientSetupPlan {
                client: SetupClient::Cursor,
                path: blocked_parent.join("config.json"),
                action: ClientPlanAction::Create,
                detected: true,
            },
            status: EditStatus::Configured,
            original: None,
            updated: Some("{\"mcpServers\":{}}".into()),
        },
    ];
    let plan = ResolvedSetupPlan {
        operation: SetupOperation::Setup,
        persistent_cli: true,
        launcher: None,
        runtime: None,
        edits,
        discovery_edits: Vec::new(),
        ownership_override: false,
        transaction_root: temp.path().join("runtime"),
    };

    let results = apply_plan(&plan);

    assert!(results.iter().all(|result| result.error.is_some()));
    assert!(!first_path.exists(), "first edit must be rolled back");
    assert_eq!(
        fs::read_to_string(blocked_parent).unwrap(),
        "not a directory"
    );
}

#[test]
fn failed_rollback_retains_recovery_journal() {
    let temp = tempfile::tempdir().unwrap();
    let runtime_root = temp.path().join("runtime");
    let parent = temp.path().join("config");
    let path = parent.join("client.json");
    fs::create_dir(&parent).unwrap();
    fs::write(&path, "old").unwrap();
    let edit = PlannedClientEdit {
        public: ClientSetupPlan {
            client: SetupClient::Codex,
            path: path.clone(),
            action: ClientPlanAction::Update,
            detected: true,
        },
        status: EditStatus::Updated,
        original: Some("old".into()),
        updated: Some("new".into()),
    };
    let plan = ResolvedSetupPlan {
        operation: SetupOperation::Setup,
        persistent_cli: true,
        launcher: None,
        runtime: None,
        edits: vec![edit],
        discovery_edits: Vec::new(),
        ownership_override: false,
        transaction_root: runtime_root.clone(),
    };
    let transaction = begin_setup_transaction(&plan)
        .unwrap()
        .expect("transaction");
    fs::write(&path, "new").unwrap();
    fs::remove_file(&path).unwrap();
    fs::remove_dir(&parent).unwrap();
    fs::write(&parent, "blocks restoration").unwrap();

    let error = rollback_setup(&plan, false, &[&plan.edits[0]], &[], Some(transaction))
        .expect_err("rollback must fail");
    assert!(matches!(error, Error::Io(_)));
    assert!(transaction_path(&runtime_root).exists());
}

#[test]
fn setup_manages_compact_discovery_skills_without_overwriting_unowned_content() {
    let temp = tempfile::tempdir().unwrap();
    let environment = environment(&temp);
    let prompt = FixedPrompt {
        selected: None,
        confirmed: true,
    };
    let request = SetupRequest {
        clients: vec![SetupClient::Codex],
        all: false,
        refresh: false,
        private_runtime: false,
        yes: true,
        dry_run: false,
        allow_outdated: false,
        force_unmanaged: false,
    };

    let report = run_with(
        SetupOperation::Setup,
        request.clone(),
        &environment,
        &prompt,
    )
    .unwrap();
    assert_eq!(report.discovery_plan.len(), 1);
    assert!(
        report
            .discovery_skill_tokens
            .is_some_and(|tokens| tokens > 0)
    );
    for effect in &report.discovery_plan {
        assert_eq!(
            effect.path,
            environment.home.join(".agents/skills/leantoken/SKILL.md")
        );
        let skill = fs::read_to_string(&effect.path).unwrap();
        assert!(skill.contains(DISCOVERY_SKILL_MARKER));
        assert!(skill.contains("leantoken.context"));
        assert!(skill.contains("once with `plan_only=false`"));
        assert!(skill.contains("at most one focused follow-up"));
        assert!(skill.contains("human review or control-plane inspection"));
        assert!(!skill.contains("first set `plan_only=true`"));
        assert!(skill.contains("leantoken.savings"));
        assert!(skill.contains("audits and code archaeology"));
        assert!(skill.contains("runtime probes"));
        assert!(skill.find("leantoken.files").unwrap() < skill.find("leantoken.outline").unwrap());
        assert!(skill.find("leantoken.outline").unwrap() < skill.find("leantoken.read").unwrap());
        assert!(skill.contains("leantoken doctor --json"));
        assert!(!skill.contains("inputSchema"));
        assert_eq!(
            report.discovery_skill_tokens,
            Some(crate::tokens::Tokenizer::Cl100kBase.count(&skill))
        );
    }

    let shared_skill = environment.home.join(".agents/skills/leantoken/SKILL.md");
    fs::write(&shared_skill, "user-owned skill").unwrap();
    let error = run_with(SetupOperation::Setup, request, &environment, &prompt)
        .expect_err("unowned skill must block setup");
    assert!(error.to_string().contains("unowned discovery skill"));
    assert_eq!(
        fs::read_to_string(shared_skill).unwrap(),
        "user-owned skill"
    );
}

#[test]
fn codex_setup_does_not_touch_an_unselected_claude_skill() {
    let temp = tempfile::tempdir().unwrap();
    let environment = environment(&temp);
    let claude_skill = environment.home.join(".claude/skills/leantoken/SKILL.md");
    fs::create_dir_all(claude_skill.parent().unwrap()).unwrap();
    fs::write(&claude_skill, "user-owned Claude skill").unwrap();
    let report = run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: vec![SetupClient::Codex],
            all: false,
            refresh: false,
            private_runtime: false,
            yes: true,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &FixedPrompt {
            selected: None,
            confirmed: true,
        },
    )
    .unwrap();

    assert_eq!(report.discovery_plan.len(), 1);
    assert_eq!(
        fs::read_to_string(claude_skill).unwrap(),
        "user-owned Claude skill"
    );
    assert!(
        environment
            .home
            .join(".agents/skills/leantoken/SKILL.md")
            .exists()
    );
}

#[test]
fn codex_setup_and_refresh_remove_marker_owned_legacy_claude_discovery() {
    let temp = tempfile::tempdir().unwrap();
    let environment = environment(&temp);
    let prompt = FixedPrompt {
        selected: None,
        confirmed: true,
    };
    let request = SetupRequest {
        clients: vec![SetupClient::Codex],
        all: false,
        refresh: false,
        private_runtime: false,
        yes: true,
        dry_run: false,
        allow_outdated: false,
        force_unmanaged: false,
    };
    run_with(
        SetupOperation::Setup,
        request.clone(),
        &environment,
        &prompt,
    )
    .unwrap();
    let agents_skill = environment.home.join(".agents/skills/leantoken/SKILL.md");
    let claude_skill = environment.home.join(".claude/skills/leantoken/SKILL.md");
    fs::create_dir_all(claude_skill.parent().unwrap()).unwrap();
    fs::copy(&agents_skill, &claude_skill).unwrap();

    let setup = run_with(SetupOperation::Setup, request, &environment, &prompt).unwrap();
    assert!(setup.discovery_plan.iter().any(|effect| {
        effect.path == claude_skill && effect.action == ClientPlanAction::Remove
    }));
    assert!(!claude_skill.exists());

    fs::copy(&agents_skill, &claude_skill).unwrap();
    let refreshed = run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: Vec::new(),
            all: false,
            refresh: true,
            private_runtime: false,
            yes: true,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &prompt,
    )
    .unwrap();
    assert!(refreshed.discovery_plan.iter().any(|effect| {
        effect.path == claude_skill && effect.action == ClientPlanAction::Remove
    }));
    assert!(!claude_skill.exists());
}

#[test]
fn final_client_removal_cleans_marker_owned_legacy_discovery_files() {
    let temp = tempfile::tempdir().unwrap();
    let environment = environment(&temp);
    let prompt = FixedPrompt {
        selected: None,
        confirmed: true,
    };
    let setup = run_with(
        SetupOperation::Setup,
        SetupRequest {
            clients: vec![SetupClient::Codex],
            all: false,
            refresh: false,
            private_runtime: false,
            yes: true,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &prompt,
    )
    .unwrap();
    assert!(!setup.has_client_failures());
    let agents_skill = environment.home.join(".agents/skills/leantoken/SKILL.md");
    let claude_skill = environment.home.join(".claude/skills/leantoken/SKILL.md");
    fs::create_dir_all(claude_skill.parent().unwrap()).unwrap();
    fs::copy(&agents_skill, &claude_skill).unwrap();

    let removal = run_with(
        SetupOperation::Remove,
        SetupRequest {
            clients: vec![SetupClient::Codex],
            all: false,
            refresh: false,
            private_runtime: false,
            yes: true,
            dry_run: false,
            allow_outdated: false,
            force_unmanaged: false,
        },
        &environment,
        &prompt,
    )
    .unwrap();

    assert_eq!(removal.discovery_plan.len(), 2);
    assert!(!agents_skill.exists());
    assert!(!claude_skill.exists());
}

#[test]
fn unmanaged_registration_requires_an_explicit_override() {
    let temp = tempfile::tempdir().unwrap();
    let environment = environment(&temp);
    let path = environment.home.join(".codex/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let manual = "[mcp_servers.leantoken]\ncommand = \"/opt/leantoken\"\nargs = [\"mcp\"]\n";
    fs::write(&path, manual).unwrap();
    let mut request = SetupRequest {
        clients: vec![SetupClient::Codex],
        all: false,
        refresh: false,
        private_runtime: false,
        yes: true,
        dry_run: false,
        allow_outdated: false,
        force_unmanaged: false,
    };
    let prompt = FixedPrompt {
        selected: None,
        confirmed: true,
    };

    let error = run_with(
        SetupOperation::Setup,
        request.clone(),
        &environment,
        &prompt,
    )
    .expect_err("manual entry must be protected");
    assert!(error.to_string().contains("--force-unmanaged"));
    assert_eq!(fs::read_to_string(&path).unwrap(), manual);

    request.force_unmanaged = true;
    let report = run_with(SetupOperation::Setup, request, &environment, &prompt).unwrap();
    assert_eq!(report.plan[0].action, ClientPlanAction::Update);
    let configured = fs::read_to_string(path).unwrap();
    assert!(configured.contains("--managed-by-setup"));
    assert!(!configured.contains("/opt/leantoken"));
}

#[test]
fn setup_ownership_recognizes_only_explicit_or_exact_legacy_launchers() {
    let runtime_root = Path::new("/data/leantoken/runtimes");
    let managed_runtime = runtime_root
        .join("1.2.3")
        .join(runtime_executable_name(cfg!(windows)))
        .to_string_lossy()
        .into_owned();
    let invalid_runtime = runtime_root
        .join("manual")
        .join(runtime_executable_name(cfg!(windows)))
        .to_string_lossy()
        .into_owned();
    assert!(is_managed_registration(
        "/opt/custom-wrapper",
        &["--managed-by-setup".into(), "mcp".into()],
        runtime_root
    ));
    assert!(is_managed_registration(
        "/usr/bin/node",
        &[
            "/usr/lib/node_modules/npm/bin/npx-cli.js".into(),
            "--yes".into(),
            "--prefer-offline".into(),
            "--package=leantoken@1.2.3".into(),
            "--".into(),
            "leantoken".into(),
            "mcp".into(),
        ],
        runtime_root
    ));
    assert!(is_managed_registration(
        &managed_runtime,
        &["mcp".into()],
        runtime_root
    ));
    assert!(!is_managed_registration(
        "/opt/custom-wrapper",
        &["--package=leantoken@1.2.3".into(), "mcp".into()],
        runtime_root
    ));
    assert!(!is_managed_registration(
        &invalid_runtime,
        &["mcp".into()],
        runtime_root
    ));
    for package in ["latest", "next", "^1.2.3", ">=1.2.3"] {
        assert!(!is_managed_registration(
            "/usr/bin/node",
            &[
                "/usr/lib/node_modules/npm/bin/npx-cli.js".into(),
                "--yes".into(),
                "--prefer-offline".into(),
                format!("--package=leantoken@{package}"),
                "--".into(),
                "leantoken".into(),
                "mcp".into(),
            ],
            runtime_root
        ));
    }
}

#[test]
fn recovery_journal_uses_its_separate_aggregate_read_bound() {
    let temp = tempfile::tempdir().unwrap();
    let runtime_root = temp.path().join("runtime");
    let path = temp.path().join("client.json");
    let original = "\n".repeat(5 * 1024 * 1024);
    fs::write(&path, "new").unwrap();
    let plan = ResolvedSetupPlan {
        operation: SetupOperation::Setup,
        persistent_cli: true,
        launcher: None,
        runtime: None,
        edits: vec![PlannedClientEdit {
            public: ClientSetupPlan {
                client: SetupClient::Claude,
                path: path.clone(),
                action: ClientPlanAction::Update,
                detected: true,
            },
            status: EditStatus::Updated,
            original: Some(original.clone()),
            updated: Some("new".into()),
        }],
        discovery_edits: Vec::new(),
        ownership_override: false,
        transaction_root: runtime_root.clone(),
    };

    let _transaction = begin_setup_transaction(&plan).unwrap().unwrap();
    let journal_path = transaction_path(&runtime_root);
    assert!(fs::metadata(&journal_path).unwrap().len() > MAX_SETUP_FILE_BYTES);
    assert!(fs::metadata(&journal_path).unwrap().len() < MAX_SETUP_JOURNAL_BYTES);

    recover_interrupted_transaction(&runtime_root).unwrap();

    assert_eq!(fs::read_to_string(path).unwrap(), original);
    assert!(!journal_path.exists());
}

#[test]
fn interrupted_setup_journal_restores_applied_and_unapplied_entries() {
    let temp = tempfile::tempdir().unwrap();
    let runtime_root = temp.path().join("runtime");
    fs::create_dir_all(&runtime_root).unwrap();
    let applied = temp.path().join("applied.json");
    let untouched = temp.path().join("untouched.json");
    fs::write(&applied, "new").unwrap();
    fs::write(&untouched, "old-two").unwrap();
    let journal = SetupTransactionJournal {
        schema_version: 1,
        entries: vec![
            SetupTransactionEntry {
                path: applied.clone(),
                original: Some("old-one".into()),
                updated_hash: Some(content_hash("new")),
                updated_exists: true,
            },
            SetupTransactionEntry {
                path: untouched.clone(),
                original: Some("old-two".into()),
                updated_hash: Some(content_hash("new-two")),
                updated_exists: true,
            },
        ],
    };
    fs::write(
        transaction_path(&runtime_root),
        serde_json::to_string(&journal).unwrap(),
    )
    .unwrap();

    recover_interrupted_transaction(&runtime_root).unwrap();

    assert_eq!(fs::read_to_string(applied).unwrap(), "old-one");
    assert_eq!(fs::read_to_string(untouched).unwrap(), "old-two");
    assert!(!transaction_path(&runtime_root).exists());
}
