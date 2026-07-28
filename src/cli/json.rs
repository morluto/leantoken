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
