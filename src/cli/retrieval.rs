use super::*;
use crate::model::SearchMode;

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
            SearchModeArg::Auto => Self::Auto,
            SearchModeArg::Text => Self::Text,
            SearchModeArg::Regex => Self::Regex,
            SearchModeArg::Identifier => Self::Identifier,
            SearchModeArg::Symbol => Self::Symbol,
            SearchModeArg::Reference => Self::Reference,
        }
    }
}
