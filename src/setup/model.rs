/// Setup or removal operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupOperation {
    /// Add or update global MCP entries.
    Setup,
    /// Remove global MCP entries.
    Remove,
}

impl SetupOperation {
    fn action(self) -> &'static str {
        match self {
            Self::Setup => "set up",
            Self::Remove => "remove",
        }
    }

    fn action_label(self) -> &'static str {
        match self {
            Self::Setup => "Set up",
            Self::Remove => "Remove",
        }
    }

    fn selection_prompt(self) -> &'static str {
        match self {
            Self::Setup => "Which coding agents should use LeanToken?",
            Self::Remove => "Remove LeanToken from which coding agents?",
        }
    }

    fn plan_label(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Remove => "removal",
        }
    }
}

/// Parsed request for the setup or removal workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupRequest {
    /// Explicitly selected clients.
    pub clients: Vec<SetupClient>,
    /// Select every supported client.
    pub all: bool,
    /// Refresh every existing LeanToken entry without selecting new clients.
    pub refresh: bool,
    /// Install and register a direct application-owned native runtime.
    pub private_runtime: bool,
    /// Apply an explicitly scoped plan without interactive confirmation.
    pub yes: bool,
    /// Resolve and print the setup plan without changing configuration.
    pub dry_run: bool,
    /// Permit an intentionally selected older npx release.
    pub allow_outdated: bool,
}

/// Planned action for one client configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientPlanAction {
    /// Create a new configuration file.
    Create,
    /// Update an existing configuration file.
    Update,
    /// The requested setup is already current.
    AlreadyCurrent,
    /// Remove an existing LeanToken entry.
    Remove,
    /// No LeanToken entry exists to remove.
    NotConfigured,
}

impl fmt::Display for ClientPlanAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::AlreadyCurrent => "already current",
            Self::Remove => "remove",
            Self::NotConfigured => "not configured",
        };
        formatter.write_str(label)
    }
}

/// Public, secret-free description of one planned client effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSetupPlan {
    /// Client whose global configuration was inspected.
    pub client: SetupClient,
    /// Exact global configuration path.
    pub path: PathBuf,
    /// Resolved action for the current state.
    pub action: ClientPlanAction,
    /// Whether local client state was detected.
    pub detected: bool,
}

/// One agent-discovery artifact owned by LeanToken setup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoverySetupPlan {
    /// Host-native skill path.
    pub path: PathBuf,
    /// Resolved action for the current state.
    pub action: ClientPlanAction,
}

/// Exact MCP launcher that setup will register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LauncherPlan {
    /// Executable written to client configuration.
    pub command: String,
    /// Arguments written to client configuration.
    pub args: Vec<String>,
    /// Exact LeanToken version represented by this launcher.
    pub version: String,
    /// Exact npm package specifier, when the launcher uses npm.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Whether client startup may contact the package registry.
    pub may_contact_network: bool,
    /// Application-owned native runtime path, when private-runtime mode is selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<PathBuf>,
    /// BLAKE3 digest of the native executable installed at `runtime_path`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_digest: Option<String>,
}

/// Outcome for one client configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSetupResult {
    /// Client that was processed.
    pub client: SetupClient,
    /// Global configuration path.
    pub path: PathBuf,
    /// Human-readable result status.
    pub status: String,
    /// Failure detail when configuration was not changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Aggregate setup or removal report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupReport {
    /// Operation that produced the report.
    pub operation: SetupOperation,
    /// Whether an interactive user cancelled before mutation.
    pub cancelled: bool,
    /// Whether this report describes a dry-run without mutation.
    pub dry_run: bool,
    /// Whether setup ran from a persistent CLI installation.
    pub persistent_cli: bool,
    /// Exact launcher considered for setup, omitted for removal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launcher: Option<LauncherPlan>,
    /// Secret-free resolved plan used for confirmation and execution.
    pub plan: Vec<ClientSetupPlan>,
    /// Agent-visible discovery artifacts included in the same transaction.
    pub discovery_plan: Vec<DiscoverySetupPlan>,
    /// Exact cl100k token count of one managed discovery skill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery_skill_tokens: Option<usize>,
    /// Per-client outcomes.
    pub results: Vec<ClientSetupResult>,
}

impl SetupReport {
    /// Return true when at least one selected client failed.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.results.iter().any(|result| result.error.is_some())
    }
}
