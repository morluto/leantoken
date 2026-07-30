use super::*;

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
