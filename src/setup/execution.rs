use super::*;
use std::time::Duration;

const SETUP_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Run global MCP setup or removal using the current user environment.
pub fn run(
    operation: SetupOperation,
    request: SetupRequest,
    json_output: bool,
) -> Result<SetupReport> {
    let home = home_directory()
        .ok_or_else(|| Error::SetupFailure("could not determine the home directory".into()))?;
    let launcher = McpLauncher::current()?;
    let native_executable = std::env::current_exe()?.canonicalize()?;
    if operation == SetupOperation::Setup
        && launcher.uses_npx()
        && !request.allow_outdated
        && npx_resolved_from_local_project(&native_executable, &std::env::current_dir()?)
    {
        require_current_npx_setup(
            launcher.version(),
            crate::upgrade::latest_npm_version().as_deref(),
        )?;
    }
    let runtime_root = setup_runtime_root(&home);
    let environment = SetupEnvironment {
        home,
        runtime_root,
        native_executable,
        persistent_cli: !launcher.is_ephemeral(),
        launcher,
        interactive: !json_output
            && std::io::stdin().is_terminal()
            && std::io::stderr().is_terminal(),
    };
    run_with(operation, request, &environment, &InteractivePrompt)
}

pub(super) fn run_with(
    operation: SetupOperation,
    request: SetupRequest,
    environment: &SetupEnvironment,
    prompt: &dyn SetupPrompt,
) -> Result<SetupReport> {
    let recovery_path = transaction_path(&environment.runtime_root);
    if request.dry_run && recovery_path.exists() {
        return Err(Error::SetupFailure(format!(
            "interrupted setup requires recovery before dry-run: {}",
            recovery_path.display()
        )));
    }
    let _setup_lock = (!request.dry_run)
        .then(|| acquire_setup_lock(&environment.runtime_root))
        .transpose()?;
    if !request.dry_run {
        recover_interrupted_transaction(&environment.runtime_root)?;
    }
    if request.refresh && operation != SetupOperation::Setup {
        return Err(Error::InvalidRequest(
            "--refresh is only valid with the setup command".into(),
        ));
    }
    if request.refresh && (request.all || !request.clients.is_empty()) {
        return Err(Error::InvalidRequest(
            "--refresh cannot be combined with client flags or --all".into(),
        ));
    }
    if request.private_runtime && operation != SetupOperation::Setup {
        return Err(Error::InvalidRequest(
            "--private-runtime is only valid with the setup command".into(),
        ));
    }
    if request.allow_outdated && operation != SetupOperation::Setup {
        return Err(Error::InvalidRequest(
            "--allow-outdated is only valid with the setup command".into(),
        ));
    }

    let runtime = request
        .private_runtime
        .then(|| runtime_install_plan(environment))
        .transpose()?;
    let private_launcher = runtime.as_ref().map(|runtime| {
        McpLauncher::from_executable_with_version(
            &runtime.destination,
            environment.launcher.version(),
        )
    });
    let launcher = private_launcher.as_ref().unwrap_or(&environment.launcher);

    let detected = SetupClient::ALL
        .into_iter()
        .filter(|client| client.is_detected(&environment.home))
        .collect::<Vec<_>>();

    let clients = if request.refresh {
        managed_clients(&environment.home, launcher)?
    } else if request.all {
        SetupClient::ALL.to_vec()
    } else if !request.clients.is_empty() {
        deduplicate(request.clients)
    } else if request.yes {
        return Err(Error::InvalidRequest(
            "--yes requires explicit client flags or --all; detection is not consent".into(),
        ));
    } else {
        if !environment.interactive {
            return Err(Error::InvalidRequest(
                "interactive setup requires a terminal; pass client flags or --all with --yes"
                    .into(),
            ));
        }
        let preferred = if operation == SetupOperation::Setup {
            detected.clone()
        } else {
            managed_clients(&environment.home, launcher)?
        };
        let Some(selected) = prompt.select(operation, &detected, &preferred)? else {
            return Ok(empty_report(operation, environment.persistent_cli));
        };
        if selected.is_empty() {
            return Ok(empty_report(operation, environment.persistent_cli));
        }
        selected
    };

    if !environment.interactive && !request.dry_run && !request.yes {
        return Err(Error::InvalidRequest(
            "non-interactive setup requires explicit client flags, --all, or --refresh with --yes"
                .into(),
        ));
    }
    let selected_discovery_paths = clients
        .iter()
        .map(|client| client.discovery_path(&environment.home))
        .collect::<std::collections::BTreeSet<_>>();
    let unselected_clients = SetupClient::ALL
        .into_iter()
        .filter(|client| !clients.contains(client))
        .collect::<Vec<_>>();
    let (unselected_registrations, configuration_snapshots) =
        configured_registrations_with_snapshots(&environment.home, launcher, &unselected_clients)?;
    let (discovery_paths, discovery_cleanup_paths) = if operation == SetupOperation::Setup {
        let mut required_paths = unselected_registrations
            .into_iter()
            .filter(|registration| registration.managed)
            .map(|registration| registration.client.discovery_path(&environment.home))
            .collect::<std::collections::BTreeSet<_>>();
        required_paths.extend(selected_discovery_paths.iter().cloned());
        let cleanup_paths = [
            environment.home.join(".agents/skills/leantoken/SKILL.md"),
            environment.home.join(".claude/skills/leantoken/SKILL.md"),
        ]
        .into_iter()
        .filter(|path| !required_paths.contains(path))
        .collect();
        (
            selected_discovery_paths.into_iter().collect(),
            cleanup_paths,
        )
    } else {
        let remaining_paths = unselected_registrations
            .into_iter()
            .map(|registration| registration.client)
            .map(|client| client.discovery_path(&environment.home))
            .collect::<std::collections::BTreeSet<_>>();
        if remaining_paths.is_empty() {
            (
                [
                    environment.home.join(".agents/skills/leantoken/SKILL.md"),
                    environment.home.join(".claude/skills/leantoken/SKILL.md"),
                ]
                .into_iter()
                .collect(),
                Vec::new(),
            )
        } else {
            (
                selected_discovery_paths
                    .difference(&remaining_paths)
                    .cloned()
                    .collect(),
                Vec::new(),
            )
        }
    };

    let plan = resolve_plan(
        operation,
        &clients,
        PlanEnvironment {
            detected: &detected,
            home: &environment.home,
            launcher,
            persistent_cli: environment.persistent_cli,
            runtime,
            discovery_paths,
            discovery_cleanup_paths,
            configuration_snapshots,
            force_unmanaged: request.force_unmanaged,
            transaction_root: &environment.runtime_root,
        },
    )?;

    if request.dry_run {
        return Ok(report_from_plan(&plan, false, true, Vec::new(), None, None));
    }

    if !request.yes && !prompt.confirm(operation, &plan)? {
        return Ok(report_from_plan(&plan, true, false, Vec::new(), None, None));
    }

    let outcome = apply_plan(&plan);
    let verification = verify_applied_setup(&plan, &outcome.results, outcome.error.as_deref());
    Ok(report_from_plan(
        &plan,
        false,
        false,
        outcome.results,
        outcome.error,
        verification,
    ))
}

fn verify_applied_setup(
    plan: &ResolvedSetupPlan,
    results: &[ClientSetupResult],
    apply_error: Option<&str>,
) -> Option<SetupVerification> {
    let launcher = plan.launcher.as_ref()?;
    let client_argument = plan
        .edits
        .first()
        .map(|edit| format!(" --client {}", edit.public.client.cli_name()))
        .unwrap_or_default();
    let repair_command = if let Some(package) = &launcher.package {
        format!("npx --yes {package} doctor{client_argument} --json")
    } else if plan.persistent_cli {
        format!("leantoken doctor{client_argument} --json")
    } else {
        format!(
            "npx --yes leantoken@{} doctor{client_argument} --json",
            launcher.version
        )
    };
    if apply_error.is_some() || results.iter().any(|result| result.error.is_some()) {
        return Some(SetupVerification {
            status: SetupVerificationStatus::Skipped,
            stage: None,
            message: Some("setup transaction did not complete".into()),
            repair_command: Some(repair_command),
        });
    }

    let result = (|| {
        let repository = tempfile::tempdir()?;
        fs::write(
            repository.path().join("lib.rs"),
            "pub fn ready() -> bool { true }\n",
        )?;
        let config = crate::Config::discover(
            repository.path(),
            Some(repository.path().join("index.sqlite")),
        )?;
        crate::doctor::run_launcher(
            &config,
            &launcher.command,
            &launcher.args,
            SETUP_VERIFICATION_TIMEOUT,
        )
    })();

    Some(match result {
        Ok(_) => SetupVerification {
            status: SetupVerificationStatus::Passed,
            stage: None,
            message: None,
            repair_command: Some(repair_command),
        },
        Err(Error::DoctorFailure { stage, message }) => SetupVerification {
            status: SetupVerificationStatus::Failed,
            stage: Some(stage.into()),
            message: Some(message),
            repair_command: Some(repair_command),
        },
        Err(error) => SetupVerification {
            status: SetupVerificationStatus::Failed,
            stage: Some("launch".into()),
            message: Some(error.to_string()),
            repair_command: Some(repair_command),
        },
    })
}

pub(super) fn report_from_plan(
    plan: &ResolvedSetupPlan,
    cancelled: bool,
    dry_run: bool,
    results: Vec<ClientSetupResult>,
    apply_error: Option<String>,
    verification: Option<SetupVerification>,
) -> SetupReport {
    let discovery_skill_tokens = plan.discovery_edits.first().and_then(|edit| {
        edit.updated
            .as_ref()
            .or(edit.original.as_ref())
            .map(|content| crate::tokens::Tokenizer::Cl100kBase.count(content))
    });
    SetupReport {
        operation: plan.operation,
        cancelled,
        dry_run,
        ownership_override: plan.ownership_override,
        persistent_cli: plan.persistent_cli,
        launcher: plan.launcher.clone(),
        plan: plan.edits.iter().map(|edit| edit.public.clone()).collect(),
        discovery_plan: plan
            .discovery_edits
            .iter()
            .map(|edit| edit.public.clone())
            .collect(),
        discovery_skill_tokens,
        results,
        apply_error,
        verification,
    }
}

pub(super) fn empty_report(operation: SetupOperation, persistent_cli: bool) -> SetupReport {
    SetupReport {
        operation,
        cancelled: true,
        dry_run: false,
        ownership_override: false,
        persistent_cli,
        launcher: None,
        plan: Vec::new(),
        discovery_plan: Vec::new(),
        discovery_skill_tokens: None,
        results: Vec::new(),
        apply_error: None,
        verification: None,
    }
}

pub(super) fn deduplicate(clients: Vec<SetupClient>) -> Vec<SetupClient> {
    SetupClient::ALL
        .into_iter()
        .filter(|client| clients.contains(client))
        .collect()
}
