use super::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Retrieval operation used for response-accounting boundaries.
pub enum TokenAccountingOperation {
    Search,
    Outline,
    Read,
    ContextPlan,
    Context,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LanguageCount {
    pub language: String,
    pub files: usize,
}
