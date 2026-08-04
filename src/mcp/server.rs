use super::*;

/// LeanToken MCP server.
#[derive(Clone)]
pub struct LeanTokenMcp {
    pub(in crate::mcp) services: McpServices,
    pub(in crate::mcp) contexts: McpContextRegistry,
    pub(in crate::mcp) result_mode: McpResultMode,
    pub(in crate::mcp) request_admission: RequestAdmission,
    pub(in crate::mcp) request_dispatch: RequestAdmission,
    pub(in crate::mcp) resource_read_admission: RequestAdmission,
}

impl LeanTokenMcp {
    pub fn context_registry(&self) -> McpContextRegistry {
        self.contexts.clone()
    }

    #[must_use]
    pub fn new(services: Arc<Services>) -> Self {
        let services = McpServices::ready(services);
        Self {
            contexts: McpContextRegistry::primary(services.clone()),
            services,
            result_mode: McpResultMode::Structured,
            request_admission: RequestAdmission::new(DEFAULT_ACTIVE_TOOL_CALL_CAPACITY),
            request_dispatch: RequestAdmission::new(DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY),
            resource_read_admission: RequestAdmission::new(default_receipt_resource_read_capacity()),
        }
    }

    /// Construct a protocol-ready server before storage and indexing start.
    #[must_use]
    pub fn pending() -> (Self, McpServices) {
        let services = McpServices::starting(McpLimitPolicy::DEFAULT);
        (
            Self {
                services: services.clone(),
                contexts: McpContextRegistry::primary(services.clone()),
                result_mode: McpResultMode::Structured,
                request_admission: RequestAdmission::new(DEFAULT_ACTIVE_TOOL_CALL_CAPACITY),
                request_dispatch: RequestAdmission::new(DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY),
                resource_read_admission: RequestAdmission::new(
                    default_receipt_resource_read_capacity(),
                ),
            },
            services,
        )
    }

    /// Select the successful-result representation for this server instance.
    #[must_use]
    pub fn with_result_mode(mut self, result_mode: McpResultMode) -> Self {
        self.result_mode = result_mode;
        self
    }
}
