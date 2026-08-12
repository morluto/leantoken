use super::*;

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
