use super::*;
use std::num::NonZeroUsize;

pub(super) async fn run_mcp(cli: Cli, result_mode: mcp::McpResultMode) -> Result<()> {
    const PRODUCTION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
    let (server, service_state) = mcp::LeanTokenMcp::pending();
    let server = server.with_result_mode(result_mode);
    let runtime_contexts = server.context_registry();
    let mut server_task = tokio::spawn(mcp::serve_stdio_server(server));

    tokio::select! {
        result = &mut server_task => return result?,
        () = service_state.wait_initialized() => {}
    }

    let cancellation = CancellationToken::new();
    let runtime_cancellation = cancellation.clone();
    let runtime_state = service_state.clone();
    let mut runtime_task = tokio::spawn(async move {
        run_mcp_runtime(cli, runtime_state, runtime_contexts, runtime_cancellation).await
    });
    let failure_state = service_state;

    tokio::select! {
        server = &mut server_task => {
            cancellation.cancel();
            let server = server?;
            let runtime = tokio::time::timeout(PRODUCTION_SHUTDOWN_TIMEOUT, runtime_task)
                .await
                .map_err(|_| leantoken::Error::ShutdownTimeout {
                    component: "MCP indexing runtime",
                })??;
            server?;
            match runtime {
                Ok(()) | Err(leantoken::Error::Cancelled) => Ok(()),
                Err(error) => Err(error),
            }
        }
        runtime = &mut runtime_task => {
            let error = match runtime {
                Ok(Ok(())) => leantoken::Error::McpRuntimeStopped,
                Ok(Err(error)) => error,
                Err(error) => error.into(),
            };
            failure_state.set_failed(&error);
            tracing::error!(%error, "MCP indexing runtime failed");

            // A repository runtime failure is an operational tool failure, not
            // an MCP transport failure. Keep the initialized protocol alive so
            // clients can discover the catalog and receive the bounded failed
            // service state until they close stdin.
            match server_task.await {
                Ok(Ok(())) => {}
                Ok(Err(server_error)) => {
                    tracing::warn!(%server_error, "MCP transport failed after indexing runtime stopped");
                }
                Err(join_error) => {
                    tracing::warn!(%join_error, "MCP transport task failed after indexing runtime stopped");
                }
            }
            Err(error)
        }
    }
}

pub(super) async fn run_mcp_runtime(
    cli: Cli,
    service_state: mcp::McpServices,
    contexts: mcp::McpContextRegistry,
    cancellation: CancellationToken,
) -> Result<()> {
    let startup_cancellation = cancellation.clone();
    let startup_state = service_state.clone();
    let use_background_worker_default = cli.max_index_workers.is_none();
    let context_cli = cli.clone();
    let mut config = tokio::task::spawn_blocking(move || cli.config()).await??;
    let approved_contexts = config.approved_repository_contexts()?;
    let mut approved_contexts = approved_contexts
        .into_iter()
        .map(|approved| {
            let context_state = mcp::McpServices::starting_default();
            contexts.register(approved.name.clone(), context_state.clone())?;
            Ok::<_, leantoken::Error>((approved, context_state))
        })
        .collect::<Result<Vec<_>>>()?;

    // MCP indexing is background work. Reserve host capacity for protocol
    // handling and sibling agents unless the user made concurrency explicit.
    config.max_index_workers =
        mcp_index_worker_limit(config.max_index_workers, !use_background_worker_default);

    // Process-wide indexing budget: each approved context independently
    // configures its own indexing workers. Log the aggregate so operators
    // know the total process-wide concurrency. See issue #565: with K
    // approved contexts the process-wide default is (1 + K) workers.
    let process_wide_workers = config
        .max_index_workers
        .saturating_add(approved_contexts.len());
    let cpu_capacity = std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(1);
    if process_wide_workers > cpu_capacity {
        tracing::warn!(
            process_wide_indexing_workers = process_wide_workers,
            approved_context_count = approved_contexts.len(),
            cpu_capacity,
            "process-wide indexing workers exceed CPU capacity; consider setting              --max-index-workers to bound total concurrency"
        );
    }
    let startup = tokio::task::spawn_blocking(move || {
        startup_state.configure_limits(&config)?;
        Services::open_cancellable(config, &startup_cancellation)
    })
    .await;
    let services = match startup {
        Ok(Ok(services)) => Arc::new(services),
        Ok(Err(error)) => {
            for (_, context_state) in &approved_contexts {
                context_state.set_failed(&error);
            }
            return Err(error);
        }
        Err(error) => {
            let error: leantoken::Error = error.into();
            for (_, context_state) in &approved_contexts {
                context_state.set_failed(&error);
            }
            return Err(error);
        }
    };
    if cancellation.is_cancelled() {
        let error = leantoken::Error::Cancelled;
        for (_, context_state) in &approved_contexts {
            context_state.set_failed(&error);
        }
        return Err(error);
    }
    service_state.set_ready(Arc::clone(&services));
    let mut context_tasks = Vec::with_capacity(approved_contexts.len());
    for (approved, context_state) in approved_contexts.drain(..) {
        let context_cancellation = cancellation.clone();
        let startup_cancellation = context_cancellation.clone();
        let context_name = approved.name.clone();
        let context_cli = context_cli.clone();
        context_tasks.push(tokio::spawn(async move {
            let startup = tokio::task::spawn_blocking(move || {
                let mut config = context_cli.config_for_root(approved.root, None)?;
                config.max_index_workers = mcp_index_worker_limit(
                    config.max_index_workers,
                    context_cli.max_index_workers.is_some(),
                );
                leantoken::services::Services::open_cancellable(config, &startup_cancellation)
            })
            .await;
            match startup {
                Ok(Ok(services)) => {
                    let services = Arc::new(services);
                    context_state.set_ready(Arc::clone(&services));
                    if let Err(error) = run_mcp_index_loop(services, context_cancellation).await {
                        context_state.set_failed(&error);
                        tracing::error!(context = %context_name, %error, "approved repository context stopped");
                    }
                }
                Ok(Err(error)) => {
                    context_state.set_failed(&error);
                    tracing::error!(context = %context_name, %error, "approved repository context failed to start");
                }
                Err(error) => {
                    let error: leantoken::Error = error.into();
                    context_state.set_failed(&error);
                    tracing::error!(context = %context_name, %error, "approved repository context startup task failed");
                }
            }
        }));
    }
    let result = run_mcp_index_loop(services, cancellation.clone()).await;
    cancellation.cancel();
    for context_task in context_tasks {
        if let Err(error) = context_task.await {
            tracing::warn!(%error, "approved repository context task failed to join during shutdown");
        }
    }
    result
}

async fn run_mcp_index_loop(
    services: Arc<leantoken::services::Services>,
    cancellation: CancellationToken,
) -> Result<()> {
    let mut leadership_backoff =
        RetryBackoff::new(INDEX_RETRY_INITIAL_DELAY, INDEX_RETRY_MAX_DELAY);
    let mut follower_backoff =
        RetryBackoff::new(LEADERSHIP_POLL_INITIAL_DELAY, LEADERSHIP_POLL_MAX_DELAY);

    loop {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        let services_for_leadership = Arc::clone(&services);
        let leader = tokio::task::spawn_blocking(move || {
            services_for_leadership.try_acquire_index_leadership()
        })
        .await??;

        let retry_delay;
        if let Some(leader) = leader {
            follower_backoff.reset();
            let result = run_index_leader(Arc::clone(&services), cancellation.clone()).await;
            drop(leader);
            if cancellation.is_cancelled() {
                return Ok(());
            }
            if let Err(error) = result {
                if is_terminal_index_error(&error) {
                    return Err(error);
                }
                retry_delay = leadership_backoff.failure_delay();
                tracing::error!(
                    %error,
                    retry_delay_ms = retry_delay.as_millis(),
                    "automatic indexing leadership failed"
                );
            } else {
                leadership_backoff.reset();
                retry_delay = LEADERSHIP_POLL_INITIAL_DELAY;
            }
        } else {
            retry_delay = follower_backoff.failure_delay();
        }

        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            _ = tokio::time::sleep(retry_delay) => {}
        }
    }
}
