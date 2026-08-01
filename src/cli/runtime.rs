use super::*;

/// Private-runtime operation.
#[derive(Debug, Clone, Args)]
pub struct RuntimeArgs {
    /// Runtime subcommand.
    #[command(subcommand)]
    pub command: RuntimeCommand,
}

/// Commands for application-owned native runtimes.
#[derive(Debug, Clone, Subcommand)]
pub enum RuntimeCommand {
    /// List installed versions, sizes, and client references.
    List,
    /// Remove unreferenced versions outside the retention window.
    Prune(RuntimePruneArgs),
}

/// Selection and consent for `runtime prune`.
#[derive(Debug, Clone, Args)]
pub struct RuntimePruneArgs {
    /// Newest unreferenced runtimes to retain.
    #[arg(
        long,
        value_name = "COUNT",
        default_value_t = crate::setup::DEFAULT_RUNTIME_RETENTION,
        value_parser = parse_runtime_retention
    )]
    pub keep_latest: usize,
    /// Show the exact prune plan without deleting runtimes.
    #[arg(long)]
    pub dry_run: bool,
    /// Apply the prune plan without prompting.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

fn parse_runtime_retention(value: &str) -> std::result::Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|_| "retention must be a non-negative integer".to_owned())?;
    if value > crate::setup::MAX_RUNTIME_RETENTION {
        return Err(format!(
            "retention must not exceed {}",
            crate::setup::MAX_RUNTIME_RETENTION
        ));
    }
    Ok(value)
}

impl From<RuntimePruneArgs> for crate::setup::RuntimePruneRequest {
    fn from(args: RuntimePruneArgs) -> Self {
        Self {
            keep_latest: args.keep_latest,
            dry_run: args.dry_run || !args.yes,
            yes: args.yes,
        }
    }
}
