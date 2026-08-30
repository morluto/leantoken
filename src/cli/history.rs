use super::*;

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
    /// Diff an ordered symbol set across two revisions.
    DiffSymbols {
        /// JSON array of targets; each object has path, symbol, and optional head_path and head_symbol.
        targets: String,
        /// Base Git revision.
        base_revision: String,
        /// Head Git revision.
        head_revision: String,
        /// Opaque cursor from a preceding page.
        #[arg(long)]
        cursor: Option<String>,
    },
}

impl HistoryArgs {
    pub fn into_single(self) -> HistoryRequest {
        let operation = match self.operation {
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
            HistoryCommand::DiffSymbols { .. } => {
                unreachable!("DiffSymbols is handled separately")
            }
        };
        HistoryRequest {
            operation,
            max_results: self.max_results,
            max_tokens: self.max_tokens,
        }
    }

    pub fn into_diff_symbols(self) -> crate::model::DiffSymbolsRequest {
        match self.operation {
            HistoryCommand::DiffSymbols {
                targets,
                base_revision,
                head_revision,
                cursor,
            } => {
                let parsed: Vec<crate::model::DiffSymbolsTarget> =
                    serde_json::from_str(&targets)
                        .expect("targets must be a JSON array of {path, symbol, head_path?, head_symbol?}");
                crate::model::DiffSymbolsRequest {
                    targets: parsed,
                    base_revision,
                    head_revision,
                    max_results: self.max_results,
                    max_tokens: self.max_tokens,
                    cursor,
                }
            }
            _ => unreachable!("into_diff_symbols called on non-DiffSymbols"),
        }
    }
}
