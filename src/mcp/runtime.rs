use super::*;

pub(in crate::mcp) struct PreparedRetrievalCall {
    pub(in crate::mcp) services: Arc<Services>,
    pub(in crate::mcp) limits: McpLimitPolicy,
    pub(in crate::mcp) cancellation: CancellationToken,
    pub(in crate::mcp) deadline: tokio::time::Instant,
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

    pub(in crate::mcp) fn services(
        &self,
        state: &McpServiceState,
    ) -> std::result::Result<Arc<Services>, CallToolResult> {
        match state {
            McpServiceState::Ready { services, .. } => Ok(Arc::clone(services)),
            McpServiceState::Starting(_) => Err(self.retryable_result(RetryableToolResponse::new(
                "index_starting",
                "repository index is starting; retry the same call shortly",
                500,
            ))),
            McpServiceState::Failed { failure, .. } => {
                Err(tool_unavailable(failure.reason, failure.message))
            }
        }
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
        let deadline = tokio::time::Instant::now() + INITIAL_INDEX_WAIT;
        let state = self.services.get();
        validate(state.limits()).map_err(into_mcp_error)?;
        let state = self
            .services
            .wait_for_services(state, cancellation.clone(), deadline)
            .await
            .map_err(into_mcp_error)?;
        let limits = state.limits();
        validate(limits).map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(RetrievalPreparation::Unavailable(result)),
        };
        Ok(RetrievalPreparation::Ready(PreparedRetrievalCall {
            services,
            limits,
            cancellation,
            deadline,
        }))
    }

    pub(in crate::mcp) async fn run_prepared<T, F, Fut>(
        &self,
        tool: &'static str,
        prepared: PreparedRetrievalCall,
        expected_repository_id: Option<String>,
        mut operation: F,
    ) -> Result<CallToolResult, ErrorData>
    where
        T: Serialize,
        F: FnMut(Arc<Services>, CancellationToken, tokio::time::Instant) -> Fut,
        Fut: Future<Output = crate::Result<T>>,
    {
        let PreparedRetrievalCall {
            services,
            cancellation,
            deadline,
            ..
        } = prepared;
        let mcp_services = self.services.clone();
        self.run_admitted(
            services,
            expected_repository_id,
            move |services| async move {
                retry_after_initial_index(
                    tool,
                    &mcp_services,
                    &services,
                    cancellation.clone(),
                    deadline,
                    || operation(Arc::clone(&services), cancellation.clone(), deadline),
                )
                .await
            },
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
                        "index_building",
                        "repository index is being built; retry the same call shortly",
                        500,
                    )
                    .with_index_progress(index_progress),
                ))
            }
            Err(error)
                if matches!(
                    error.reconciliation_cause(),
                    crate::Error::RetryableConflict(_)
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
            )),
            Err(error) => Err(into_mcp_error(error)),
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
        services
            .validate_repository_id(expected_repository_id.as_deref())
            .map_err(into_mcp_error)?;
        let _admission = match self.request_admission.try_admit() {
            Ok(permit) => permit,
            Err(error) => return self.service_result::<T>(Err(error)),
        };
        let progress_services = Arc::clone(&services);
        let result = operation(services).await;
        let index_progress = result
            .as_ref()
            .err()
            .is_some_and(|error| {
                matches!(error.reconciliation_cause(), crate::Error::IndexNotReady)
            })
            .then(|| progress_services.index_progress_for_retry());
        self.service_result_with_progress(result, index_progress)
    }
}

pub(in crate::mcp) async fn retry_after_initial_index<T, F, Fut>(
    tool: &'static str,
    mcp_services: &McpServices,
    services: &Services,
    cancellation: CancellationToken,
    deadline: tokio::time::Instant,
    operation: F,
) -> crate::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = crate::Result<T>>,
{
    retry_after_initial_index_with_policy(
        tool,
        mcp_services,
        cancellation,
        deadline.saturating_duration_since(tokio::time::Instant::now()),
        |wait_cancellation| services.wait_for_initial_index_cancellable(wait_cancellation),
        operation,
    )
    .await
}

pub(in crate::mcp) async fn retry_after_initial_index_with_policy<T, F, Fut, W, WaitFut>(
    tool: &'static str,
    mcp_services: &McpServices,
    cancellation: CancellationToken,
    wait: Duration,
    wait_until_ready: W,
    mut operation: F,
) -> crate::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = crate::Result<T>>,
    W: FnOnce(CancellationToken) -> WaitFut,
    WaitFut: Future<Output = crate::Result<()>>,
{
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + wait;
    let result = operation().await;
    if !matches!(result, Err(crate::Error::IndexNotReady)) {
        return result;
    }

    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        tracing::debug!(
            tool,
            waited_ms = started.elapsed().as_millis(),
            ready = false,
            "MCP retrieval waited for the first index generation"
        );
        return result;
    }

    let wait_cancellation = cancellation.child_token();
    let readiness = wait_until_ready(wait_cancellation.clone());
    tokio::pin!(readiness);
    loop {
        let state_changed = mcp_services.state_changed.notified();
        tokio::pin!(state_changed);
        state_changed.as_mut().enable();
        if matches!(mcp_services.get(), McpServiceState::Failed { .. }) {
            wait_cancellation.cancel();
            return Err(crate::Error::McpRuntimeStopped);
        }
        tokio::select! {
            ready = &mut readiness => {
                if matches!(mcp_services.get(), McpServiceState::Failed { .. }) {
                    wait_cancellation.cancel();
                    return Err(crate::Error::McpRuntimeStopped);
                }
                ready?;
                if matches!(mcp_services.get(), McpServiceState::Failed { .. }) {
                    wait_cancellation.cancel();
                    return Err(crate::Error::McpRuntimeStopped);
                }
                let result = operation().await;
                if matches!(mcp_services.get(), McpServiceState::Failed { .. }) {
                    wait_cancellation.cancel();
                    return Err(crate::Error::McpRuntimeStopped);
                }
                tracing::debug!(
                    tool,
                    waited_ms = started.elapsed().as_millis(),
                    ready = !matches!(result, Err(crate::Error::IndexNotReady)),
                    "MCP retrieval waited for the first index generation"
                );
                return result;
            }
            _ = cancellation.cancelled() => {
                wait_cancellation.cancel();
                return Err(crate::Error::Cancelled);
            }
            _ = tokio::time::sleep_until(deadline) => {
                wait_cancellation.cancel();
                tracing::debug!(
                    tool,
                    waited_ms = started.elapsed().as_millis(),
                    ready = false,
                    "MCP retrieval waited for the first index generation"
                );
                return result;
            }
            _ = &mut state_changed => {}
        }
        if matches!(mcp_services.get(), McpServiceState::Failed { .. }) {
            wait_cancellation.cancel();
            return Err(crate::Error::McpRuntimeStopped);
        }
    }
}
