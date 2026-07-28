async fn run_mcp(cli: Cli, result_mode: mcp::McpResultMode) -> Result<()> {
    let (server, service_state) = mcp::LeanTokenMcp::pending();
    let server = server.with_result_mode(result_mode);
    let mut server_task = tokio::spawn(mcp::serve_stdio_server(server));

    tokio::select! {
        result = &mut server_task => return result?,
        () = service_state.wait_initialized() => {}
    }

    let cancellation = CancellationToken::new();
    let runtime_cancellation = cancellation.clone();
    let runtime_state = service_state.clone();
    let mut runtime_task =
        tokio::spawn(
            async move { run_mcp_runtime(cli, runtime_state, runtime_cancellation).await },
        );
    let failure_state = service_state;

    tokio::select! {
        server = &mut server_task => {
            cancellation.cancel();
            let server = server?;
            let runtime = runtime_task.await?;
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

async fn run_mcp_runtime(
    cli: Cli,
    service_state: mcp::McpServices,
    cancellation: CancellationToken,
) -> Result<()> {
    let startup_cancellation = cancellation.clone();
    let startup_state = service_state.clone();
    let services = Arc::new(
        tokio::task::spawn_blocking(move || {
            let use_background_worker_default = cli.max_index_workers.is_none();
            let mut config = cli.config()?;
            // MCP indexing is background work. Reserve host capacity for
            // protocol handling and sibling agents unless the user made
            // concurrency explicit.
            config.max_index_workers =
                mcp_index_worker_limit(config.max_index_workers, !use_background_worker_default);
            startup_state.configure_limits(&config)?;
            Services::open_cancellable(config, &startup_cancellation)
        })
        .await??,
    );
    if cancellation.is_cancelled() {
        return Err(leantoken::Error::Cancelled);
    }
    service_state.set_ready(Arc::clone(&services));
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
