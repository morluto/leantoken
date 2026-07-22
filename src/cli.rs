use std::{
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    str::FromStr,
};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::Config;
use crate::Result;
use crate::cache::CachePruneRequest;
use crate::mcp::McpResultMode;
use crate::model::{
    ContextRequest, FileOperation, FilesRequest, OutlineRequest, ReadRequest, SearchMode,
    SearchRequest,
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

/// LeanToken CLI and MCP server entry point.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "leantoken",
    version,
    about = "Token-budgeted repository context"
)]
pub struct Cli {
    /// Repository root path.
    #[arg(long, value_name = "PATH", global = true, default_value = ".")]
    pub root: PathBuf,

    /// Allow indexing a filesystem root, home directory, or parent of home.
    #[arg(long, global = true)]
    pub allow_broad_root: bool,

    /// Include known generated and package-cache directories.
    #[arg(long, global = true)]
    pub include_generated: bool,

    /// Maximum filesystem entries yielded by repository discovery.
    #[arg(long, value_name = "COUNT", global = true)]
    pub max_walk_entries: Option<NonZeroU64>,

    /// Maximum files admitted to the repository index.
    #[arg(long, value_name = "COUNT", global = true)]
    pub max_files: Option<NonZeroU64>,

    /// Maximum aggregate bytes admitted to the repository index.
    #[arg(long, value_name = "BYTES", global = true)]
    pub max_total_source_bytes: Option<NonZeroU64>,

    /// Maximum repository-relative traversal depth.
    #[arg(long, value_name = "DEPTH", global = true)]
    pub max_depth: Option<NonZeroUsize>,

    /// Maximum bytes admitted from one file.
    #[arg(long, value_name = "BYTES", global = true)]
    pub max_file_bytes: Option<NonZeroU64>,

    /// Maximum files scheduled in one preparation batch.
    #[arg(long, value_name = "COUNT", global = true)]
    pub max_prepare_batch_files: Option<NonZeroUsize>,

    /// Maximum source bytes scheduled in one preparation batch.
    #[arg(long, value_name = "BYTES", global = true)]
    pub max_prepare_batch_bytes: Option<NonZeroU64>,

    /// SQLite database path.
    #[arg(long, value_name = "PATH", global = true)]
    pub database: Option<PathBuf>,

    /// Emit JSON output where applicable.
    #[arg(long, global = true)]
    pub json: bool,

    /// Tokenizer used for source and protocol token accounting.
    #[arg(long, value_enum, value_name = "ENCODING", default_value_t = Tokenizer::default(), global = true)]
    pub tokenizer: Tokenizer,

    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
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
        config.include_generated = self.include_generated;
        config.tokenizer = self.tokenizer;
        config.discovery_limits().validate()?;
        Ok(config)
    }

    /// Convert the parsed CLI into an application request.
    pub fn app_request(self) -> AppRequest {
        match self.command {
            Commands::Index { rebuild } => AppRequest::Index { rebuild },
            Commands::Status => AppRequest::Status,
            Commands::Files(args) => AppRequest::Files(args.into()),
            Commands::Search(args) => AppRequest::Search(args.into()),
            Commands::Outline(args) => AppRequest::Outline(args.into()),
            Commands::Read(args) => AppRequest::Read(args.into()),
            Commands::Context(args) => AppRequest::Context(args.into()),
            Commands::Doctor => AppRequest::Doctor,
            Commands::Mcp(args) => AppRequest::Mcp {
                result_mode: args.result_mode,
            },
            Commands::Setup(args) => AppRequest::Setup(args.into()),
            Commands::Remove(args) => AppRequest::Remove(args.into()),
            Commands::Cache(args) => match args.command {
                CacheCommand::List => AppRequest::CacheList,
                CacheCommand::Prune(args) => AppRequest::CachePrune(args.into()),
            },
            Commands::Update(args) | Commands::Upgrade(args) => AppRequest::Upgrade {
                check: args.check,
                yes: args.yes,
            },
        }
    }
}

/// Parsed application request produced by the CLI.
#[derive(Debug, Clone)]
pub enum AppRequest {
    Index { rebuild: bool },
    Status,
    Files(FilesRequest),
    Search(SearchRequest),
    Outline(OutlineRequest),
    Read(ReadRequest),
    Context(ContextRequest),
    Doctor,
    Mcp { result_mode: McpResultMode },
    Setup(SetupRequest),
    Remove(SetupRequest),
    CacheList,
    CachePrune(CachePruneRequest),
    Upgrade { check: bool, yes: bool },
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

    /// List, find, or glob repository paths.
    Files(FilesArgs),

    /// Search the repository for terms, symbols, or references.
    Search(SearchArgs),

    /// Show the structural outline of one or more files.
    Outline(OutlineArgs),

    /// Read a bounded source range.
    Read(ReadArgs),

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
    /// Apply without prompting; requires explicit clients, --all, or --refresh.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Show the exact configuration plan without making changes.
    #[arg(long)]
    pub dry_run: bool,
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
    List,
    /// Remove inactive managed caches selected by explicit criteria.
    Prune(CachePruneArgs),
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
    /// Show the exact prune plan without deleting files.
    #[arg(long)]
    pub dry_run: bool,
    /// Apply the prune plan without prompting.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

impl From<CachePruneArgs> for CachePruneRequest {
    fn from(args: CachePruneArgs) -> Self {
        Self {
            older_than_days: args.older_than.map(NonZeroU64::get),
            max_total_bytes: args.max_total_bytes,
            remove_missing_roots: args.remove_missing_roots,
            dry_run: args.dry_run,
            yes: args.yes,
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
            yes: args.yes,
            dry_run: args.dry_run,
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

#[derive(Debug, Clone, Parser)]
pub struct FilesArgs {
    /// Files operation to perform.
    pub operation: FileOperationArg,

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

    /// Lines of context around each match.
    #[arg(long)]
    pub context_lines: Option<usize>,

    /// Perform a case-sensitive search.
    #[arg(long)]
    pub case_sensitive: bool,

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
            cursor: args.cursor,
        }
    }
}

#[derive(Debug, Clone, Parser)]
pub struct OutlineArgs {
    /// Paths to outline.
    pub paths: Vec<String>,

    /// Filter by symbol name.
    #[arg(long)]
    pub symbol_name: Option<String>,

    /// Filter by symbol kind.
    #[arg(long)]
    pub symbol_kind: Option<String>,

    /// Maximum number of symbols.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_results: Option<usize>,

    /// Maximum tokens to return.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_tokens: Option<usize>,
}

impl From<OutlineArgs> for OutlineRequest {
    fn from(args: OutlineArgs) -> Self {
        Self {
            paths: args.paths,
            symbol_name: args.symbol_name,
            symbol_kind: args.symbol_kind,
            max_results: args.max_results,
            max_tokens: args.max_tokens,
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

    /// Line range as START:END.
    #[arg(short, long, value_name = "START:END")]
    pub lines: Option<LineRange>,

    /// Read the range for the named symbol.
    #[arg(long, conflicts_with = "lines")]
    pub symbol: Option<String>,

    /// Maximum tokens to return.
    #[arg(long, value_parser = parse_positive_usize)]
    pub max_tokens: Option<usize>,

    /// Expected content hash; returns not_modified when current.
    #[arg(long)]
    pub expected_hash: Option<String>,
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
            max_tokens: args.max_tokens,
            expected_hash: args.expected_hash,
        }
    }
}

#[derive(Debug, Clone, Parser)]
pub struct ContextArgs {
    /// Task description.
    #[arg(short, long)]
    pub task: String,

    /// Token budget for the response.
    #[arg(short, long, value_parser = parse_positive_usize)]
    pub budget: usize,

    /// Focus on these paths (repeatable).
    #[arg(long = "focus")]
    pub focus_paths: Vec<String>,

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

    /// Base revision for diff-scoped context (e.g. "origin/main").
    #[arg(long = "base-revision")]
    pub base_revision: Option<String>,

    /// Changed paths for diff-scoped context (repeatable).
    #[arg(long = "changed-path")]
    pub changed_paths: Vec<String>,
}

impl From<ContextArgs> for ContextRequest {
    fn from(args: ContextArgs) -> Self {
        Self {
            task: args.task,
            token_budget: args.budget,
            focus_paths: args.focus_paths,
            focus_symbols: args.focus_symbols,
            exclude_paths: args.exclude_paths,
            known_hashes: args.known_hashes,
            prior_repository_generation: args.prior_repository_generation,
            base_revision: args.base_revision,
            changed_paths: args.changed_paths,
        }
    }
}
