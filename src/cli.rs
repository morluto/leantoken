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
    ContextRequest, ContextRequiredEvidence, FileOperation, FilesRequest, HandoffManifestRequest,
    HistoryOperation, HistoryRequest, IndexConsistency, JsonOperation, JsonProjection, JsonRequest,
    JsonSelector, OutlineRequest, ReadRequest, SearchMode, SearchRequest, WorkflowEvidence,
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

fn parse_required_evidence(value: &str) -> std::result::Result<ContextRequiredEvidence, String> {
    serde_json::from_str(value).map_err(|error| format!("invalid required-evidence JSON: {error}"))
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
        id: "index_include",
        long: "--index-include",
        advanced: true,
    },
    ScopedGlobalOption {
        id: "index_exclude",
        long: "--index-exclude",
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

    /// Include only repository-relative paths matched by these patterns.
    #[arg(
        long = "index-include",
        value_name = "PATTERN",
        global = true,
        action = clap::ArgAction::Append,
        help_heading = "Advanced repository options"
    )]
    pub index_include: Vec<String>,

    /// Exclude repository-relative paths matched by these patterns.
    #[arg(
        long = "index-exclude",
        value_name = "PATTERN",
        global = true,
        action = clap::ArgAction::Append,
        help_heading = "Advanced repository options"
    )]
    pub index_exclude: Vec<String>,

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
        let index_scope =
            crate::IndexScope::new(self.index_include.clone(), self.index_exclude.clone())?;
        let mut config = Config::discover_scoped_with_broad_root(
            &self.root,
            self.database.clone(),
            self.allow_broad_root,
            index_scope,
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
            Commands::Savings(args) => {
                args.snapshot
                    .map_or(AppRequest::Savings, |snapshot| AppRequest::SavingsDelta {
                        snapshot,
                    })
            }
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
                let response_profile = args.response_profile.map(Into::into);
                AppRequest::Context {
                    request: args.into(),
                    workflow,
                    workflow_evidence,
                    handoff: handoff.map(Box::new),
                    max_response_tokens,
                    response_profile,
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

/// Parsed application request produced by the CLI.
// Boxing the established public variants would be a source-breaking API change.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum AppRequest {
    Index {
        rebuild: bool,
    },
    Status,
    Savings,
    SavingsDelta {
        snapshot: String,
    },
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
        response_profile: Option<crate::model::ContextResponseProfile>,
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

// Clap owns this public command shape; keep it source-compatible with AppRequest.
#[allow(clippy::large_enum_variant)]
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
    Savings(SavingsArgs),

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

// Command DTOs remain transport-specific, but each command family has a
// distinct physical owner while Cli, Commands, and AppRequest stay here.
include!("cli/integration.rs");
include!("cli/cache.rs");
include!("cli/retrieval.rs");
include!("cli/files.rs");
include!("cli/search.rs");
include!("cli/outline.rs");
include!("cli/read.rs");
include!("cli/history.rs");
include!("cli/json.rs");
include!("cli/context.rs");

#[cfg(test)]
mod tests;
