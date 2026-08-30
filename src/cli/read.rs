use super::*;

/// Verification policy for reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum ReadPolicyArg {
    /// Stop after the requested page; no full-file hash or index staleness.
    #[default]
    Bounded,
    /// Hash the complete live file and report index verification metadata.
    Full,
}

impl From<ReadPolicyArg> for crate::model::ReadPolicy {
    fn from(value: ReadPolicyArg) -> Self {
        match value {
            ReadPolicyArg::Bounded => Self::Bounded,
            ReadPolicyArg::Full => Self::Full,
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

    /// Read an exact Markdown or LaTeX section title or outline signature.
    #[arg(long, conflicts_with_all = ["lines", "symbol", "cursor"])]
    pub heading: Option<String>,

    /// One-based occurrence of a duplicate document heading.
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

    /// Record this target and prefer a cheaper follow-up. Without `expected_hash`,
    /// select the latest compatible base for this exact target.
    #[arg(long)]
    pub delta: bool,

    /// I/O and verification policy; `full` is required for `--delta`.
    #[arg(long, value_enum, default_value_t = ReadPolicyArg::Bounded)]
    pub policy: ReadPolicyArg,
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
            delta: args.delta,
            receipt_id: None,
            policy: args.policy.into(),
        }
    }
}
