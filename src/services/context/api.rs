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
        self.context_execute(
            request,
            ContextExecution::new(ContextWorkflow::Auto),
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    /// Select ranked task evidence using typed caller-observed workflow signals.
    pub async fn context_with_workflow_evidence(
        &self,
        request: ContextRequest,
        workflow_evidence: WorkflowEvidence,
    ) -> Result<ContextResponse> {
        self.context_execute(
            request,
            ContextExecution::new(ContextWorkflow::Auto).with_workflow_evidence(workflow_evidence),
            RetrievalExecution::direct(ServiceCallOptions::default(), CancellationToken::new()),
        )
        .await
    }

    /// Select context and attach compact provenance for a host-triggered handoff.
    pub async fn context_with_handoff(
        &self,
        request: ContextRequest,
        handoff: HandoffManifestRequest,
    ) -> Result<ContextResponse> {
        self.context_execute(
            request,
            ContextExecution::new(ContextWorkflow::Auto).with_handoff(handoff),
            RetrievalExecution::direct(ServiceCallOptions::default(), CancellationToken::new()),
        )
        .await
    }

    /// Retrieve context after applying the requested index consistency boundary.
    pub async fn context_with_consistency_cancellable(
        &self,
        request: ContextRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        self.context_execute(
            request,
            ContextExecution::new(ContextWorkflow::Auto),
            RetrievalExecution::consistent(
                consistency,
                ServiceCallOptions::default(),
                cancellation,
            ),
        )
        .await
    }

    /// Retrieve context under an explicit or auto-detected workflow.
    pub async fn context_with_workflow_consistency_cancellable(
        &self,
        request: ContextRequest,
        workflow: ContextWorkflow,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        self.context_execute(
            request,
            ContextExecution::new(workflow),
            RetrievalExecution::consistent(
                consistency,
                ServiceCallOptions::default(),
                cancellation,
            ),
        )
        .await
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
        self.context_execute(
            request,
            ContextExecution::new(workflow).with_handoff(handoff),
            RetrievalExecution::consistent(
                consistency,
                ServiceCallOptions::default(),
                cancellation,
            ),
        )
        .await
    }

    pub async fn context_cancellable(
        &self,
        request: ContextRequest,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        self.context_execute(
            request,
            ContextExecution::new(ContextWorkflow::Auto),
            RetrievalExecution::direct(ServiceCallOptions::default(), cancellation),
        )
        .await
    }

    /// Retrieve context with typed caller-observed workflow evidence.
    pub async fn context_with_workflow_options_consistency_cancellable(
        &self,
        params: ContextWorkflowOptions,
    ) -> Result<ContextResponse> {
        let ContextWorkflowOptions {
            request,
            handoff,
            workflow,
            workflow_evidence,
            consistency,
            options,
            cancellation,
        } = params;
        self.context_execute(
            request,
            ContextExecution {
                handoff,
                workflow,
                workflow_evidence,
            },
            RetrievalExecution::consistent(consistency, options, cancellation),
        )
        .await
    }

    /// Retrieve context with workflow selection and serialized-response controls.
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
        self.context_with_workflow_options_consistency_cancellable(ContextWorkflowOptions {
            request,
            handoff,
            workflow,
            workflow_evidence: WorkflowEvidence::default(),
            consistency,
            options,
            cancellation,
        })
        .await
    }

    /// Retrieve context with caller-observed workflow evidence and explicit controls.
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
        self.context_with_workflow_options_consistency_cancellable(ContextWorkflowOptions {
            request,
            handoff,
            workflow,
            workflow_evidence,
            consistency,
            options,
            cancellation,
        })
        .await
    }

    async fn context_execute(
        &self,
        request: ContextRequest,
        context: ContextExecution,
        execution: RetrievalExecution,
    ) -> Result<ContextResponse> {
        let operation = context_accounting_operation(&request);
        let RetrievalExecution {
            consistency,
            options,
            cancellation,
        } = execution;
        let parsed = self.observe_service_result(
            operation,
            self.parse_context_input(request, context, options),
        )?;

        if let Some(consistency) = consistency {
            let consistency_result = self
                .apply_consistency_with_initial_deadline(
                    consistency,
                    cancellation.clone(),
                    options.initial_reconciliation_deadline(),
                )
                .await;
            self.observe_service_result(operation, consistency_result)?;
        }

        let accounted = self.context_run(parsed, options, cancellation).await?;
        let mut response = accounted.response;
        if let Some(consistency) = consistency {
            set_routing_consistency(&mut response, consistency);
            let finalize_result = if options.mcp_response_shape().is_some() {
                self.finalize_bounded_response(&mut response, options)
            } else {
                self.finalize_response(&mut response)
            };
            self.observe_service_result(operation, finalize_result)?;
            if let Some(max_response_tokens) = options.max_response_tokens()
                && response.meta.total_response_tokens > max_response_tokens
            {
                return self.observe_service_result(
                    operation,
                    Err(Self::response_budget_exceeded(
                        &response.meta,
                        max_response_tokens,
                        response.meta.total_response_tokens,
                    )),
                );
            }
        }
        Ok(response)
    }

    async fn context_run(
        &self,
        parsed: super::execution::ParsedContextRequest,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<AccountedContextResponse> {
        let operation = context_accounting_operation(&parsed.request);
        let this = self.clone();
        let result = self
            .process_budget
            .run(cancellation, move |cancellation| {
                let evaluation = this.context_sync(super::execution::ContextSyncRequest {
                    parsed,
                    retrieval: super::execution::ContextRetrieval {
                        options,
                        cancellation,
                        diagnostics: CandidateDiagnostics::Omit,
                        signals: ContextSignals::PRODUCTION,
                    },
                })?;
                Ok(AccountedContextResponse {
                    response: evaluation.response,
                })
            })
            .await;
        self.observe_service_result(operation, result)
    }

    /// Retrieve context and expose pre-selection candidate paths for evaluation.
    ///
    /// Production adapters should use [`Self::context`]. This method exists for
    /// frozen retrieval benchmarks and does not alter the MCP response schema.
    pub async fn context_evaluation(&self, request: ContextRequest) -> Result<ContextEvaluation> {
        let parsed = self.parse_context_input(
            request,
            ContextExecution::new(ContextWorkflow::Implementation),
            ServiceCallOptions::default(),
        )?;
        let this = self.clone();
        self.process_budget
            .run(CancellationToken::new(), move |cancellation| {
                this.context_sync(super::execution::ContextSyncRequest {
                    parsed,
                    retrieval: super::execution::ContextRetrieval {
                        options: ServiceCallOptions::default(),
                        cancellation,
                        diagnostics: CandidateDiagnostics::Collect,
                        signals: ContextSignals::PRODUCTION,
                    },
                })
            })
            .await
    }

    /// Evaluate typed workflow evidence while exposing pre-selection candidates.
    pub async fn context_evaluation_with_workflow_evidence(
        &self,
        request: ContextRequest,
        workflow_evidence: WorkflowEvidence,
    ) -> Result<ContextEvaluation> {
        let parsed = self.parse_context_input(
            request,
            ContextExecution::new(ContextWorkflow::Implementation)
                .with_workflow_evidence(workflow_evidence),
            ServiceCallOptions::default(),
        )?;
        let this = self.clone();
        self.process_budget
            .run(CancellationToken::new(), move |cancellation| {
                this.context_sync(super::execution::ContextSyncRequest {
                    parsed,
                    retrieval: super::execution::ContextRetrieval {
                        options: ServiceCallOptions::default(),
                        cancellation,
                        diagnostics: CandidateDiagnostics::Collect,
                        signals: ContextSignals::PRODUCTION,
                    },
                })
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
        let parsed = self.parse_context_input(
            request,
            ContextExecution::new(ContextWorkflow::Implementation),
            ServiceCallOptions::default(),
        )?;
        let this = self.clone();
        self.process_budget
            .run(CancellationToken::new(), move |cancellation| {
                this.context_sync(super::execution::ContextSyncRequest {
                    parsed,
                    retrieval: super::execution::ContextRetrieval {
                        options: ServiceCallOptions::default(),
                        cancellation,
                        diagnostics: CandidateDiagnostics::Collect,
                        signals: ContextSignals::evaluation(policy),
                    },
                })
            })
            .await
    }
}
use super::*;
