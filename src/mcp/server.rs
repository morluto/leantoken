use super::*;

/// LeanToken MCP server.
#[derive(Clone)]
pub struct LeanTokenMcp {
    pub(in crate::mcp) services: Arc<Services>,
    pub(in crate::mcp) limits: McpLimitPolicy,
    pub(in crate::mcp) result_mode: McpResultMode,
    pub(in crate::mcp) request_admission: RequestAdmission,
}

impl LeanTokenMcp {
    #[must_use]
    pub fn new(services: Arc<Services>) -> Self {
        let limits = McpLimitPolicy::from_config(services.config())
            .expect("Services always contains a validated configuration");
        Self {
            services,
            limits,
            result_mode: McpResultMode::Structured,
            request_admission: RequestAdmission::new(DEFAULT_ACTIVE_TOOL_CALL_CAPACITY),
        }
    }

    /// Select the successful-result representation for this server instance.
    #[must_use]
    pub fn with_result_mode(mut self, result_mode: McpResultMode) -> Self {
        self.result_mode = result_mode;
        self
    }
}
