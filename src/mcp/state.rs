use super::*;

#[derive(Debug, Clone, Copy)]
pub(in crate::mcp) struct McpLimitPolicy {
    pub(in crate::mcp) max_results: usize,
    pub(in crate::mcp) max_output_tokens: usize,
    pub(in crate::mcp) max_response_tokens: usize,
    pub(in crate::mcp) max_context_lines: usize,
    pub(in crate::mcp) default_context_tokens: usize,
}

impl McpLimitPolicy {
    pub(in crate::mcp) fn from_config(config: &Config) -> crate::Result<Self> {
        config.validate()?;
        Ok(Self {
            max_results: config.max_results,
            max_output_tokens: config.max_output_tokens,
            max_response_tokens: MAX_OUTPUT_TOKENS,
            max_context_lines: MAX_CONTEXT_LINES,
            default_context_tokens: config.default_context_tokens,
        })
    }
}
