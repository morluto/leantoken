use super::*;

/// Options shared by `update` and `upgrade`.
#[derive(Debug, Clone, Args)]
pub struct UpgradeArgs {
    /// Check for a newer release without installing it.
    #[arg(long)]
    pub check: bool,

    /// Run the package-manager command without prompting.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

/// MCP stdio transport options.
#[derive(Debug, Clone, Args)]
pub struct McpArgs {
    /// Successful-result representation.
    #[arg(long, value_enum, default_value_t = McpResultMode::Structured)]
    pub result_mode: McpResultMode,
}

/// Client selection and controls shared by `setup` and `remove`.
#[derive(Debug, Clone, Args)]
pub struct IntegrationArgs {
    /// Configure Claude Code.
    #[arg(long)]
    pub claude: bool,
    /// Configure Cursor.
    #[arg(long)]
    pub cursor: bool,
    /// Configure OpenCode.
    #[arg(long)]
    pub opencode: bool,
    /// Configure Codex.
    #[arg(long)]
    pub codex: bool,
    /// Configure Gemini CLI.
    #[arg(long)]
    pub gemini: bool,
    /// Configure Antigravity.
    #[arg(long)]
    pub antigravity: bool,
    /// Select every supported client.
    #[arg(long)]
    pub all: bool,
    /// Apply an explicitly scoped plan without prompting.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Show the exact configuration plan without making changes.
    #[arg(long)]
    pub dry_run: bool,
    /// Replace or remove a `leantoken` entry not recognized as setup-managed.
    #[arg(long)]
    pub force_unmanaged: bool,
}

/// Options specific to the setup workflow.
#[derive(Debug, Clone, Args)]
pub struct SetupArgs {
    /// Client selection and controls shared with removal.
    #[command(flatten)]
    pub integration: IntegrationArgs,
    /// Refresh existing LeanToken MCP entries without selecting new clients.
    #[arg(long)]
    pub refresh: bool,
    /// Recommended: register a verified private native runtime instead of
    /// retaining an npx/Node process chain.
    #[arg(long)]
    pub private_runtime: bool,
    /// Permit setup from an older npx release for an intentional rollback.
    #[arg(long)]
    pub allow_outdated: bool,
}

impl From<IntegrationArgs> for SetupRequest {
    fn from(args: IntegrationArgs) -> Self {
        let mut clients = Vec::new();
        if args.claude {
            clients.push(SetupClient::Claude);
        }
        if args.cursor {
            clients.push(SetupClient::Cursor);
        }
        if args.opencode {
            clients.push(SetupClient::OpenCode);
        }
        if args.codex {
            clients.push(SetupClient::Codex);
        }
        if args.gemini {
            clients.push(SetupClient::Gemini);
        }
        if args.antigravity {
            clients.push(SetupClient::Antigravity);
        }
        Self {
            clients,
            all: args.all,
            refresh: false,
            private_runtime: false,
            yes: args.yes,
            dry_run: args.dry_run,
            allow_outdated: false,
            force_unmanaged: args.force_unmanaged,
        }
    }
}

impl From<SetupArgs> for SetupRequest {
    fn from(args: SetupArgs) -> Self {
        let mut request = Self::from(args.integration);
        request.refresh = args.refresh;
        request.private_runtime = args.private_runtime;
        request.allow_outdated = args.allow_outdated;
        request
    }
}
