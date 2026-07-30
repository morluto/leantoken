use super::*;

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
            query_receipt: None,
            cursor: args.cursor,
        }
    }
}
