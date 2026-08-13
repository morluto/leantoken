use super::*;

/// Local, read-only episode-analysis commands.
#[derive(Debug, Clone, Args)]
pub struct EpisodeArgs {
    /// Episode subcommand.
    #[command(subcommand)]
    pub command: EpisodeCommand,
}

/// Episode-analysis operation.
#[derive(Debug, Clone, Subcommand)]
pub enum EpisodeCommand {
    /// Normalize one existing redacted analyzer report.
    Audit(EpisodeAuditArgs),
}

/// Arguments for `episode audit`.
#[derive(Debug, Clone, Args)]
pub struct EpisodeAuditArgs {
    /// Explicit input adapter and schema version.
    #[arg(long, value_enum)]
    pub adapter: EpisodeAdapterArg,
    /// Existing redacted analyzer report.
    #[arg(long, value_name = "PATH")]
    pub input: PathBuf,
}

/// Versioned existing-report adapter accepted by the episode auditor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EpisodeAdapterArg {
    /// Aggregate produced by `codex_multi_agent_suite`.
    MultiAgentSuiteV1,
    /// Classification produced by `model_ab_trajectory`.
    ModelAbTrajectoryV1,
    /// Version-two report produced by `mcp_wire_analyze`.
    McpWireReportV2,
    /// Publishable receipt produced by `codex_host_receipt`.
    CodexHostReceiptV1,
    /// Classification produced by `context_utilization`.
    ContextUtilizationV1,
}

impl From<EpisodeAuditArgs> for crate::episode::EpisodeAuditRequest {
    fn from(args: EpisodeAuditArgs) -> Self {
        Self {
            adapter: match args.adapter {
                EpisodeAdapterArg::MultiAgentSuiteV1 => {
                    crate::episode::EpisodeAdapter::MultiAgentSuiteV1
                }
                EpisodeAdapterArg::ModelAbTrajectoryV1 => {
                    crate::episode::EpisodeAdapter::ModelAbTrajectoryV1
                }
                EpisodeAdapterArg::McpWireReportV2 => {
                    crate::episode::EpisodeAdapter::McpWireReportV2
                }
                EpisodeAdapterArg::CodexHostReceiptV1 => {
                    crate::episode::EpisodeAdapter::CodexHostReceiptV1
                }
                EpisodeAdapterArg::ContextUtilizationV1 => {
                    crate::episode::EpisodeAdapter::ContextUtilizationV1
                }
            },
            input: args.input,
        }
    }
}
