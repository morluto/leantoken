impl Services {
    /// Select ranked task evidence within an exact source-token budget.
    pub async fn context(&self, request: ContextRequest) -> Result<ContextResponse> {
        self.context_with_options(request, ServiceCallOptions::default())
            .await
    }

    /// Select ranked task evidence under explicit serialized-response controls.
    pub async fn context_with_options(
        &self,
        request: ContextRequest,
        options: ServiceCallOptions,
    ) -> Result<ContextResponse> {
        let accounted = self
            .context_cancellable_with_workflow(
                request,
                ContextWorkflow::Auto,
                options,
                CancellationToken::new(),
            )
            .await?;
        Ok(self.record_context_response(accounted))
    }

    /// Select ranked task evidence using typed caller-observed workflow signals.
    pub async fn context_with_workflow_evidence(
        &self,
        request: ContextRequest,
        workflow_evidence: WorkflowEvidence,
    ) -> Result<ContextResponse> {
        let accounted = self
            .context_cancellable_with_workflow_evidence_and_handoff(
                request,
                ContextWorkflow::Auto,
                None,
                ServiceCallOptions::default(),
                workflow_evidence,
                CancellationToken::new(),
            )
            .await?;
        Ok(self.record_context_response(accounted))
    }

    /// Select context and attach compact provenance for a host-triggered handoff.
    pub async fn context_with_handoff(
        &self,
        request: ContextRequest,
        handoff: HandoffManifestRequest,
    ) -> Result<ContextResponse> {
        let accounted = self
            .context_cancellable_with_workflow_and_handoff(
                request,
                ContextWorkflow::Auto,
                Some(handoff),
                ServiceCallOptions::default(),
                CancellationToken::new(),
            )
            .await?;
        Ok(self.record_context_response(accounted))
    }

    /// Retrieve context after applying the requested index consistency boundary.
    pub async fn context_with_consistency_cancellable(
        &self,
        request: ContextRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        let operation = context_accounting_operation(&request);
        self.observe_service_result(operation, self.validate_context_request(&request, None))?;
        let consistency_result = self
            .apply_consistency(consistency, cancellation.clone())
            .await;
        self.observe_service_result(operation, consistency_result)?;
        let accounted = self
            .context_cancellable_with_workflow(
                request,
                ContextWorkflow::Auto,
                ServiceCallOptions::default(),
                cancellation,
            )
            .await?;
        let mut response = accounted.response;
        set_routing_consistency(&mut response, consistency);
        let finalize_result = self.finalize_response(&mut response);
        self.observe_service_result(operation, finalize_result)?;
        self.record_token_savings(
            accounted.operation,
            accounted.baseline_source_tokens,
            &response.meta,
        );
        Ok(response)
    }

    /// Retrieve context under an explicit or auto-detected workflow.
    pub async fn context_with_workflow_consistency_cancellable(
        &self,
        request: ContextRequest,
        workflow: ContextWorkflow,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        let operation = context_accounting_operation(&request);
        self.observe_service_result(operation, self.validate_context_request(&request, None))?;
        let consistency_result = self
            .apply_consistency(consistency, cancellation.clone())
            .await;
        self.observe_service_result(operation, consistency_result)?;
        let accounted = self
            .context_cancellable_with_workflow(
                request,
                workflow,
                ServiceCallOptions::default(),
                cancellation,
            )
            .await?;
        let mut response = accounted.response;
        set_routing_consistency(&mut response, consistency);
        let finalize_result = self.finalize_response(&mut response);
        self.observe_service_result(operation, finalize_result)?;
        self.record_token_savings(
            accounted.operation,
            accounted.baseline_source_tokens,
            &response.meta,
        );
        Ok(response)
    }

    /// Retrieve context with an opt-in handoff manifest under explicit adapter policy.
    pub async fn context_with_handoff_workflow_consistency_cancellable(
        &self,
        request: ContextRequest,
        handoff: HandoffManifestRequest,
        workflow: ContextWorkflow,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        let operation = context_accounting_operation(&request);
        self.observe_service_result(
            operation,
            self.validate_context_request(&request, Some(&handoff)),
        )?;
        let consistency_result = self
            .apply_consistency(consistency, cancellation.clone())
            .await;
        self.observe_service_result(operation, consistency_result)?;
        let accounted = self
            .context_cancellable_with_workflow_and_handoff(
                request,
                workflow,
                Some(handoff),
                ServiceCallOptions::default(),
                cancellation,
            )
            .await?;
        let mut response = accounted.response;
        set_routing_consistency(&mut response, consistency);
        let finalize_result = self.finalize_response(&mut response);
        self.observe_service_result(operation, finalize_result)?;
        self.record_token_savings(
            accounted.operation,
            accounted.baseline_source_tokens,
            &response.meta,
        );
        Ok(response)
    }

    pub async fn context_cancellable(
        &self,
        request: ContextRequest,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        let accounted = self
            .context_cancellable_with_workflow(
                request,
                ContextWorkflow::Auto,
                ServiceCallOptions::default(),
                cancellation,
            )
            .await?;
        Ok(self.record_context_response(accounted))
    }

    /// Retrieve context under adapter policy and explicit response controls.
    #[allow(clippy::too_many_arguments)]
    pub async fn context_with_options_workflow_consistency_cancellable(
        &self,
        request: ContextRequest,
        handoff: Option<HandoffManifestRequest>,
        workflow: ContextWorkflow,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        self.context_with_workflow_evidence_options_consistency_cancellable(
            request,
            handoff,
            workflow,
            WorkflowEvidence::default(),
            consistency,
            options,
            cancellation,
        )
        .await
    }

    /// Retrieve context with typed caller-observed workflow evidence.
    #[allow(clippy::too_many_arguments)]
    pub async fn context_with_workflow_evidence_options_consistency_cancellable(
        &self,
        request: ContextRequest,
        handoff: Option<HandoffManifestRequest>,
        workflow: ContextWorkflow,
        workflow_evidence: WorkflowEvidence,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        let operation = context_accounting_operation(&request);
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.observe_service_result(
            operation,
            self.validate_context_request(&request, handoff.as_ref()),
        )?;
        self.observe_service_result(
            operation,
            self.validate_workflow_evidence(&workflow_evidence),
        )?;
        let consistency_result = self
            .apply_consistency(consistency, cancellation.clone())
            .await;
        self.observe_service_result(operation, consistency_result)?;
        let accounted = self
            .context_cancellable_with_workflow_evidence_and_handoff(
                request,
                workflow,
                handoff,
                options,
                workflow_evidence,
                cancellation,
            )
            .await?;
        let mut response = accounted.response;
        set_routing_consistency(&mut response, consistency);
        let finalize_result = self.finalize_response(&mut response);
        self.observe_service_result(operation, finalize_result)?;
        if let Some(max_response_tokens) = options.max_response_tokens()
            && response.meta.total_response_tokens > max_response_tokens
        {
            return self.observe_service_result(
                operation,
                Err(Error::RequestLimitExceeded {
                    field: "max_response_tokens",
                    requested: response.meta.total_response_tokens,
                    limit: max_response_tokens,
                }),
            );
        }
        self.record_token_savings(
            accounted.operation,
            accounted.baseline_source_tokens,
            &response.meta,
        );
        Ok(response)
    }

    async fn context_cancellable_with_workflow(
        &self,
        request: ContextRequest,
        workflow: ContextWorkflow,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<AccountedContextResponse> {
        self.context_cancellable_with_workflow_and_handoff(
            request,
            workflow,
            None,
            options,
            cancellation,
        )
        .await
    }

    async fn context_cancellable_with_workflow_and_handoff(
        &self,
        request: ContextRequest,
        workflow: ContextWorkflow,
        handoff: Option<HandoffManifestRequest>,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<AccountedContextResponse> {
        self.context_cancellable_with_workflow_evidence_and_handoff(
            request,
            workflow,
            handoff,
            options,
            WorkflowEvidence::default(),
            cancellation,
        )
        .await
    }

    async fn context_cancellable_with_workflow_evidence_and_handoff(
        &self,
        request: ContextRequest,
        workflow: ContextWorkflow,
        handoff: Option<HandoffManifestRequest>,
        options: ServiceCallOptions,
        workflow_evidence: WorkflowEvidence,
        cancellation: CancellationToken,
    ) -> Result<AccountedContextResponse> {
        let operation = if request.plan_only {
            TokenAccountingOperation::ContextPlan
        } else {
            TokenAccountingOperation::Context
        };
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                let (evaluation, baseline_source_tokens) = this.context_sync(
                    request,
                    workflow,
                    handoff,
                    options,
                    workflow_evidence,
                    cancellation,
                    CandidateDiagnostics::Omit,
                    ContextSignals::PRODUCTION,
                )?;
                Ok(AccountedContextResponse {
                    response: evaluation.response,
                    baseline_source_tokens,
                    operation,
                })
            })
            .await;
        self.observe_service_result(operation, result)
    }

    fn record_context_response(&self, accounted: AccountedContextResponse) -> ContextResponse {
        self.record_token_savings(
            accounted.operation,
            accounted.baseline_source_tokens,
            &accounted.response.meta,
        );
        accounted.response
    }

    /// Retrieve context and expose pre-selection candidate paths for evaluation.
    ///
    /// Production adapters should use [`Self::context`]. This method exists for
    /// frozen retrieval benchmarks and does not alter the MCP response schema.
    pub async fn context_evaluation(&self, request: ContextRequest) -> Result<ContextEvaluation> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |cancellation| {
                this.context_sync(
                    request,
                    ContextWorkflow::Implementation,
                    None,
                    ServiceCallOptions::default(),
                    WorkflowEvidence::default(),
                    cancellation,
                    CandidateDiagnostics::Collect,
                    ContextSignals::PRODUCTION,
                )
                .map(|(evaluation, _)| evaluation)
            })
            .await
    }

    /// Evaluate typed workflow evidence while exposing pre-selection candidates.
    pub async fn context_evaluation_with_workflow_evidence(
        &self,
        request: ContextRequest,
        workflow_evidence: WorkflowEvidence,
    ) -> Result<ContextEvaluation> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |cancellation| {
                this.context_sync(
                    request,
                    ContextWorkflow::Implementation,
                    None,
                    ServiceCallOptions::default(),
                    workflow_evidence,
                    cancellation,
                    CandidateDiagnostics::Collect,
                    ContextSignals::PRODUCTION,
                )
                .map(|(evaluation, _)| evaluation)
            })
            .await
    }

    /// Retrieve context under one evaluation-only dependency or caller policy.
    ///
    /// This API is not exposed through CLI or MCP adapters. It exists so frozen
    /// ablations can compare additive signals without approximating selection.
    pub async fn context_signal_evaluation(
        &self,
        request: ContextRequest,
        policy: ContextSignalPolicy,
    ) -> Result<ContextEvaluation> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |cancellation| {
                this.context_sync(
                    request,
                    ContextWorkflow::Implementation,
                    None,
                    ServiceCallOptions::default(),
                    WorkflowEvidence::default(),
                    cancellation,
                    CandidateDiagnostics::Collect,
                    ContextSignals::evaluation(policy),
                )
                .map(|(evaluation, _)| evaluation)
            })
            .await
    }
}
