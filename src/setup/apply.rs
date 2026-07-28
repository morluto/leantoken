fn apply_plan(plan: &ResolvedSetupPlan) -> Vec<ClientSetupResult> {
    let runtime_installed = match plan.runtime.as_ref().map(install_runtime).transpose() {
        Ok(installed) => installed.unwrap_or(false),
        Err(error) => return failed_results(&plan.edits, error.to_string()),
    };
    if let Err(error) =
        preflight_edits(&plan.edits).and_then(|()| preflight_discovery(&plan.discovery_edits))
    {
        if runtime_installed && let Some(runtime) = &plan.runtime {
            let _ = fs::remove_file(&runtime.destination);
        }
        return failed_results(&plan.edits, error.to_string());
    }
    let transaction = match begin_setup_transaction(plan) {
        Ok(transaction) => transaction,
        Err(error) => {
            if runtime_installed && let Some(runtime) = &plan.runtime {
                let _ = fs::remove_file(&runtime.destination);
            }
            return failed_results(&plan.edits, error.to_string());
        }
    };

    let mut applied: Vec<&PlannedClientEdit> = Vec::new();
    let mut applied_discovery: Vec<&PlannedDiscoveryEdit> = Vec::new();
    for edit in &plan.edits {
        if let Err(error) = apply_edit(edit) {
            let rollback = rollback_setup(
                plan,
                runtime_installed,
                &applied,
                &applied_discovery,
                transaction,
            );
            return failed_results(&plan.edits, rollback_message(error, rollback));
        }
        applied.push(edit);
    }
    for edit in &plan.discovery_edits {
        if let Err(error) = apply_discovery_edit(edit) {
            let rollback = rollback_setup(
                plan,
                runtime_installed,
                &applied,
                &applied_discovery,
                transaction,
            );
            return failed_results(&plan.edits, rollback_message(error, rollback));
        }
        applied_discovery.push(edit);
    }
    if let Some(transaction) = transaction
        && let Err(error) = transaction.commit()
    {
        return failed_results(&plan.edits, error.to_string());
    }
    plan.edits
        .iter()
        .map(|edit| ClientSetupResult {
            client: edit.public.client,
            path: edit.public.path.clone(),
            status: edit.status.to_string(),
            error: None,
        })
        .collect()
}

fn rollback_setup(
    plan: &ResolvedSetupPlan,
    runtime_installed: bool,
    applied: &[&PlannedClientEdit],
    applied_discovery: &[&PlannedDiscoveryEdit],
    transaction: Option<SetupTransaction>,
) -> Result<()> {
    for edit in applied_discovery.iter().rev() {
        restore_discovery_edit(edit)?;
    }
    for edit in applied.iter().rev() {
        restore_edit(edit)?;
    }
    if runtime_installed && let Some(runtime) = &plan.runtime {
        let _ = fs::remove_file(&runtime.destination);
    }
    if let Some(transaction) = transaction {
        transaction.commit()?;
    }
    Ok(())
}

fn rollback_message(error: Error, rollback: Result<()>) -> String {
    match rollback {
        Ok(()) => format!("setup transaction rolled back: {error}"),
        Err(rollback_error) => format!(
            "setup transaction failed: {error}; rollback requires recovery: {rollback_error}"
        ),
    }
}

fn preflight_edits(edits: &[PlannedClientEdit]) -> Result<()> {
    for edit in edits {
        if read_optional(&edit.public.path)? != edit.original {
            return Err(Error::InternalFailure(format!(
                "configuration changed after preflight: {}",
                edit.public.path.display()
            )));
        }
    }
    Ok(())
}

fn preflight_discovery(edits: &[PlannedDiscoveryEdit]) -> Result<()> {
    for edit in edits {
        if read_optional(&edit.public.path)? != edit.original {
            return Err(Error::InternalFailure(format!(
                "discovery skill changed after preflight: {}",
                edit.public.path.display()
            )));
        }
    }
    Ok(())
}

fn failed_results(edits: &[PlannedClientEdit], error: String) -> Vec<ClientSetupResult> {
    edits
        .iter()
        .map(|edit| ClientSetupResult {
            client: edit.public.client,
            path: edit.public.path.clone(),
            status: "failed".into(),
            error: Some(error.clone()),
        })
        .collect()
}

fn restore_edit(edit: &PlannedClientEdit) -> Result<()> {
    restore_path(&edit.public.path, edit.original.as_deref())
}

fn apply_discovery_edit(edit: &PlannedDiscoveryEdit) -> Result<()> {
    match edit.public.action {
        ClientPlanAction::Create | ClientPlanAction::Update => write_if_changed(
            &edit.public.path,
            edit.original.as_deref().unwrap_or_default(),
            edit.updated.as_deref().unwrap_or_default(),
        ),
        ClientPlanAction::Remove => {
            if edit.public.path.exists() {
                fs::remove_file(&edit.public.path)?;
                sync_parent_directory(&edit.public.path)?;
            }
            Ok(())
        }
        ClientPlanAction::AlreadyCurrent | ClientPlanAction::NotConfigured => Ok(()),
    }
}

fn restore_discovery_edit(edit: &PlannedDiscoveryEdit) -> Result<()> {
    restore_path(&edit.public.path, edit.original.as_deref())
}

fn apply_edit(edit: &PlannedClientEdit) -> Result<()> {
    let current = read_optional(&edit.public.path)?;
    if current != edit.original {
        return Err(Error::InternalFailure(format!(
            "configuration changed after preflight: {}",
            edit.public.path.display()
        )));
    }
    if let Some(updated) = &edit.updated {
        write_if_changed(
            &edit.public.path,
            edit.original.as_deref().unwrap_or_default(),
            updated,
        )?;
    }
    Ok(())
}
