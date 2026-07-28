#[derive(Debug)]
struct PlannedClientEdit {
    public: ClientSetupPlan,
    status: EditStatus,
    original: Option<String>,
    updated: Option<String>,
}

#[derive(Debug)]
struct PlannedDiscoveryEdit {
    public: DiscoverySetupPlan,
    original: Option<String>,
    updated: Option<String>,
}

struct PlanEnvironment<'a> {
    detected: &'a [SetupClient],
    home: &'a Path,
    launcher: &'a McpLauncher,
    persistent_cli: bool,
    runtime: Option<RuntimeInstallPlan>,
    manage_discovery: bool,
    transaction_root: &'a Path,
}

fn resolve_plan(
    operation: SetupOperation,
    clients: &[SetupClient],
    environment: PlanEnvironment<'_>,
) -> Result<ResolvedSetupPlan> {
    let edits = clients
        .iter()
        .copied()
        .map(|client| {
            resolve_client_edit(
                operation,
                client,
                environment.detected,
                environment.home,
                environment.launcher,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let discovery_edits = if environment.manage_discovery {
        resolve_discovery_edits(operation, environment.home, Some(environment.launcher))?
    } else {
        Vec::new()
    };
    let launcher = (operation == SetupOperation::Setup)
        .then(|| launcher_plan(environment.launcher, environment.runtime.as_ref()))
        .transpose()?;
    Ok(ResolvedSetupPlan {
        operation,
        persistent_cli: environment.persistent_cli,
        launcher,
        runtime: environment.runtime,
        edits,
        discovery_edits,
        transaction_root: environment.transaction_root.to_path_buf(),
    })
}

fn launcher_plan(
    launcher: &McpLauncher,
    runtime: Option<&RuntimeInstallPlan>,
) -> Result<LauncherPlan> {
    Ok(LauncherPlan {
        command: launcher.command()?.to_string(),
        args: launcher.args.clone(),
        version: launcher.version().into(),
        package: launcher.npm_package().map(str::to_owned),
        may_contact_network: launcher.uses_npx(),
        runtime_path: runtime.map(|runtime| runtime.destination.clone()),
        runtime_digest: runtime.map(|runtime| runtime.digest.clone()),
    })
}

fn resolve_discovery_edits(
    operation: SetupOperation,
    home: &Path,
    launcher: Option<&McpLauncher>,
) -> Result<Vec<PlannedDiscoveryEdit>> {
    let content = launcher.map(discovery_skill).transpose()?;
    [
        home.join(".agents/skills/leantoken/SKILL.md"),
        home.join(".claude/skills/leantoken/SKILL.md"),
    ]
    .into_iter()
    .map(|path| {
        let original = read_optional(&path)?;
        let owned = original
            .as_deref()
            .is_some_and(|value| value.contains(DISCOVERY_SKILL_MARKER));
        let (action, updated) = match operation {
            SetupOperation::Setup => {
                if original.as_deref() == content.as_deref() {
                    (ClientPlanAction::AlreadyCurrent, None)
                } else if original.is_none() || owned {
                    (
                        if original.is_none() {
                            ClientPlanAction::Create
                        } else {
                            ClientPlanAction::Update
                        },
                        content.clone(),
                    )
                } else {
                    return Err(Error::InternalFailure(format!(
                        "refusing to overwrite unowned discovery skill {}",
                        path.display()
                    )));
                }
            }
            SetupOperation::Remove if owned => (ClientPlanAction::Remove, Some(String::new())),
            SetupOperation::Remove => (ClientPlanAction::NotConfigured, None),
        };
        Ok(PlannedDiscoveryEdit {
            public: DiscoverySetupPlan { path, action },
            original,
            updated,
        })
    })
    .collect()
}

fn discovery_skill(launcher: &McpLauncher) -> Result<String> {
    let doctor = if launcher.uses_npx() {
        format!(
            "npx --yes {} doctor --json",
            launcher.npm_package().unwrap_or("leantoken")
        )
    } else {
        "leantoken doctor --json".into()
    };
    Ok(format!(
        "---\nname: leantoken\ndescription: Use LeanToken for token-bounded repository exploration, audits, codebase investigations, architecture reviews, source archaeology, code search, symbol outlines, exact source reads, symbol history, and structural JSON queries.\n---\n\n{DISCOVERY_SKILL_MARKER}\n\nBefore retrieving repository source, including for audits and code archaeology, discover the deferred `leantoken` MCP server and choose the narrowest applicable lane:\n\n1. For autonomous broad triage, call `leantoken.context` once with `plan_only=false` and use the materialized evidence directly. Make at most one focused follow-up only when coverage identifies a concrete gap.\n2. For known scope, `leantoken.files` finds paths, `leantoken.outline` maps definitions and imports, and `leantoken.search` locates symbols, references, identifiers, text, or regex matches. `leantoken.read` returns the exact current symbol or narrow line range; `leantoken.history` reads, diffs, or traces symbols across immutable Git revisions; `leantoken.json` queries, summarizes, or compares exact JSON artifacts without whole-file output.\n3. For human review or control-plane inspection before expensive or high-risk materialization, call `leantoken.context` with `plan_only=true`, inspect the bounded metadata and coverage, then materialize after approval.\n\nPass `BASE..HEAD` as `base_revision` with `strict_changed_paths` for immutable range context. Use `leantoken.savings` for repository-local savings. Use native workspace tools for edits, commands, tests, runtime probes, unsupported files, or evidence that is not source retrieval. If the server or tools cannot be discovered, run `{doctor}` and report its structured registration, launch, handshake, and catalog status instead of silently claiming LeanToken was used.\n"
    ))
}

fn resolve_client_edit(
    operation: SetupOperation,
    client: SetupClient,
    detected: &[SetupClient],
    home: &Path,
    launcher: &McpLauncher,
) -> Result<PlannedClientEdit> {
    let definition = client.definition(home);
    let (status, original, updated) = match definition.format {
        ConfigFormat::Json { section, shape } => {
            resolve_json_edit(operation, &definition.path, section, shape, launcher)?
        }
        ConfigFormat::Toml => resolve_toml_edit(operation, &definition.path, launcher)?,
    };
    let action = match status {
        EditStatus::Configured if original.is_none() => ClientPlanAction::Create,
        EditStatus::Configured | EditStatus::Updated => ClientPlanAction::Update,
        EditStatus::AlreadyConfigured => ClientPlanAction::AlreadyCurrent,
        EditStatus::Removed => ClientPlanAction::Remove,
        EditStatus::NotConfigured => ClientPlanAction::NotConfigured,
    };
    Ok(PlannedClientEdit {
        public: ClientSetupPlan {
            client,
            path: definition.path,
            action,
            detected: detected.contains(&client),
        },
        status,
        original,
        updated,
    })
}
