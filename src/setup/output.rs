use super::*;

pub(super) fn print_preflight(plan: &ResolvedSetupPlan) -> Result<()> {
    let stderr = std::io::stderr();
    let mut output = stderr.lock();
    writeln!(output)?;
    writeln!(output, "◆ LeanToken {} plan", plan.operation.plan_label())?;
    for edit in &plan.edits {
        writeln!(output, "  Client configuration")?;
        writeln!(
            output,
            "    {} {}",
            plan_symbol(edit.public.action),
            edit.public.client.display_name()
        )?;
        writeln!(
            output,
            "      {} · {}",
            edit.public.action,
            edit.public.path.display()
        )?;
    }
    for edit in &plan.discovery_edits {
        let label = if edit
            .public
            .path
            .components()
            .any(|part| part.as_os_str() == ".agents")
        {
            "Universal agent discovery"
        } else {
            "Claude-compatible discovery"
        };
        writeln!(output, "  {label}")?;
        writeln!(output, "    {} skill", plan_symbol(edit.public.action))?;
        writeln!(
            output,
            "      {} · {}",
            edit.public.action,
            edit.public.path.display()
        )?;
    }
    if let Some(launcher) = &plan.launcher {
        writeln!(output)?;
        writeln!(output, "  Launcher")?;
        writeln!(output, "    pinned version: {}", launcher.version)?;
        if let Some(package) = &launcher.package {
            writeln!(output, "    exact package: {package}")?;
        }
        if let (Some(path), Some(digest)) = (&launcher.runtime_path, &launcher.runtime_digest) {
            writeln!(output, "    private runtime: {}", path.display())?;
            writeln!(output, "    BLAKE3: {digest}")?;
        }
        if launcher.may_contact_network {
            writeln!(
                output,
                "    Uses npm's cache first; a missing exact version may be fetched."
            )?;
        } else {
            writeln!(output, "    Uses the current LeanToken executable.")?;
        }
    }
    writeln!(output)?;
    if plan.ownership_override {
        writeln!(
            output,
            "  Explicit override: selected unmanaged client entries may change."
        )?;
    } else {
        writeln!(
            output,
            "  Only LeanToken-owned client entries and discovery files will change."
        )?;
    }
    if plan.operation == SetupOperation::Setup {
        writeln!(
            output,
            "  After setup, the exact launcher will be checked through MCP initialize, catalog, and first retrieval."
        )?;
    }
    Ok(())
}

pub(super) fn plan_symbol(action: ClientPlanAction) -> &'static str {
    match action {
        ClientPlanAction::Create | ClientPlanAction::Update | ClientPlanAction::Remove => "◇",
        ClientPlanAction::AlreadyCurrent | ClientPlanAction::NotConfigured => "─",
    }
}

pub(super) fn print_report_plan(output: &mut impl Write, report: &SetupReport) -> Result<()> {
    writeln!(output, "◆ LeanToken dry-run")?;
    writeln!(output, "  No changes were made.")?;
    if report.ownership_override {
        writeln!(
            output,
            "  Explicit override: applying this plan may replace an unmanaged client entry."
        )?;
    }
    for effect in &report.plan {
        writeln!(
            output,
            "  {} {}: {} ({})",
            plan_symbol(effect.action),
            effect.client.display_name(),
            effect.path.display(),
            effect.action
        )?;
    }
    for effect in &report.discovery_plan {
        writeln!(
            output,
            "  {} Agent discovery: {} ({})",
            plan_symbol(effect.action),
            effect.path.display(),
            effect.action
        )?;
    }
    if let Some(launcher) = &report.launcher {
        writeln!(output)?;
        writeln!(output, "  Launcher: {}", launcher.command)?;
        writeln!(
            output,
            "  Arguments: {}",
            serde_json::to_string(&launcher.args)?
        )?;
        writeln!(output, "  Version: {}", launcher.version)?;
        if let Some(package) = &launcher.package {
            writeln!(output, "  Package: {package}")?;
        }
        if let (Some(path), Some(digest)) = (&launcher.runtime_path, &launcher.runtime_digest) {
            writeln!(output, "  Private runtime: {}", path.display())?;
            writeln!(output, "  BLAKE3: {digest}")?;
        }
        if launcher.may_contact_network {
            writeln!(
                output,
                "  Client startup may contact npm, but it can resolve only this exact version."
            )?;
        }
    }
    Ok(())
}

/// Print a setup report as JSON or concise human-readable output.
pub fn print_report(report: &SetupReport, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    if report.cancelled {
        writeln!(
            output,
            "LeanToken {} cancelled. No changes were made.",
            report.operation.action()
        )?;
        return Ok(());
    }
    if report.dry_run {
        print_report_plan(&mut output, report)?;
        return Ok(());
    }
    writeln!(output, "◆ LeanToken // Context Distillery")?;
    let operation_label = match report.operation {
        SetupOperation::Setup => "MCP client setup",
        SetupOperation::Remove => "MCP client removal",
    };
    writeln!(output, "  {operation_label}")?;
    for result in &report.results {
        if let Some(error) = result.outcome.error() {
            writeln!(
                output,
                "  ✗ {}: {} ({})",
                result.client.display_name(),
                result.path.display(),
                error
            )?;
        } else {
            writeln!(
                output,
                "  ✓ {}: {} ({})",
                result.client.display_name(),
                result.path.display(),
                result.outcome.status()
            )?;
        }
    }
    if let Some(error) = &report.apply_error {
        writeln!(output, "  ✗ Setup transaction: {error}")?;
    }
    if let Some(verification) = &report.verification {
        match verification {
            SetupVerification::Passed { .. } => writeln!(
                output,
                "  ✓ Exact launcher verified: initialize, 9-tool catalog, first retrieval"
            )?,
            SetupVerification::Skipped { message, .. } => {
                writeln!(output, "  ─ Launcher verification skipped: {}", message)?
            }
            SetupVerification::Failed {
                stage,
                message,
                repair_command,
            } => {
                writeln!(
                    output,
                    "  ✗ Launcher verification failed at {}: {}",
                    stage, message
                )?;
                writeln!(output, "    Retry: {repair_command}")?;
            }
        }
    }
    if report.operation == SetupOperation::Setup {
        let configured = report
            .results
            .iter()
            .filter(|result| result.outcome.error().is_none())
            .count();
        let changed = report
            .results
            .iter()
            .filter(|result| result.outcome.changed())
            .count();
        writeln!(output)?;
        writeln!(
            output,
            "LeanToken is configured for {configured} client{}.",
            if configured == 1 { "" } else { "s" }
        )?;
        if report.has_apply_failure() {
            writeln!(
                output,
                "The setup transaction did not complete; planned discovery cleanup may remain."
            )?;
        } else if report.has_client_failures() {
            writeln!(
                output,
                "Some selected clients failed; successful changes were not rolled back."
            )?;
        } else if report.has_verification_failure() {
            writeln!(
                output,
                "Client configuration succeeded, but launcher verification failed. The configured entries remain in place for diagnosis."
            )?;
        } else if changed > 0 {
            writeln!(
                output,
                "Next: restart or reload the configured clients to connect LeanToken."
            )?;
        } else if report.results.is_empty() && report.discovery_plan.is_empty() {
            writeln!(
                output,
                "No existing LeanToken client registrations were recognized; no changes were made."
            )?;
        } else {
            writeln!(output, "No configuration changes were needed.")?;
        }
        writeln!(output)?;
        if report
            .launcher
            .as_ref()
            .is_some_and(|launcher| launcher.runtime_path.is_some())
        {
            writeln!(
                output,
                "MCP clients now launch the pinned private native runtime directly."
            )?;
            writeln!(output, "Versioned runtimes are retained during removal.")?;
            writeln!(
                output,
                "Inspect or reclaim them later with: {} runtime list",
                command_prefix(report)
            )?;
        } else if report.persistent_cli {
            writeln!(output, "Update later with: leantoken upgrade")?;
        } else {
            let version = report
                .launcher
                .as_ref()
                .map_or(env!("CARGO_PKG_VERSION"), |launcher| {
                    launcher.version.as_str()
                });
            writeln!(
                output,
                "This was a zero-install npx setup; no global `leantoken` command was installed."
            )?;
            writeln!(
                output,
                "Configured MCP clients are pinned to LeanToken v{version}."
            )?;
            writeln!(
                output,
                "Refresh existing MCP entries explicitly with: npx --yes leantoken@latest setup --refresh --yes"
            )?;
            writeln!(
                output,
                "Recommended direct launcher: npx --yes leantoken@{version} setup --refresh --private-runtime --yes"
            )?;
            writeln!(
                output,
                "Install the shell command with: npm install --global leantoken@latest"
            )?;
        }
        if let Some(client) = report
            .results
            .iter()
            .find(|result| result.outcome.error().is_none())
            .map(|result| result.client)
        {
            writeln!(
                output,
                "Verify the stored {} launcher from a repository: {} doctor --client {}",
                client.display_name(),
                command_prefix(report),
                client.cli_name()
            )?;
        }
        writeln!(
            output,
            "In-agent smoke test: Ask the client to use LeanToken to find the code related to request cancellation."
        )?;
    }
    Ok(())
}

fn command_prefix(report: &SetupReport) -> String {
    if report.persistent_cli {
        "leantoken".into()
    } else {
        let version = report
            .launcher
            .as_ref()
            .map_or(env!("CARGO_PKG_VERSION"), |launcher| {
                launcher.version.as_str()
            });
        format!("npx --yes leantoken@{version}")
    }
}
