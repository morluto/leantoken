use super::*;

pub(in crate::mcp) struct PreparedRetrievalCall {
    pub(in crate::mcp) services: Arc<Services>,
    pub(in crate::mcp) limits: McpLimitPolicy,
    pub(in crate::mcp) cancellation: CancellationToken,
}

pub(in crate::mcp) enum RetrievalPreparation {
    Ready(PreparedRetrievalCall),
    Unavailable(CallToolResult),
}

impl LeanTokenMcp {
    pub(in crate::mcp) fn result<T: Serialize>(
        &self,
        value: T,
    ) -> Result<CallToolResult, ErrorData> {
        tool_result(value, self.result_mode)
    }

    fn result_with_limit<T: Serialize>(
        &self,
        value: T,
        max_response_tokens: Option<usize>,
        protocol: Option<&ProtocolVersion>,
    ) -> Result<CallToolResult, ErrorData> {
        tool_result_with_limit(value, self.result_mode, max_response_tokens, protocol)
    }

    pub(in crate::mcp) fn retryable_result(
        &self,
        response: RetryableToolResponse,
    ) -> CallToolResult {
        retryable_tool_result(response, self.result_mode)
    }

    pub(in crate::mcp) async fn prepare_retrieval_call(
        &self,
        cancellation: CancellationToken,
        validate: impl Fn(McpLimitPolicy) -> crate::Result<()>,
    ) -> Result<RetrievalPreparation, ErrorData> {
        if let Err(error) = validate(self.limits) {
            return into_tool_error(error, self.result_mode).map(RetrievalPreparation::Unavailable);
        }
        Ok(RetrievalPreparation::Ready(PreparedRetrievalCall {
            services: Arc::clone(&self.services),
            limits: self.limits,
            cancellation,
        }))
    }

    pub(in crate::mcp) async fn run_prepared<T, F, Fut>(
        &self,
        prepared: PreparedRetrievalCall,
        expected_repository_id: Option<String>,
        max_response_tokens: Option<usize>,
        protocol: Option<ProtocolVersion>,
        mut operation: F,
    ) -> Result<CallToolResult, ErrorData>
    where
        T: Serialize,
        F: FnMut(Arc<Services>, CancellationToken) -> Fut,
        Fut: Future<Output = crate::Result<T>>,
    {
        let PreparedRetrievalCall {
            services,
            cancellation,
            ..
        } = prepared;
        self.run_admitted_with_limit(
            services,
            expected_repository_id,
            max_response_tokens,
            protocol,
            move |services| async move { operation(Arc::clone(&services), cancellation).await },
        )
        .await
    }

    pub(in crate::mcp) fn service_result<T: Serialize>(
        &self,
        result: crate::Result<T>,
    ) -> Result<CallToolResult, ErrorData> {
        self.service_result_with_progress(result, None)
    }

    fn service_result_with_progress<T: Serialize>(
        &self,
        result: crate::Result<T>,
        index_progress: Option<IndexProgressSnapshot>,
    ) -> Result<CallToolResult, ErrorData> {
        match result {
            Ok(value) => self.result(value),
            Err(error) if matches!(error.reconciliation_cause(), crate::Error::IndexNotReady) => {
                Ok(self.retryable_result(
                    RetryableToolResponse::new(
                        "refresh_required",
                        "no repository generation has been published; call refresh",
                        0,
                    )
                    .with_index_progress(index_progress),
                ))
            }
            Err(error)
                if matches!(
                    error.reconciliation_cause(),
                    crate::Error::StaleReconciliation { .. } | crate::Error::RetryableConflict(_)
                ) =>
            {
                Ok(self.retryable_result(RetryableToolResponse::new(
                    "repository_changed",
                    "repository index changed during retrieval; retry the same call",
                    100,
                )))
            }
            Err(error)
                if matches!(
                    error.reconciliation_cause(),
                    crate::Error::RetrievalOverloaded
                ) =>
            {
                Ok(self.retryable_result(RetryableToolResponse::new(
                    "retrieval_capacity_exhausted",
                    "repository tool-call capacity is exhausted; retry shortly",
                    500,
                )))
            }
            Err(error)
                if matches!(
                    error.reconciliation_cause(),
                    crate::Error::RetrievalQueueTimeout
                ) =>
            {
                Ok(self.retryable_result(RetryableToolResponse::new(
                    "retrieval_queue_timeout",
                    "repository retrieval did not obtain execution capacity in time; retry shortly",
                    500,
                )))
            }
            Err(crate::Error::McpRuntimeStopped) => Ok(tool_unavailable(
                "index_runtime_stopped",
                "repository index is unavailable; check server logs and retry",
                self.result_mode,
            )),
            Err(error) => into_tool_error(error, self.result_mode),
        }
    }

    pub(in crate::mcp) async fn run_admitted<T, F, Fut>(
        &self,
        services: Arc<Services>,
        expected_repository_id: Option<String>,
        operation: F,
    ) -> Result<CallToolResult, ErrorData>
    where
        T: Serialize,
        F: FnOnce(Arc<Services>) -> Fut,
        Fut: Future<Output = crate::Result<T>>,
    {
        self.run_admitted_with_limit(services, expected_repository_id, None, None, operation)
            .await
    }

    async fn run_admitted_with_limit<T, F, Fut>(
        &self,
        services: Arc<Services>,
        expected_repository_id: Option<String>,
        max_response_tokens: Option<usize>,
        protocol: Option<ProtocolVersion>,
        operation: F,
    ) -> Result<CallToolResult, ErrorData>
    where
        T: Serialize,
        F: FnOnce(Arc<Services>) -> Fut,
        Fut: Future<Output = crate::Result<T>>,
    {
        if let Err(error) = services.validate_repository_id(expected_repository_id.as_deref()) {
            return into_tool_error(error, self.result_mode);
        }
        let _admission = match self.request_admission.try_admit() {
            Ok(permit) => permit,
            Err(error) => return self.service_result::<T>(Err(error)),
        };
        let result = operation(services).await;
        match result {
            Ok(value) => {
                match self.result_with_limit(value, max_response_tokens, protocol.as_ref()) {
                    Ok(result) => Ok(result),
                    Err(error) => visible_mcp_error(error, self.result_mode),
                }
            }
            // This centralizes retryable-state projection and converts every
            // remaining semantic failure with `into_tool_error`; internal
            // failures still emerge as protocol errors from that conversion.
            Err(error) => self.service_result_with_progress::<T>(Err(error), None),
        }
    }
}
