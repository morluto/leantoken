trait SetupPrompt {
    fn select(
        &self,
        operation: SetupOperation,
        detected: &[SetupClient],
    ) -> Result<Option<Vec<SetupClient>>>;

    fn confirm(&self, operation: SetupOperation, plan: &ResolvedSetupPlan) -> Result<bool>;
}

struct InteractivePrompt;

#[derive(Clone)]
struct AgentOption {
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
    ) -> Result<Option<Vec<SetupClient>>> {
        let stderr = std::io::stderr();
        let mut output = stderr.lock();
        writeln!(output, "◆ LeanToken // Context Distillery")?;
        writeln!(
            output,
            "  Detected agents are labeled for context; none are selected automatically."
        )?;
        writeln!(output)?;
        drop(output);
        let options = SetupClient::ALL
            .iter()
            .copied()
            .map(|client| AgentOption {
                client,
                detected: detected.contains(&client),
            })
            .collect::<Vec<_>>();
        match MultiSelect::new(operation.selection_prompt(), options)
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

fn prompt_error(error: InquireError) -> Error {
    Error::InternalFailure(format!("interactive setup failed: {error}"))
}
