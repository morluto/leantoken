use super::*;

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
