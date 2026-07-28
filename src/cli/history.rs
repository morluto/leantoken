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
