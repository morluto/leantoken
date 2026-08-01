use super::*;

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
    pub(super) fn action(self) -> &'static str {
        match self {
            Self::Setup => "set up",
            Self::Remove => "remove",
        }
    }

    pub(super) fn action_label(self) -> &'static str {
        match self {
            Self::Setup => "Set up",
            Self::Remove => "Remove",
        }
    }

    pub(super) fn selection_prompt(self) -> &'static str {
        match self {
            Self::Setup => "Which coding agents should use LeanToken?",
            Self::Remove => "Remove LeanToken from which coding agents?",
        }
    }

    pub(super) fn plan_label(self) -> &'static str {
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
    /// Permit replacing or removing a registration not recognized as setup-managed.
    pub force_unmanaged: bool,
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

/// Outcome of post-configuration MCP launcher verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupVerificationStatus {
    /// The registered launcher satisfied the doctor contract.
    Passed,
    /// The launcher failed at a named doctor boundary.
    Failed,
    /// Configuration failures made launcher verification misleading.
    Skipped,
}

/// Post-configuration verification of the exact registered MCP launcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupVerification {
    /// Typed verification outcome.
    pub status: SetupVerificationStatus,
    /// Stable MCP boundary where verification failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// Bounded diagnostic detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Exact command the user can run to repeat the diagnostic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_command: Option<String>,
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
    /// Whether the caller explicitly allowed effects on an unmanaged registration.
    pub ownership_override: bool,
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
    /// Transaction-wide apply failure, including discovery-only cleanup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_error: Option<String>,
    /// Exact-launcher MCP verification after a setup mutation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<SetupVerification>,
}

impl SetupReport {
    /// Return true when at least one selected client edit failed.
    #[must_use]
    pub fn has_client_failures(&self) -> bool {
        self.results.iter().any(|result| result.error.is_some())
    }

    /// Return true when the setup transaction itself did not complete.
    #[must_use]
    pub fn has_apply_failure(&self) -> bool {
        self.apply_error.is_some()
    }

    /// Return true when post-setup launcher verification failed.
    #[must_use]
    pub fn has_verification_failure(&self) -> bool {
        self.verification
            .as_ref()
            .is_some_and(|verification| verification.status == SetupVerificationStatus::Failed)
    }

    /// Return true when apply, a client edit, or launcher verification failed.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.has_apply_failure() || self.has_client_failures() || self.has_verification_failure()
    }
}

/// One versioned private runtime managed by LeanToken setup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeEntryReport {
    /// Semantic LeanToken release stored in this directory.
    pub version: String,
    /// Exact native executable path.
    pub path: PathBuf,
    /// Native executable size in bytes.
    pub size_bytes: u64,
    /// Configured clients whose launcher references this runtime.
    pub referenced_by: Vec<SetupClient>,
    /// Whether this executable is running the current command.
    pub active: bool,
    /// Whether the directory has the exact safe managed layout required for pruning.
    pub safely_prunable: bool,
}

/// Bounded inventory of application-owned private runtimes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeListReport {
    /// Versioned runtime root inspected by the command.
    pub runtime_root: PathBuf,
    /// Recognized runtime directories.
    pub total_entries: usize,
    /// Aggregate bytes of recognized runtime executables.
    pub total_bytes: u64,
    /// Unrecognized root entries retained without inspection or mutation.
    pub ignored_entries: usize,
    /// Runtime entries in descending semantic-version order.
    pub entries: Vec<RuntimeEntryReport>,
}

/// Selection and consent for private-runtime pruning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimePruneRequest {
    /// Newest unreferenced runtimes to retain in addition to every referenced runtime.
    pub keep_latest: usize,
    /// Resolve the exact deletion plan without mutation.
    pub dry_run: bool,
    /// Apply the deletion plan without prompting.
    pub yes: bool,
}

/// One private-runtime prune decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimePruneResult {
    /// Runtime release considered.
    pub version: String,
    /// Exact executable path.
    pub path: PathBuf,
    /// Bytes represented by this decision.
    pub size_bytes: u64,
    /// `retained`, `would_remove`, `removed`, `partially_removed`, or `failed`.
    pub action: String,
    /// Stable explanation for retaining or selecting the runtime.
    pub reason: String,
    /// Bounded failure detail when deletion did not complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Outcome of a bounded private-runtime prune operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimePruneReport {
    /// Versioned runtime root inspected by the command.
    pub runtime_root: PathBuf,
    /// Whether this report is a non-mutating plan.
    pub dry_run: bool,
    /// Bytes present before pruning.
    pub total_bytes_before: u64,
    /// Bytes retained after completed removals, or projected for a dry-run.
    pub total_bytes_after: u64,
    /// Complete decision for every recognized runtime.
    pub results: Vec<RuntimePruneResult>,
}

impl RuntimePruneReport {
    /// Return true when one or more selected runtimes could not be removed.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.results.iter().any(|result| result.error.is_some())
    }
}
