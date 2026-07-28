/// Run global MCP setup or removal using the current user environment.
pub fn run(
    operation: SetupOperation,
    request: SetupRequest,
    json_output: bool,
) -> Result<SetupReport> {
    let home = home_directory()
        .ok_or_else(|| Error::InternalFailure("could not determine the home directory".into()))?;
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
        persistent_cli: !launcher.uses_npx(),
        launcher,
        interactive: !json_output
            && std::io::stdin().is_terminal()
            && std::io::stderr().is_terminal(),
    };
    run_with(operation, request, &environment, &InteractivePrompt)
}

fn run_with(
    operation: SetupOperation,
    request: SetupRequest,
    environment: &SetupEnvironment,
    prompt: &dyn SetupPrompt,
) -> Result<SetupReport> {
    let recovery_path = transaction_path(&environment.runtime_root);
    if request.dry_run && recovery_path.exists() {
        return Err(Error::InternalFailure(format!(
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
        configured_clients(&environment.home, launcher)?
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
        let Some(selected) = prompt.select(operation, &detected)? else {
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
    let manage_discovery = if operation == SetupOperation::Setup {
        true
    } else {
        configured_clients(&environment.home, launcher)?
            .into_iter()
            .all(|configured| clients.contains(&configured))
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
            manage_discovery,
            transaction_root: &environment.runtime_root,
        },
    )?;

    if request.dry_run {
        return Ok(report_from_plan(&plan, false, true, Vec::new()));
    }

    if !request.yes && !prompt.confirm(operation, &plan)? {
        return Ok(report_from_plan(&plan, true, false, Vec::new()));
    }

    let results = apply_plan(&plan);
    Ok(report_from_plan(&plan, false, false, results))
}

fn report_from_plan(
    plan: &ResolvedSetupPlan,
    cancelled: bool,
    dry_run: bool,
    results: Vec<ClientSetupResult>,
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
    }
}

fn empty_report(operation: SetupOperation, persistent_cli: bool) -> SetupReport {
    SetupReport {
        operation,
        cancelled: true,
        dry_run: false,
        persistent_cli,
        launcher: None,
        plan: Vec::new(),
        discovery_plan: Vec::new(),
        discovery_skill_tokens: None,
        results: Vec::new(),
    }
}

fn deduplicate(clients: Vec<SetupClient>) -> Vec<SetupClient> {
    SetupClient::ALL
        .into_iter()
        .filter(|client| clients.contains(client))
        .collect()
}
