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
    writeln!(
        output,
        "  Only LeanToken-owned client entries and discovery files will change."
    )?;
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
        if let Some(error) = &result.error {
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
                result.status
            )?;
        }
    }
    if let Some(verification) = &report.verification {
        match verification.status {
            SetupVerificationStatus::Passed => {
                writeln!(output, "  ✓ Launcher verification: MCP ready")?
            }
            SetupVerificationStatus::Skipped => writeln!(
                output,
                "  ─ Launcher verification skipped: {}",
                verification.message.as_deref().unwrap_or("not applicable")
            )?,
            SetupVerificationStatus::Failed => {
                writeln!(
                    output,
                    "  ✗ Launcher verification failed at {}: {}",
                    verification.stage.as_deref().unwrap_or("unknown"),
                    verification.message.as_deref().unwrap_or("unknown failure")
                )?;
                if let Some(command) = &verification.repair_command {
                    writeln!(output, "    Retry: {command}")?;
                }
            }
        }
    }
    if report.operation == SetupOperation::Setup {
        let configured = report
            .results
            .iter()
            .filter(|result| result.error.is_none())
            .count();
        let changed = report
            .results
            .iter()
            .filter(|result| matches!(result.status.as_str(), "configured" | "updated"))
            .count();
        writeln!(output)?;
        writeln!(
            output,
            "LeanToken is configured for {configured} client{}.",
            if configured == 1 { "" } else { "s" }
        )?;
        if report.has_failures() {
            writeln!(
                output,
                "Some selected clients failed; successful changes were not rolled back."
            )?;
        } else if changed > 0 {
            writeln!(
                output,
                "Restart or reload the configured clients to connect LeanToken."
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
            writeln!(output, "Verify from a repository: leantoken doctor")?;
        } else if report.persistent_cli {
            writeln!(output, "Verify from a repository: leantoken doctor")?;
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
                "Verify from a repository: npx leantoken@{version} doctor"
            )?;
            writeln!(
                output,
                "Install the shell command with: npm install --global leantoken@latest"
            )?;
        }
    }
    Ok(())
}
