use std::{
    ffi::OsString,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    str::FromStr,
};

use clap::{Args, Command, Parser, Subcommand, ValueEnum};

use crate::Config;
use crate::Result;
use crate::cache::{
    CacheCompatibility, CacheListRequest, CacheListV2Request, CachePruneRequest,
    CachePruneV2Request, CacheState, DEFAULT_CACHE_LIST_LIMIT, MAX_CACHE_LIST_LIMIT,
};
use crate::config::DEFAULT_CONTEXT_TOKENS;
use crate::mcp::McpResultMode;
use crate::model::{
    ContextRequest, FileOperation, FilesRequest, HandoffManifestRequest, HistoryOperation,
    HistoryRequest, IndexConsistency, JsonOperation, JsonProjection, JsonRequest, JsonSelector,
    OutlineRequest, ReadRequest, SearchMode, SearchRequest, WorkflowEvidence,
};
use crate::setup::{SetupClient, SetupRequest};
use crate::tokens::Tokenizer;

fn parse_positive_usize(value: &str) -> std::result::Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|_| "value must be a positive integer".to_owned())?;
    if value == 0 {
        return Err("value must be a positive integer".to_owned());
    }
    Ok(value)
}

fn parse_cache_list_limit(value: &str) -> std::result::Result<usize, String> {
    let value = parse_positive_usize(value)?;
    if value > MAX_CACHE_LIST_LIMIT {
        return Err(format!("must not exceed {MAX_CACHE_LIST_LIMIT}"));
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy)]
struct ScopedGlobalOption {
    id: &'static str,
    long: &'static str,
    advanced: bool,
}

// Keep command-scope policy beside the option identifiers. Clap owns the
// parser shape; this table owns the separate rule that repository-free
// commands must reject repository and tokenizer flags.
const COMMAND_SCOPE_OPTIONS: &[ScopedGlobalOption] = &[
    ScopedGlobalOption {
        id: "root",
        long: "--root",
        advanced: false,
    },
    ScopedGlobalOption {
        id: "allow_broad_root",
        long: "--allow-broad-root",
        advanced: true,
    },
    ScopedGlobalOption {
        id: "include_generated",
        long: "--include-generated",
        advanced: true,
    },
    ScopedGlobalOption {
        id: "max_walk_entries",
        long: "--max-walk-entries",
        advanced: true,
    },
    ScopedGlobalOption {
        id: "max_files",
        long: "--max-files",
        advanced: true,
    },
    ScopedGlobalOption {
        id: "max_total_source_bytes",
        long: "--max-total-source-bytes",
        advanced: true,
    },
    ScopedGlobalOption {
        id: "max_depth",
        long: "--max-depth",
        advanced: true,
    },
    ScopedGlobalOption {
        id: "max_file_bytes",
        long: "--max-file-bytes",
        advanced: true,
    },
    ScopedGlobalOption {
        id: "max_prepare_batch_files",
        long: "--max-prepare-batch-files",
        advanced: true,
    },
    ScopedGlobalOption {
        id: "max_prepare_batch_bytes",
        long: "--max-prepare-batch-bytes",
        advanced: true,
    },
    ScopedGlobalOption {
        id: "max_index_workers",
        long: "--max-index-workers",
        advanced: true,
    },
    ScopedGlobalOption {
        id: "tokenizer",
        long: "--tokenizer",
        advanced: false,
    },
];

fn hide_advanced_repository_options(command: Command) -> Command {
    command
        .mut_args(|argument| {
            if COMMAND_SCOPE_OPTIONS
                .iter()
                .any(|option| option.advanced && option.id == argument.get_id().as_str())
            {
                argument.hide(true)
            } else {
                argument
            }
        })
        .mut_subcommands(|subcommand| subcommand.defer(hide_advanced_repository_options))
}

fn keep_advanced_repository_options_in_root_help(command: Command) -> Command {
    command.mut_subcommands(|subcommand| subcommand.defer(hide_advanced_repository_options))
}

/// LeanToken CLI and MCP server entry point.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "leantoken",
    version,
    about = "Token-budgeted repository context",
    defer = keep_advanced_repository_options_in_root_help
)]
pub struct Cli {
    /// Repository root path.
    #[arg(long, value_name = "PATH", global = true, default_value = ".")]
    pub root: PathBuf,

    /// Allow indexing a filesystem root, home directory, or parent of home.
    #[arg(long, global = true, help_heading = "Advanced repository options")]
    pub allow_broad_root: bool,

    /// Include known generated and package-cache directories.
    #[arg(long, global = true, help_heading = "Advanced repository options")]
    pub include_generated: bool,

    /// Maximum filesystem entries yielded by repository discovery.
    #[arg(
        long,
        value_name = "COUNT",
        global = true,
        help_heading = "Advanced repository options"
    )]
    pub max_walk_entries: Option<NonZeroU64>,

    /// Maximum files admitted to the repository index.
    #[arg(
        long,
        value_name = "COUNT",
        global = true,
        help_heading = "Advanced repository options"
    )]
    pub max_files: Option<NonZeroU64>,

    /// Maximum aggregate bytes admitted to the repository index.
    #[arg(
        long,
        value_name = "BYTES",
        global = true,
        help_heading = "Advanced repository options"
    )]
    pub max_total_source_bytes: Option<NonZeroU64>,

    /// Maximum repository-relative traversal depth.
    #[arg(
        long,
        value_name = "DEPTH",
        global = true,
        help_heading = "Advanced repository options"
    )]
    pub max_depth: Option<NonZeroUsize>,

    /// Maximum bytes admitted from one file.
    #[arg(
        long,
        value_name = "BYTES",
        global = true,
        help_heading = "Advanced repository options"
    )]
    pub max_file_bytes: Option<NonZeroU64>,

    /// Maximum files scheduled in one preparation batch.
    #[arg(
        long,
        value_name = "COUNT",
        global = true,
        help_heading = "Advanced repository options"
    )]
    pub max_prepare_batch_files: Option<NonZeroUsize>,

    /// Maximum source bytes scheduled in one preparation batch.
    #[arg(
        long,
        value_name = "BYTES",
        global = true,
        help_heading = "Advanced repository options"
    )]
    pub max_prepare_batch_bytes: Option<NonZeroU64>,

    /// Maximum parallel file-preparation workers.
    #[arg(
        long,
        value_name = "COUNT",
        global = true,
        help_heading = "Advanced repository options"
    )]
    pub max_index_workers: Option<NonZeroUsize>,

    /// SQLite database path.
    #[arg(long, value_name = "PATH", global = true)]
    pub database: Option<PathBuf>,

    /// Emit compact JSON; retrieval commands use pretty JSON by default.
    #[arg(long, global = true)]
    pub json: bool,

    /// Tokenizer used for source and protocol token accounting.
    #[arg(long, value_enum, value_name = "ENCODING", default_value_t = Tokenizer::default(), global = true)]
    pub tokenizer: Tokenizer,

    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
    /// Reject repository-scoped global options for repository-free commands.
    ///
    /// Clap propagates global arguments to every subcommand. Checking the
    /// original argument tokens preserves before/after-subcommand placement
    /// for repository commands while distinguishing an explicit default value
    /// such as `--root .` from an omitted option.
    pub fn validate_option_scope(
        &self,
        arguments: &[OsString],
    ) -> std::result::Result<(), clap::Error> {
        if !matches!(
            self.command,
            Commands::Setup(_)
                | Commands::Remove(_)
                | Commands::Cache(_)
                | Commands::Update(_)
                | Commands::Upgrade(_)
        ) {
            return Ok(());
        }
        let supplied = arguments.iter().skip(1).find_map(|argument| {
            let argument = argument.as_os_str().as_encoded_bytes();
            COMMAND_SCOPE_OPTIONS
                .iter()
                .map(|option| option.long)
                .find(|option| {
                    argument == option.as_bytes()
                        || argument
                            .strip_prefix(option.as_bytes())
                            .is_some_and(|suffix| suffix.starts_with(b"="))
                })
        });
        let Some(option) = supplied else {
            return Ok(());
        };
        let command = match self.command {
            Commands::Setup(_) => "setup",
            Commands::Remove(_) => "remove",
            Commands::Cache(_) => "cache",
            Commands::Update(_) => "update",
            Commands::Upgrade(_) => "upgrade",
            _ => unreachable!("repository-free commands checked above"),
        };
        Err(clap::Error::raw(
            clap::error::ErrorKind::ArgumentConflict,
            format!("repository option {option} cannot be used with `{command}`"),
        ))
    }

    /// Resolve global options into a [`Config`].
    ///
    /// # Errors
    ///
    /// Returns an error when the repository root cannot be canonicalized or is
    /// an unsafe broad root without the explicit override.
    pub fn config(&self) -> Result<Config> {
        let mut config = Config::discover_with_broad_root(
            &self.root,
            self.database.clone(),
            self.allow_broad_root,
        )?;
        if let Some(value) = self.max_walk_entries {
            config.max_walk_entries = value.get();
        }
        if let Some(value) = self.max_files {
            config.max_files = value.get();
        }
        if let Some(value) = self.max_total_source_bytes {
            config.max_total_source_bytes = value.get();
        }
        if let Some(value) = self.max_depth {
            config.max_depth = value.get();
        }
        if let Some(value) = self.max_file_bytes {
            config.max_file_bytes = value.get();
        }
        if let Some(value) = self.max_prepare_batch_files {
            config.max_prepare_batch_files = value.get();
        }
        if let Some(value) = self.max_prepare_batch_bytes {
            config.max_prepare_batch_bytes = value.get();
        }
        if let Some(value) = self.max_index_workers {
            config.max_index_workers = value.get();
        }
        config.include_generated = self.include_generated;
        config.tokenizer = self.tokenizer;
        config.discovery_limits().validate()?;
        Ok(config)
    }

    /// Return the consistency boundary requested by an index-backed retrieval.
    #[must_use]
    pub fn retrieval_consistency(&self) -> Option<IndexConsistency> {
        let consistency = match &self.command {
            Commands::Files(args) => args.index_consistency.consistency,
            Commands::Search(args) => args.index_consistency.consistency,
            Commands::Outline(args) => args.index_consistency.consistency,
            Commands::Read(args) => args.index_consistency.consistency,
            Commands::Context(args) => args.index_consistency.consistency,
            _ => return None,
        };
        Some(consistency.into())
    }

    /// Convert the parsed CLI into an application request.
    pub fn app_request(self) -> AppRequest {
        match self.command {
            Commands::Index { rebuild } => AppRequest::Index { rebuild },
            Commands::Status => AppRequest::Status,
            Commands::Savings => AppRequest::Savings,
            Commands::Files(args) => {
                let max_response_tokens = args.max_response_tokens;
                let request: FilesRequest = args.into();
                max_response_tokens.map_or(AppRequest::Files(request.clone()), |limit| {
                    AppRequest::FilesWithOptions {
                        request,
                        max_response_tokens: limit,
                    }
                })
            }
            Commands::Search(args) => {
                let max_response_tokens = args.max_response_tokens;
                let request: SearchRequest = args.into();
                max_response_tokens.map_or(AppRequest::Search(request.clone()), |limit| {
                    AppRequest::SearchWithOptions {
                        request,
                        max_response_tokens: limit,
                    }
                })
            }
            Commands::Outline(args) => {
                let max_response_tokens = args.max_response_tokens;
                let request: OutlineRequest = args.into();
                max_response_tokens.map_or(AppRequest::Outline(request.clone()), |limit| {
                    AppRequest::OutlineWithOptions {
                        request,
                        max_response_tokens: limit,
                    }
                })
            }
            Commands::Read(args) => {
                let max_response_tokens = args.max_response_tokens;
                let request: ReadRequest = args.into();
                max_response_tokens.map_or(AppRequest::Read(request.clone()), |limit| {
                    AppRequest::ReadWithOptions {
                        request,
                        max_response_tokens: limit,
                    }
                })
            }
            Commands::History(args) => {
                let max_response_tokens = args.max_response_tokens;
                let request: HistoryRequest = args.into();
                max_response_tokens.map_or(AppRequest::History(request.clone()), |limit| {
                    AppRequest::HistoryWithOptions {
                        request,
                        max_response_tokens: limit,
                    }
                })
            }
            Commands::Json(args) => {
                let max_response_tokens = args.max_response_tokens;
                let request: JsonRequest = args.into();
                max_response_tokens.map_or(AppRequest::Json(request.clone()), |limit| {
                    AppRequest::JsonWithOptions {
                        request,
                        max_response_tokens: limit,
                    }
                })
            }
            Commands::Context(args) => {
                let workflow = args.workflow.into();
                let handoff = args.handoff_request();
                let workflow_evidence = args.workflow_evidence();
                let max_response_tokens = args.max_response_tokens;
                AppRequest::Context {
                    request: args.into(),
                    workflow,
                    workflow_evidence,
                    handoff: handoff.map(Box::new),
                    max_response_tokens,
                }
            }
            Commands::Doctor => AppRequest::Doctor,
            Commands::Mcp(args) => AppRequest::Mcp {
                result_mode: args.result_mode,
            },
            Commands::Setup(args) => AppRequest::Setup(args.into()),
            Commands::Remove(args) => AppRequest::Remove(args.into()),
            Commands::Cache(args) => match args.command {
                CacheCommand::List(args) => AppRequest::CacheListV2(args.into()),
                CacheCommand::Prune(args) => AppRequest::CachePruneV2(args.into()),
            },
            Commands::Update(args) | Commands::Upgrade(args) => AppRequest::Upgrade {
                check: args.check,
                yes: args.yes,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn scoped_global_option_registry_matches_clap_arguments() {
        let command = Cli::command();
        for option in COMMAND_SCOPE_OPTIONS {
            assert!(
                command
                    .get_arguments()
                    .any(|argument| argument.get_id().as_str() == option.id),
                "scoped option {} is not defined by Clap",
                option.id
            );
        }
    }
}

/// Parsed application request produced by the CLI.
#[derive(Debug, Clone)]
pub enum AppRequest {
    Index {
        rebuild: bool,
    },
    Status,
    Savings,
    Files(FilesRequest),
    Search(SearchRequest),
    Outline(OutlineRequest),
    Read(ReadRequest),
    History(HistoryRequest),
    Json(JsonRequest),
    FilesWithOptions {
        request: FilesRequest,
        max_response_tokens: usize,
    },
    SearchWithOptions {
        request: SearchRequest,
        max_response_tokens: usize,
    },
    OutlineWithOptions {
        request: OutlineRequest,
        max_response_tokens: usize,
    },
    ReadWithOptions {
        request: ReadRequest,
        max_response_tokens: usize,
    },
    HistoryWithOptions {
        request: HistoryRequest,
        max_response_tokens: usize,
    },
    JsonWithOptions {
        request: JsonRequest,
        max_response_tokens: usize,
    },
    Context {
        request: ContextRequest,
        workflow: crate::model::ContextWorkflow,
        workflow_evidence: WorkflowEvidence,
        handoff: Option<Box<HandoffManifestRequest>>,
        max_response_tokens: Option<usize>,
    },
    Doctor,
    Mcp {
        result_mode: McpResultMode,
    },
    Setup(SetupRequest),
    Remove(SetupRequest),
    CacheList(CacheListRequest),
    CachePrune(CachePruneRequest),
    CacheListV2(CacheListV2Request),
    CachePruneV2(CachePruneV2Request),
    Upgrade {
        check: bool,
        yes: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum Commands {
    /// Index the repository.
    Index {
        /// Rebuild the index from scratch.
        #[arg(long)]
        rebuild: bool,
    },

    /// Show index status.
    Status,

    /// Show source compression and full-response token accounting.
    Savings,

    /// List, find, or glob repository paths.
    Files(FilesArgs),

    /// Search the repository for terms, symbols, or references.
    Search(SearchArgs),

    /// Show the structural outline of one or more files.
    Outline(OutlineArgs),

    /// Read a bounded source range.
    Read(ReadArgs),

    /// Read, diff, or trace a symbol across Git revisions.
    History(HistoryArgs),

    /// Query, summarize, or compare live JSON structures.
    Json(JsonArgs),

    /// Retrieve ranked task context within a token budget.
    Context(ContextArgs),

    /// Verify MCP identity, tools, and first-retrieval readiness.
    Doctor,

    /// Run the MCP server over stdio.
    Mcp(McpArgs),

    /// Configure LeanToken as a global MCP server for coding clients.
    Setup(IntegrationArgs),

    /// Remove LeanToken's global MCP server entries.
    Remove(IntegrationArgs),

    /// Inspect or prune centrally managed repository caches.
    Cache(CacheArgs),

    /// Update LeanToken to the latest release.
    Update(UpgradeArgs),

    /// Update LeanToken to the latest release.
    Upgrade(UpgradeArgs),
}

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
    /// Successful-result representation. Keep `dual` unless the host is known
    /// to consume structured-only results.
    #[arg(long, value_enum, default_value_t = McpResultMode::Dual)]
    pub result_mode: McpResultMode,
}

/// Client selection shared by `setup` and `remove`.
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
    /// Refresh existing LeanToken MCP entries without selecting new clients.
    #[arg(long)]
    pub refresh: bool,
    /// Copy the verified native executable into LeanToken's private runtime
    /// directory and register it directly instead of retaining an npx chain.
    #[arg(long)]
    pub private_runtime: bool,
    /// Apply without prompting; requires explicit clients, --all, or --refresh.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Show the exact configuration plan without making changes.
    #[arg(long)]
    pub dry_run: bool,
    /// Permit setup from an older npx release for an intentional rollback.
    #[arg(long)]
    pub allow_outdated: bool,
}

/// Managed cache operation.
#[derive(Debug, Clone, Args)]
pub struct CacheArgs {
    /// Cache subcommand.
    #[command(subcommand)]
    pub command: CacheCommand,
}

/// Commands for centrally managed repository caches.
#[derive(Debug, Clone, Subcommand)]
pub enum CacheCommand {
    /// List managed caches, sizes, roots, access times, and active leases.
    List(CacheListArgs),
    /// Remove inactive managed caches selected by explicit criteria.
    Prune(CachePruneArgs),
}

/// Filters and response bounds for `cache list`.
#[derive(Debug, Clone, Args)]
pub struct CacheListArgs {
    /// Return aggregate diagnostics without per-cache entries.
    #[arg(long, conflicts_with = "cursor")]
    pub summary: bool,
    /// Keep caches in this metadata state (repeatable).
    #[arg(long, value_enum, value_name = "STATE")]
    pub state: Vec<CacheStateArg>,
    /// Keep caches in this content-compatibility class (repeatable).
    #[arg(long, value_enum, value_name = "COMPATIBILITY")]
    pub compatibility: Vec<CacheCompatibilityArg>,
    /// Keep caches with this exact index-content version (repeatable).
    #[arg(long, value_name = "VERSION")]
    pub index_content_version: Vec<u32>,
    /// Keep only older or legacy-unversioned content.
    #[arg(long)]
    pub incompatible_with_current: bool,
    /// Keep the exact recorded repository root.
    #[arg(long, value_name = "PATH")]
    pub repository_root: Option<PathBuf>,
    /// Maximum entries returned by one page (1-100).
    #[arg(
        long,
        default_value_t = DEFAULT_CACHE_LIST_LIMIT,
        value_parser = parse_cache_list_limit
    )]
    pub limit: usize,
    /// Continue from an opaque cursor returned by the same filters.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
}

/// Cache metadata state accepted by `cache list --state`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CacheStateArg {
    /// Current schema and access metadata.
    Current,
    /// Readable older schema without current access metadata.
    Legacy,
    /// Known artifacts without a readable database.
    Incomplete,
    /// SQLite metadata inspection failed.
    Corrupt,
    /// Newer or mismatched metadata unsafe for this binary.
    Unsupported,
    /// Unexpected directory content.
    Unrecognized,
}

/// Cache content compatibility accepted by `cache list --compatibility`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CacheCompatibilityArg {
    /// Content produced by the current index-content version.
    CompatibleCurrent,
    /// Content produced by a known older version.
    ObsoleteOlder,
    /// Legacy content without a versioned cache identity.
    LegacyUnversioned,
    /// Content produced by a newer unsupported version.
    NewerUnsupported,
    /// Content whose compatibility cannot be trusted.
    Unknown,
}

impl From<CacheStateArg> for CacheState {
    fn from(value: CacheStateArg) -> Self {
        match value {
            CacheStateArg::Current => Self::Current,
            CacheStateArg::Legacy => Self::Legacy,
            CacheStateArg::Incomplete => Self::Incomplete,
            CacheStateArg::Corrupt => Self::Corrupt,
            CacheStateArg::Unsupported => Self::Unsupported,
            CacheStateArg::Unrecognized => Self::Unrecognized,
        }
    }
}

impl From<CacheCompatibilityArg> for CacheCompatibility {
    fn from(value: CacheCompatibilityArg) -> Self {
        match value {
            CacheCompatibilityArg::CompatibleCurrent => Self::CompatibleCurrent,
            CacheCompatibilityArg::ObsoleteOlder => Self::ObsoleteOlder,
            CacheCompatibilityArg::LegacyUnversioned => Self::LegacyUnversioned,
            CacheCompatibilityArg::NewerUnsupported => Self::NewerUnsupported,
            CacheCompatibilityArg::Unknown => Self::Unknown,
        }
    }
}

impl From<CacheListArgs> for CacheListV2Request {
    fn from(args: CacheListArgs) -> Self {
        Self {
            request: CacheListRequest {
                summary: args.summary,
                states: args.state.into_iter().map(Into::into).collect(),
                repository_root: args.repository_root,
                limit: args.limit,
                cursor: args.cursor,
            },
            compatibilities: args.compatibility.into_iter().map(Into::into).collect(),
            index_content_versions: args.index_content_version,
            incompatible_with_current: args.incompatible_with_current,
        }
    }
}

/// Selection and consent for `cache prune`.
#[derive(Debug, Clone, Args)]
pub struct CachePruneArgs {
    /// Remove caches not accessed for at least this many days.
    #[arg(long, value_name = "DAYS")]
    pub older_than: Option<NonZeroU64>,
    /// Reduce managed cache storage to at most this many bytes using LRU order.
    #[arg(long, value_name = "BYTES")]
    pub max_total_bytes: Option<u64>,
    /// Remove caches whose recorded repository roots are currently missing.
    #[arg(long)]
    pub remove_missing_roots: bool,
    /// Select inactive older or legacy-unversioned caches.
    ///
    /// Without `--yes`, this criterion defaults to a dry-run.
    #[arg(long)]
    pub incompatible_with_current: bool,
    /// Show the exact prune plan without deleting files.
    #[arg(long)]
    pub dry_run: bool,
    /// Apply the prune plan without prompting.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

impl From<CachePruneArgs> for CachePruneV2Request {
    fn from(args: CachePruneArgs) -> Self {
        Self {
            request: CachePruneRequest {
                older_than_days: args.older_than.map(NonZeroU64::get),
                max_total_bytes: args.max_total_bytes,
                remove_missing_roots: args.remove_missing_roots,
                dry_run: args.dry_run || (args.incompatible_with_current && !args.yes),
                yes: args.yes,
            },
            incompatible_with_current: args.incompatible_with_current,
        }
    }
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
            refresh: args.refresh,
            private_runtime: args.private_runtime,
            yes: args.yes,
            dry_run: args.dry_run,
            allow_outdated: args.allow_outdated,
        }
    }
}

/// Clap value for the `files` operation.
#[derive(Debug, Clone, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum FileOperationArg {
    Tree,
    Find,
    Glob,
}

impl From<FileOperationArg> for FileOperation {
    fn from(value: FileOperationArg) -> Self {
        match value {
            FileOperationArg::Tree => FileOperation::Tree,
            FileOperationArg::Find => FileOperation::Find,
            FileOperationArg::Glob => FileOperation::Glob,
        }
    }
}

/// Clap value for the `search` mode.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum SearchModeArg {
    #[default]
    Auto,
    Text,
    Regex,
    Identifier,
    Symbol,
    Reference,
}

impl From<SearchModeArg> for SearchMode {
    fn from(value: SearchModeArg) -> Self {
        match value {
            SearchModeArg::Auto => SearchMode::Auto,
            SearchModeArg::Text => SearchMode::Text,
            SearchModeArg::Regex => SearchMode::Regex,
            SearchModeArg::Identifier => SearchMode::Identifier,
            SearchModeArg::Symbol => SearchMode::Symbol,
            SearchModeArg::Reference => SearchMode::Reference,
        }
    }
}

/// Clap value for the index consistency boundary.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum IndexConsistencyArg {
    /// Query the latest completed index generation without scanning live changes.
    IndexedGeneration,
    /// Reconcile the live working tree before retrieval.
    #[default]
    ReconcileWorkingTree,
}

impl From<IndexConsistencyArg> for IndexConsistency {
    fn from(value: IndexConsistencyArg) -> Self {
        match value {
            IndexConsistencyArg::IndexedGeneration => Self::IndexedGeneration,
            IndexConsistencyArg::ReconcileWorkingTree => Self::ReconcileWorkingTree,
        }
    }
}

/// Consistency options shared by index-backed CLI retrievals.
#[derive(Debug, Clone, Args)]
pub struct RetrievalConsistencyArgs {
    /// Index consistency boundary applied before retrieval.
    #[arg(long, value_enum, default_value_t = IndexConsistencyArg::ReconcileWorkingTree)]
    pub consistency: IndexConsistencyArg,
}

#[derive(Debug, Clone, Parser)]
pub struct FilesArgs {
    /// Files operation to perform.
    pub operation: FileOperationArg,

    /// Consistency boundary for this retrieval.
    #[command(flatten)]
    pub index_consistency: RetrievalConsistencyArgs,

    /// Starting path or path filter.
    #[arg(short, long)]
    pub path: Option<String>,

    /// Fuzzy path or basename query.
    #[arg(short, long)]
    pub query: Option<String>,

    /// Glob pattern.
    #[arg(long)]
    pub pattern: Option<String>,

    /// Maximum number of results.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_results: Option<usize>,

    /// Maximum tokens in the final serialized JSON service response.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_response_tokens: Option<usize>,

    /// Pagination cursor.
    #[arg(long)]
    pub cursor: Option<String>,

    /// Maximum directory depth for tree.
    #[arg(long)]
    pub depth: Option<usize>,
}

impl From<FilesArgs> for FilesRequest {
    fn from(args: FilesArgs) -> Self {
        Self {
            operation: args.operation.into(),
            path: args.path,
            query: args.query,
            pattern: args.pattern,
            max_results: args.max_results,
            cursor: args.cursor,
            depth: args.depth,
        }
    }
}

#[derive(Debug, Clone, Parser)]
pub struct SearchArgs {
    /// Search query.
    pub query: String,

    /// Consistency boundary for this retrieval.
    #[command(flatten)]
    pub index_consistency: RetrievalConsistencyArgs,

    /// Search mode.
    #[arg(short, long, value_enum, default_value_t = SearchModeArg::Auto)]
    pub mode: SearchModeArg,

    /// Include only paths matching this pattern (repeatable).
    #[arg(long = "include")]
    pub include_paths: Vec<String>,

    /// Exclude paths matching this pattern (repeatable).
    #[arg(long = "exclude")]
    pub exclude_paths: Vec<String>,

    /// Focus on paths matching this pattern (repeatable).
    #[arg(long = "focus")]
    pub focus_paths: Vec<String>,

    /// Maximum number of results.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_results: Option<usize>,

    /// Maximum tokens to return.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_tokens: Option<usize>,

    /// Maximum tokens in the final serialized JSON service response.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_response_tokens: Option<usize>,

    /// Lines of context around each match.
    #[arg(long)]
    pub context_lines: Option<usize>,

    /// Perform a case-sensitive search.
    #[arg(long)]
    pub case_sensitive: bool,

    /// Return every text or regex occurrence with exact coordinates and counts.
    #[arg(long)]
    pub all_occurrences: bool,

    /// Prefer structural definitions when identifier channels find the same definition.
    #[arg(long)]
    pub prefer_structural: bool,

    /// Pagination cursor.
    #[arg(long)]
    pub cursor: Option<String>,
}

impl From<SearchArgs> for SearchRequest {
    fn from(args: SearchArgs) -> Self {
        Self {
            query: args.query,
            mode: args.mode.into(),
            include_paths: args.include_paths,
            exclude_paths: args.exclude_paths,
            focus_paths: args.focus_paths,
            max_results: args.max_results,
            max_tokens: args.max_tokens,
            context_lines: args.context_lines,
            case_sensitive: args.case_sensitive,
            all_occurrences: args.all_occurrences,
            prefer_structural: args.prefer_structural,
            receipt_id: None,
            cursor: args.cursor,
        }
    }
}

#[derive(Debug, Clone, Parser)]
pub struct OutlineArgs {
    /// Paths to outline.
    pub paths: Vec<String>,

    /// Consistency boundary for this retrieval.
    #[command(flatten)]
    pub index_consistency: RetrievalConsistencyArgs,

    /// Filter by symbol name.
    #[arg(long)]
    pub symbol_name: Option<String>,

    /// Filter by symbol kind.
    #[arg(long)]
    pub symbol_kind: Option<String>,

    /// Maximum number of symbols and imports.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_results: Option<usize>,

    /// Maximum tokens to return.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_tokens: Option<usize>,

    /// Maximum tokens in the final serialized JSON service response.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_response_tokens: Option<usize>,

    /// Continue a result-limited outline.
    #[arg(long)]
    pub cursor: Option<String>,
}

impl From<OutlineArgs> for OutlineRequest {
    fn from(args: OutlineArgs) -> Self {
        Self {
            paths: args.paths,
            symbol_name: args.symbol_name,
            symbol_kind: args.symbol_kind,
            max_results: args.max_results,
            max_tokens: args.max_tokens,
            receipt_id: None,
            cursor: args.cursor,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LineRange {
    pub start: Option<usize>,
    pub end: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct LineRangeError(String);

impl std::fmt::Display for LineRangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for LineRangeError {}

impl FromStr for LineRange {
    type Err = LineRangeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(LineRangeError("line range must not be empty".into()));
        }

        if let Some(pos) = s.find(':') {
            let start_str = &s[..pos];
            let end_str = &s[pos + 1..];

            let start = if start_str.is_empty() {
                None
            } else {
                Some(
                    start_str
                        .parse()
                        .map_err(|_| LineRangeError(format!("invalid start line: {start_str}")))?,
                )
            };
            let end = if end_str.is_empty() {
                None
            } else {
                Some(
                    end_str
                        .parse()
                        .map_err(|_| LineRangeError(format!("invalid end line: {end_str}")))?,
                )
            };

            if start.is_none() && end.is_none() {
                return Err(LineRangeError(
                    "line range must provide a start or end line".into(),
                ));
            }

            Ok(Self { start, end })
        } else {
            let start = s
                .parse()
                .map_err(|_| LineRangeError(format!("invalid line range: {s}")))?;
            Ok(Self {
                start: Some(start),
                end: None,
            })
        }
    }
}

#[derive(Debug, Clone, Parser)]
pub struct ReadArgs {
    /// File path to read.
    pub path: String,

    /// Consistency boundary for this retrieval.
    #[command(flatten)]
    pub index_consistency: RetrievalConsistencyArgs,

    /// Line range as START:END.
    #[arg(short, long, value_name = "START:END")]
    pub lines: Option<LineRange>,

    /// Read the range for the named symbol.
    #[arg(long, conflicts_with_all = ["lines", "heading", "cursor"])]
    pub symbol: Option<String>,

    /// Read the section for an exact Markdown heading title or outline signature.
    #[arg(long, conflicts_with_all = ["lines", "symbol", "cursor"])]
    pub heading: Option<String>,

    /// One-based occurrence of a duplicate Markdown heading.
    #[arg(
        long,
        requires = "heading",
        value_parser = parse_positive_usize
    )]
    pub heading_occurrence: Option<usize>,

    /// Continue a truncated read.
    #[arg(
        long,
        conflicts_with_all = ["lines", "symbol", "heading", "heading_occurrence"]
    )]
    pub cursor: Option<String>,

    /// Maximum tokens to return.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_tokens: Option<usize>,

    /// Maximum tokens in the final serialized JSON service response.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_response_tokens: Option<usize>,

    /// Expected content hash; returns not_modified when current.
    #[arg(long)]
    pub expected_hash: Option<String>,

    /// Compatibility field; one-shot CLI parsing never enables process-local delta state.
    #[arg(skip)]
    #[doc(hidden)]
    pub delta: bool,
}

impl From<ReadArgs> for ReadRequest {
    fn from(args: ReadArgs) -> Self {
        let (start_line, end_line) = match args.lines {
            Some(range) => (range.start, range.end),
            None => (None, None),
        };

        Self {
            path: args.path,
            start_line,
            end_line,
            symbol: args.symbol,
            heading: args.heading,
            heading_occurrence: args.heading_occurrence,
            continuation_cursor: args.cursor,
            max_tokens: args.max_tokens,
            expected_hash: args.expected_hash,
            // A one-shot CLI process cannot retain the base for a follow-up.
            delta: false,
            receipt_id: None,
        }
    }
}

#[derive(Debug, Clone, Parser)]
pub struct HistoryArgs {
    #[command(subcommand)]
    pub operation: HistoryCommand,

    /// Maximum commits returned by symbol-log.
    #[arg(long, global = true, value_parser = parse_positive_usize)]
    pub max_results: Option<usize>,

    /// Maximum source or diff tokens to return.
    #[arg(long, global = true, value_parser = parse_positive_usize)]
    pub max_tokens: Option<usize>,

    /// Maximum tokens in the final serialized JSON service response.
    #[arg(long, global = true, value_parser = parse_positive_usize)]
    pub max_response_tokens: Option<usize>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum HistoryCommand {
    /// Read one parsed symbol from a Git revision.
    ReadSymbol {
        /// Repository-relative source file path.
        path: String,
        /// Exact parsed symbol name.
        symbol: String,
        /// Immutable Git revision.
        revision: String,
    },
    /// Diff one parsed symbol between two Git revisions.
    DiffSymbol {
        /// Repository-relative source file path.
        path: String,
        /// Exact parsed symbol name.
        symbol: String,
        /// Base Git revision.
        base_revision: String,
        /// Head Git revision.
        head_revision: String,
    },
    /// List recent commits that touched a symbol's tracked lines.
    SymbolLog {
        /// Repository-relative source file path.
        path: String,
        /// Exact parsed symbol name.
        symbol: String,
        /// Revision from which history starts.
        #[arg(long)]
        revision: Option<String>,
    },
}

impl From<HistoryArgs> for HistoryRequest {
    fn from(args: HistoryArgs) -> Self {
        let operation = match args.operation {
            HistoryCommand::ReadSymbol {
                path,
                symbol,
                revision,
            } => HistoryOperation::ReadSymbol {
                path,
                symbol,
                revision,
            },
            HistoryCommand::DiffSymbol {
                path,
                symbol,
                base_revision,
                head_revision,
            } => HistoryOperation::DiffSymbol {
                path,
                symbol,
                base_revision,
                head_revision,
            },
            HistoryCommand::SymbolLog {
                path,
                symbol,
                revision,
            } => HistoryOperation::SymbolLog {
                path,
                symbol,
                revision,
            },
        };
        Self {
            operation,
            max_results: args.max_results,
            max_tokens: args.max_tokens,
        }
    }
}

#[derive(Debug, Clone, Parser)]
pub struct JsonArgs {
    #[command(subcommand)]
    pub operation: JsonCommand,

    /// Maximum tokens across selected/projected JSON.
    #[arg(long, global = true, value_parser = parse_positive_usize)]
    pub max_tokens: Option<usize>,

    /// Maximum tokens in the final serialized JSON service response.
    #[arg(long, global = true, value_parser = parse_positive_usize)]
    pub max_response_tokens: Option<usize>,

    /// Maximum structural items returned.
    #[arg(long, global = true, value_parser = parse_positive_usize)]
    pub max_items: Option<usize>,

    /// Array elements sampled by collapsed projections.
    #[arg(long, global = true)]
    pub array_sample_size: Option<usize>,

    /// Continue an incomplete keys projection.
    #[arg(long, global = true)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum JsonProjectionArg {
    Value,
    Collapsed,
    Keys,
    Schema,
}

impl From<JsonProjectionArg> for JsonProjection {
    fn from(value: JsonProjectionArg) -> Self {
        match value {
            JsonProjectionArg::Value => Self::Value,
            JsonProjectionArg::Collapsed => Self::Collapsed,
            JsonProjectionArg::Keys => Self::Keys,
            JsonProjectionArg::Schema => Self::Schema,
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum JsonCommand {
    /// Select and project one JSON value.
    Query {
        /// Repository-relative JSON file path.
        path: String,
        /// RFC 6901 JSON Pointer.
        #[arg(long, conflicts_with = "jmespath")]
        pointer: Option<String>,
        /// Standard JMESPath expression.
        #[arg(long, conflicts_with = "pointer")]
        jmespath: Option<String>,
        /// Structural result projection.
        #[arg(long, value_enum, default_value_t = JsonProjectionArg::Value)]
        projection: JsonProjectionArg,
    },
    /// Summarize numeric leaves below one JSON selection.
    NumericSummary {
        /// Repository-relative JSON file path.
        path: String,
        /// RFC 6901 JSON Pointer.
        #[arg(long, conflicts_with = "jmespath")]
        pointer: Option<String>,
        /// Standard JMESPath expression.
        #[arg(long, conflicts_with = "pointer")]
        jmespath: Option<String>,
    },
    /// Compare selected fields between two JSON files.
    #[command(group(
        clap::ArgGroup::new("selectors")
            .required(true)
            .multiple(true)
            .args(["pointer", "jmespath"])
    ))]
    DiffFields {
        /// Base JSON file path.
        base_path: String,
        /// Head JSON file path.
        head_path: String,
        /// RFC 6901 JSON Pointer (repeatable).
        #[arg(long)]
        pointer: Vec<String>,
        /// Standard JMESPath expression (repeatable).
        #[arg(long)]
        jmespath: Vec<String>,
        /// Structural projection for selected values.
        #[arg(long, value_enum, default_value_t = JsonProjectionArg::Value)]
        projection: JsonProjectionArg,
    },
}

fn json_selector(pointer: Option<String>, jmespath: Option<String>) -> Option<JsonSelector> {
    pointer
        .map(|pointer| JsonSelector::Pointer { pointer })
        .or_else(|| jmespath.map(|expression| JsonSelector::Jmespath { expression }))
}

impl From<JsonArgs> for JsonRequest {
    fn from(args: JsonArgs) -> Self {
        let operation = match args.operation {
            JsonCommand::Query {
                path,
                pointer,
                jmespath,
                projection,
            } => JsonOperation::Query {
                path,
                selector: json_selector(pointer, jmespath),
                projection: projection.into(),
            },
            JsonCommand::NumericSummary {
                path,
                pointer,
                jmespath,
            } => JsonOperation::NumericSummary {
                path,
                selector: json_selector(pointer, jmespath),
            },
            JsonCommand::DiffFields {
                base_path,
                head_path,
                pointer,
                jmespath,
                projection,
            } => JsonOperation::DiffFields {
                base_path,
                head_path,
                selectors: pointer
                    .into_iter()
                    .map(|pointer| JsonSelector::Pointer { pointer })
                    .chain(
                        jmespath
                            .into_iter()
                            .map(|expression| JsonSelector::Jmespath { expression }),
                    )
                    .collect(),
                projection: projection.into(),
            },
        };
        Self {
            operation,
            max_tokens: args.max_tokens,
            max_items: args.max_items,
            array_sample_size: args.array_sample_size,
            cursor: args.cursor,
        }
    }
}

#[derive(Debug, Clone, Parser)]
pub struct ContextArgs {
    /// Task description.
    #[arg(short, long)]
    pub task: String,

    /// Consistency boundary for this retrieval.
    #[command(flatten)]
    pub index_consistency: RetrievalConsistencyArgs,

    /// Evidence workflow; auto selects only on high-confidence task language.
    #[arg(long, value_enum, default_value = "auto")]
    pub workflow: ContextWorkflowArg,

    /// Caller-observed compiler, test, runtime, or log excerpt (repeatable).
    #[arg(long = "failure-trace")]
    pub failure_traces: Vec<String>,

    /// Caller-observed exact or qualified identifier (repeatable).
    #[arg(long = "evidence-symbol")]
    pub evidence_symbols: Vec<String>,

    /// Caller-observed repository-relative path (repeatable).
    #[arg(long = "evidence-path")]
    pub evidence_paths: Vec<String>,

    /// Caller-observed test name, command, or behavioral check (repeatable).
    #[arg(long = "test-intent")]
    pub test_intents: Vec<String>,

    /// Maximum source tokens across returned fragments.
    #[arg(
        short,
        long,
        value_parser = parse_positive_usize,
        default_value_t = DEFAULT_CONTEXT_TOKENS
    )]
    pub budget: usize,

    /// Maximum tokens in the final serialized JSON service response.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_response_tokens: Option<usize>,

    /// Include only paths matching these patterns (repeatable).
    #[arg(long = "include")]
    pub include_paths: Vec<String>,

    /// Require evidence matching each path pattern (repeatable).
    #[arg(long = "must-include")]
    pub must_include_paths: Vec<String>,

    /// Require evidence for each exact symbol (repeatable).
    #[arg(long = "must-include-symbol")]
    pub must_include_symbols: Vec<String>,

    /// Maximum number of returned fragments (default: 8).
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_fragments: Option<usize>,

    /// Preview ranked candidates without returning source fragments.
    #[arg(long)]
    pub plan_only: bool,

    /// Focus on these paths (repeatable).
    #[arg(long = "focus")]
    pub focus_paths: Vec<String>,

    /// Restrict returned fragments to focus paths.
    #[arg(long)]
    pub strict_focus_paths: bool,

    /// Minimum fragments to return for each focus path.
    #[arg(long, value_parser = parse_positive_usize)]
    pub minimum_fragments_per_focus_path: Option<usize>,

    /// Focus on these symbols (repeatable).
    #[arg(long = "focus-symbol")]
    pub focus_symbols: Vec<String>,

    /// Exclude these paths (repeatable).
    #[arg(long = "exclude")]
    pub exclude_paths: Vec<String>,

    /// Content hashes the caller already holds (repeatable).
    #[arg(long = "known-hash")]
    pub known_hashes: Vec<String>,

    /// Prior repository generation for delta context.
    #[arg(long = "prior-generation")]
    pub prior_repository_generation: Option<u64>,

    /// Base revision or immutable range (e.g. "origin/main" or "BASE..HEAD").
    #[arg(long = "base-revision")]
    pub base_revision: Option<String>,

    /// Changed paths for diff-scoped context (repeatable).
    #[arg(long = "changed-path")]
    pub changed_paths: Vec<String>,

    /// Restrict returned fragments to resolved changed paths.
    #[arg(long)]
    pub strict_changed_paths: bool,

    /// Include full omission facet diagnostics.
    #[arg(long)]
    pub verbose_diagnostics: bool,

    /// Attach compact provenance for a host-triggered executor handoff.
    #[arg(long)]
    pub handoff: bool,

    /// Override the compact handoff task summary.
    #[arg(long, value_name = "TEXT", requires = "handoff")]
    pub handoff_summary: Option<String>,
}

impl ContextArgs {
    fn handoff_request(&self) -> Option<HandoffManifestRequest> {
        self.handoff.then(|| HandoffManifestRequest {
            summary: self.handoff_summary.clone(),
            ..HandoffManifestRequest::default()
        })
    }

    fn workflow_evidence(&self) -> WorkflowEvidence {
        WorkflowEvidence::new()
            .with_failure_traces(self.failure_traces.clone())
            .with_symbols(self.evidence_symbols.clone())
            .with_paths(self.evidence_paths.clone())
            .with_test_intents(self.test_intents.clone())
    }
}

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum ContextWorkflowArg {
    #[default]
    Auto,
    Implementation,
    Contribution,
    Review,
    Investigation,
}

impl From<ContextWorkflowArg> for crate::model::ContextWorkflow {
    fn from(value: ContextWorkflowArg) -> Self {
        match value {
            ContextWorkflowArg::Auto => Self::Auto,
            ContextWorkflowArg::Implementation => Self::Implementation,
            ContextWorkflowArg::Contribution => Self::Contribution,
            ContextWorkflowArg::Review => Self::Review,
            ContextWorkflowArg::Investigation => Self::Investigation,
        }
    }
}

impl From<ContextArgs> for ContextRequest {
    fn from(args: ContextArgs) -> Self {
        Self {
            task: args.task,
            token_budget: args.budget,
            include_paths: args.include_paths,
            must_include_paths: args.must_include_paths,
            must_include_symbols: args.must_include_symbols,
            max_fragments: args.max_fragments,
            plan_only: args.plan_only,
            focus_paths: args.focus_paths,
            strict_focus_paths: args.strict_focus_paths,
            minimum_fragments_per_focus_path: args.minimum_fragments_per_focus_path,
            focus_symbols: args.focus_symbols,
            exclude_paths: args.exclude_paths,
            known_hashes: args.known_hashes,
            receipt_id: None,
            prior_repository_generation: args.prior_repository_generation,
            base_revision: args.base_revision,
            changed_paths: args.changed_paths,
            strict_changed_paths: args.strict_changed_paths,
            verbose_diagnostics: args.verbose_diagnostics,
        }
    }
}
