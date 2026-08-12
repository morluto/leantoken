use super::*;

pub(super) trait SetupPrompt {
    fn select(
        &self,
        operation: SetupOperation,
        detected: &[SetupClient],
        preferred: &[SetupClient],
    ) -> Result<Option<Vec<SetupClient>>>;

    fn confirm(&self, operation: SetupOperation, plan: &ResolvedSetupPlan) -> Result<bool>;
}

pub(super) struct InteractivePrompt;

#[derive(Clone)]
pub(super) struct AgentOption {
    client: SetupClient,
    detected: bool,
}

impl fmt::Display for AgentOption {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.client.display_name())?;
        if self.detected {
            formatter.write_str(" — detected")?;
        }
        Ok(())
    }
}

impl SetupPrompt for InteractivePrompt {
    fn select(
        &self,
        operation: SetupOperation,
        detected: &[SetupClient],
        preferred: &[SetupClient],
    ) -> Result<Option<Vec<SetupClient>>> {
        let stderr = std::io::stderr();
        let mut output = stderr.lock();
        writeln!(output, "◆ LeanToken // Context Distillery")?;
        writeln!(
            output,
            "  Detected or configured agents are shown first and selected by default."
        )?;
        writeln!(output)?;
        drop(output);
        let ordered = SetupClient::ALL
            .into_iter()
            .filter(|client| preferred.contains(client))
            .chain(
                SetupClient::ALL
                    .into_iter()
                    .filter(|client| !preferred.contains(client)),
            )
            .collect::<Vec<_>>();
        let defaults = (0..preferred.len()).collect::<Vec<_>>();
        let options = ordered
            .iter()
            .copied()
            .map(|client| AgentOption {
                client,
                detected: detected.contains(&client),
            })
            .collect::<Vec<_>>();
        match MultiSelect::new(operation.selection_prompt(), options)
            .with_default(&defaults)
            .without_filtering()
            .with_help_message("↑/↓ move • Space select • Enter continue • Esc cancel")
            .prompt_skippable()
        {
            Ok(selection) => {
                Ok(selection
                    .map(|options| options.into_iter().map(|option| option.client).collect()))
            }
            Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => Ok(None),
            Err(error) => Err(prompt_error(error)),
        }
    }

    fn confirm(&self, operation: SetupOperation, plan: &ResolvedSetupPlan) -> Result<bool> {
        print_preflight(plan)?;
        match Confirm::new(&format!("{} these changes?", operation.action_label()))
            .with_default(false)
            .prompt()
        {
            Ok(answer) => Ok(answer),
            Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => Ok(false),
            Err(error) => Err(prompt_error(error)),
        }
    }
}

pub(super) fn prompt_error(error: InquireError) -> Error {
    Error::SetupFailure(format!("interactive setup failed: {error}"))
}
