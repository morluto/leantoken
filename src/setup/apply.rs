use super::*;

pub(super) struct SetupApplyOutcome {
    pub(super) results: Vec<ClientSetupResult>,
    pub(super) error: Option<String>,
}

pub(super) fn apply_plan(plan: &ResolvedSetupPlan) -> SetupApplyOutcome {
    let mut runtime_installation = match plan.runtime.as_ref().map(install_runtime).transpose() {
        Ok(installation) => installation,
        Err(error) => return failed_outcome(plan, error.to_string()),
    };
    if let Err(error) = preflight_configuration_snapshots(&plan.configuration_snapshots)
        .and_then(|()| preflight_edits(&plan.edits))
        .and_then(|()| preflight_discovery(&plan.discovery_edits))
    {
        let message = rollback_runtime_message(error, runtime_installation.take());
        return failed_outcome(plan, message);
    }
    let transaction = match begin_setup_transaction(plan) {
        Ok(transaction) => transaction,
        Err(error) => {
            let message = rollback_runtime_message(error, runtime_installation.take());
            return failed_outcome(plan, message);
        }
    };

    let mut applied: Vec<&PlannedClientEdit> = Vec::new();
    let mut applied_discovery: Vec<&PlannedDiscoveryEdit> = Vec::new();
    for edit in &plan.edits {
        // A write can succeed before its directory sync fails. Recovery must
        // cover attempted mutations as well as fully completed ones.
        if edit.updated().is_some() {
            applied.push(edit);
        }
        if let Err(error) = apply_edit(edit) {
            let rollback = rollback_setup(
                runtime_installation.take(),
                &applied,
                &applied_discovery,
                transaction,
            );
            return failed_outcome(plan, rollback_message(error, rollback));
        }
    }
    for edit in &plan.discovery_edits {
        if !matches!(
            edit.public.action,
            ClientPlanAction::AlreadyCurrent | ClientPlanAction::NotConfigured
        ) {
            applied_discovery.push(edit);
        }
        if let Err(error) = apply_discovery_edit(edit) {
            let rollback = rollback_setup(
                runtime_installation.take(),
                &applied,
                &applied_discovery,
                transaction,
            );
            return failed_outcome(plan, rollback_message(error, rollback));
        }
    }
    if let Some(transaction) = transaction
        && let Err(error) = transaction.commit()
    {
        let rollback = rollback_setup(
            runtime_installation.take(),
            &applied,
            &applied_discovery,
            Some(transaction),
        );
        return failed_outcome(plan, rollback_message(error, rollback));
    }
    SetupApplyOutcome {
        results: plan
            .edits
            .iter()
            .map(|edit| ClientSetupResult {
                client: edit.public.client,
                path: edit.public.path.clone(),
                outcome: edit.status().into(),
            })
            .collect(),
        error: None,
    }
}

fn rollback_runtime_message(
    error: Error,
    runtime_installation: Option<RuntimeInstallReceipt>,
) -> String {
    match runtime_installation {
        Some(receipt @ RuntimeInstallReceipt::Installed(_)) => {
            rollback_message(error, rollback_installed_runtime(receipt))
        }
        Some(RuntimeInstallReceipt::Unchanged) | None => error.to_string(),
    }
}

fn failed_outcome(plan: &ResolvedSetupPlan, error: String) -> SetupApplyOutcome {
    SetupApplyOutcome {
        results: failed_results(&plan.edits, error.clone()),
        error: Some(error),
    }
}

pub(super) fn rollback_setup(
    runtime_installation: Option<RuntimeInstallReceipt>,
    applied: &[&PlannedClientEdit],
    applied_discovery: &[&PlannedDiscoveryEdit],
    transaction: Option<SetupTransaction>,
) -> Result<()> {
    let mut failure = None;
    for edit in applied_discovery.iter().rev() {
        if let Err(error) = restore_discovery_edit(edit) {
            failure.get_or_insert(error);
        }
    }
    for edit in applied.iter().rev() {
        if let Err(error) = restore_edit(edit) {
            failure.get_or_insert(error);
        }
    }
    if let Some(error) = failure {
        // Keep the journal and runtime while any configuration may still refer
        // to the installation, but restore independent files where possible.
        return Err(error);
    }
    if let Some(runtime_installation) = runtime_installation {
        rollback_installed_runtime(runtime_installation)?;
    }
    if let Some(transaction) = transaction {
        transaction.commit()?;
    }
    Ok(())
}

pub(super) fn rollback_message(error: Error, rollback: Result<()>) -> String {
    match rollback {
        Ok(()) => format!("setup transaction rolled back: {error}"),
        Err(rollback_error) => format!(
            "setup transaction failed: {error}; rollback requires recovery: {rollback_error}"
        ),
    }
}

pub(super) fn preflight_configuration_snapshots(
    snapshots: &[PlannedConfigurationSnapshot],
) -> Result<()> {
    for snapshot in snapshots {
        if read_optional(&snapshot.path)? != snapshot.original {
            return Err(Error::SetupFailure(format!(
                "configuration changed after preflight: {}",
                snapshot.path.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn preflight_edits(edits: &[PlannedClientEdit]) -> Result<()> {
    for edit in edits {
        if read_optional(&edit.public.path)?.as_deref() != edit.original() {
            return Err(Error::SetupFailure(format!(
                "configuration changed after preflight: {}",
                edit.public.path.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn preflight_discovery(edits: &[PlannedDiscoveryEdit]) -> Result<()> {
    for edit in edits {
        if read_optional(&edit.public.path)? != edit.original {
            return Err(Error::SetupFailure(format!(
                "discovery skill changed after preflight: {}",
                edit.public.path.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn failed_results(edits: &[PlannedClientEdit], error: String) -> Vec<ClientSetupResult> {
    edits
        .iter()
        .map(|edit| ClientSetupResult {
            client: edit.public.client,
            path: edit.public.path.clone(),
            outcome: ClientSetupOutcome::Failed {
                error: error.clone(),
            },
        })
        .collect()
}

pub(super) fn restore_edit(edit: &PlannedClientEdit) -> Result<()> {
    let Some(updated) = edit.updated() else {
        return Ok(());
    };
    restore_path(
        &edit.public.path,
        edit.original(),
        &SetupTransactionUpdate::Present {
            content_hash: content_hash(updated),
        },
    )
}

pub(super) fn apply_discovery_edit(edit: &PlannedDiscoveryEdit) -> Result<()> {
    if read_optional(&edit.public.path)? != edit.original {
        return Err(Error::SetupFailure(format!(
            "discovery skill changed after preflight: {}",
            edit.public.path.display()
        )));
    }
    match edit.public.action {
        ClientPlanAction::Create | ClientPlanAction::Update => write_if_changed(
            &edit.public.path,
            edit.original.as_deref().unwrap_or_default(),
            edit.updated.as_deref().unwrap_or_default(),
        ),
        ClientPlanAction::Remove => {
            reject_symlink_target(&edit.public.path)?;
            if edit.public.path.exists() {
                fs::remove_file(&edit.public.path)?;
                sync_parent_directory(&edit.public.path)?;
            }
            Ok(())
        }
        ClientPlanAction::AlreadyCurrent | ClientPlanAction::NotConfigured => Ok(()),
    }
}

pub(super) fn restore_discovery_edit(edit: &PlannedDiscoveryEdit) -> Result<()> {
    let updated = match edit.public.action {
        ClientPlanAction::Create | ClientPlanAction::Update => SetupTransactionUpdate::Present {
            content_hash: content_hash(edit.updated.as_deref().ok_or_else(|| {
                Error::SetupFailure("setup plan omitted updated discovery content".into())
            })?),
        },
        ClientPlanAction::Remove => SetupTransactionUpdate::Absent,
        ClientPlanAction::AlreadyCurrent | ClientPlanAction::NotConfigured => return Ok(()),
    };
    restore_path(&edit.public.path, edit.original.as_deref(), &updated)
}

pub(super) fn apply_edit(edit: &PlannedClientEdit) -> Result<()> {
    let current = read_optional(&edit.public.path)?;
    if current.as_deref() != edit.original() {
        return Err(Error::SetupFailure(format!(
            "configuration changed after preflight: {}",
            edit.public.path.display()
        )));
    }
    if let Some(updated) = edit.updated() {
        write_if_changed(
            &edit.public.path,
            edit.original().unwrap_or_default(),
            updated,
        )?;
    }
    Ok(())
}
