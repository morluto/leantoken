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

/// Clap value for the index consistency boundary.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum IndexConsistencyArg {
    /// Query the latest completed index generation without scanning live changes.
    IndexedGeneration,
    /// Reconcile the live working tree before retrieval.
    #[default]
    ReconcileWorkingTree,
}

impl From<IndexConsistencyArg> for IndexConsistency {
    fn from(value: IndexConsistencyArg) -> Self {
        match value {
            IndexConsistencyArg::IndexedGeneration => Self::IndexedGeneration,
            IndexConsistencyArg::ReconcileWorkingTree => Self::ReconcileWorkingTree,
        }
    }
}

/// Consistency options shared by index-backed CLI retrievals.
#[derive(Debug, Clone, Args)]
pub struct RetrievalConsistencyArgs {
    /// Index consistency boundary applied before retrieval.
    #[arg(long, value_enum, default_value_t = IndexConsistencyArg::ReconcileWorkingTree)]
    pub consistency: IndexConsistencyArg,
}

#[derive(Debug, Clone, Args)]
pub struct SavingsArgs {
    /// Opaque snapshot from an earlier report; show only subsequent aggregate activity.
    #[arg(long)]
    pub snapshot: Option<String>,
}
