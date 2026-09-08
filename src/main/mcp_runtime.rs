use super::*;

const PRODUCTION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) async fn run_mcp(cli: Cli, result_mode: mcp::McpResultMode) -> Result<()> {
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
    let runtime_task = tokio::spawn(async move {
        run_mcp_runtime(cli, runtime_state, runtime_contexts, runtime_cancellation).await
    });
    supervise_mcp_tasks(server_task, runtime_task, service_state, cancellation).await
}

async fn supervise_mcp_tasks(
    mut server_task: tokio::task::JoinHandle<Result<()>>,
    mut runtime_task: tokio::task::JoinHandle<Result<()>>,
    failure_state: mcp::McpServices,
    cancellation: CancellationToken,
) -> Result<()> {
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
    let process_runtime = leantoken::services::ServicesRuntime::new(config.max_index_workers)?;
    let startup_runtime = process_runtime.clone();
    let startup = tokio::task::spawn_blocking(move || {
        startup_state.configure_limits(&config)?;
        Services::open_cancellable_in_runtime(config, &startup_cancellation, startup_runtime)
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
        let context_runtime = process_runtime.clone();
        context_tasks.push(tokio::spawn(async move {
            if let Err(error) = context_state
                .wait_for_activation(context_cancellation.clone())
                .await
            {
                if !matches!(error, leantoken::Error::Cancelled) {
                    context_state.set_failed(&error);
                }
                return;
            }
            let startup_state = context_state.clone();
            let startup = tokio::task::spawn_blocking(move || {
                let mut config =
                    context_cli.config_for_root(approved.root.into_path_buf(), None)?;
                config.max_index_workers = context_runtime.max_index_workers();
                startup_state.configure_limits(&config)?;
                leantoken::services::Services::open_cancellable_in_runtime(
                    config,
                    &startup_cancellation,
                    context_runtime,
                )
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

        if let Some(leader) = leader {
            follower_backoff.reset();
            let result = run_index_leader(Arc::clone(&services), cancellation.clone()).await;
            drop(leader);
            if cancellation.is_cancelled() {
                return Ok(());
            }
            wait_after_index_attempt(result, &mut leadership_backoff, &cancellation).await?;
            continue;
        }

        let retry_delay = follower_backoff.failure_delay();
        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            _ = tokio::time::sleep(retry_delay) => {}
        }
    }
}

async fn wait_after_index_attempt(
    result: Result<()>,
    backoff: &mut RetryBackoff,
    cancellation: &CancellationToken,
) -> Result<()> {
    let delay = match result {
        Err(error) if is_terminal_index_error(&error) => return Err(error),
        Err(error) => {
            let delay = backoff.failure_delay();
            tracing::error!(%error, retry_delay_ms = delay.as_millis(), "automatic indexing leadership failed");
            delay
        }
        Ok(()) => {
            backoff.reset();
            LEADERSHIP_POLL_INITIAL_DELAY
        }
    };
    tokio::select! {
        () = cancellation.cancelled() => {},
        () = tokio::time::sleep(delay) => {},
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn terminal_index_failure_returns_without_scheduling_retry() {
        let mut backoff = RetryBackoff::new(INDEX_RETRY_INITIAL_DELAY, INDEX_RETRY_MAX_DELAY);
        let started = Instant::now();
        let result = wait_after_index_attempt(
            Err(leantoken::Error::IndexLimitExceeded {
                kind: leantoken::error::IndexLimitKind::Files,
                observed: 2,
                limit: 1,
            }),
            &mut backoff,
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            result,
            Err(leantoken::Error::IndexLimitExceeded { .. })
        ));
        assert_eq!(started.elapsed(), Duration::ZERO);
        assert_eq!(backoff.failure_delay(), INDEX_RETRY_INITIAL_DELAY);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_wait_obeys_boundary_and_is_interruptible() {
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (waiting, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut backoff = RetryBackoff::new(INDEX_RETRY_INITIAL_DELAY, INDEX_RETRY_MAX_DELAY);
            waiting.send(()).unwrap();
            wait_after_index_attempt(
                Err(leantoken::Error::Io(std::io::Error::other("transient"))),
                &mut backoff,
                &worker_cancellation,
            )
            .await
        });
        ready.await.unwrap();
        tokio::time::advance(INDEX_RETRY_INITIAL_DELAY - Duration::from_millis(1)).await;
        assert!(!task.is_finished());
        cancellation.cancel();
        task.await.unwrap().unwrap();

        let mut backoff = RetryBackoff::new(INDEX_RETRY_INITIAL_DELAY, INDEX_RETRY_MAX_DELAY);
        let started = Instant::now();
        wait_after_index_attempt(
            Err(leantoken::Error::Io(std::io::Error::other("transient"))),
            &mut backoff,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(started.elapsed(), INDEX_RETRY_INITIAL_DELAY);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_runtime_keeps_transport_alive_past_shutdown_deadline() {
        let (close, closed) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            closed.await.expect("transport close");
            Ok(())
        });
        let runtime = tokio::spawn(async { Err(leantoken::Error::McpRuntimeStopped) });
        let task = tokio::spawn(supervise_mcp_tasks(
            server,
            runtime,
            mcp::McpServices::starting_default(),
            CancellationToken::new(),
        ));
        // Poll both completed runtime and supervisor before moving past the
        // production shutdown budget. No subprocess or wall-clock wait is used.
        tokio::task::yield_now().await;
        tokio::time::advance(PRODUCTION_SHUTDOWN_TIMEOUT * 2).await;
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "runtime failure must not close transport"
        );
        close.send(()).unwrap();
        assert!(matches!(
            task.await.unwrap(),
            Err(leantoken::Error::McpRuntimeStopped)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn transport_close_cancels_pending_runtime_delay() {
        let cancellation = CancellationToken::new();
        let runtime_cancellation = cancellation.clone();
        let runtime = tokio::spawn(async move {
            tokio::select! {
                () = runtime_cancellation.cancelled() => Err(leantoken::Error::Cancelled),
                () = tokio::time::sleep(Duration::from_secs(60)) => panic!("delay was not cancelled"),
            }
        });
        let server = tokio::spawn(async { Ok(()) });
        let started = Instant::now();
        supervise_mcp_tasks(
            server,
            runtime,
            mcp::McpServices::starting_default(),
            cancellation.clone(),
        )
        .await
        .expect("cancelled runtime joins cleanly");
        assert!(cancellation.is_cancelled());
        assert!(started.elapsed() < PRODUCTION_SHUTDOWN_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn transport_close_bounds_an_unresponsive_runtime_join() {
        let runtime = tokio::spawn(std::future::pending::<Result<()>>());
        let abort = runtime.abort_handle();
        let server = tokio::spawn(async { Ok(()) });
        let started = Instant::now();
        let result = supervise_mcp_tasks(
            server,
            runtime,
            mcp::McpServices::starting_default(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            result,
            Err(leantoken::Error::ShutdownTimeout {
                component: "MCP indexing runtime"
            })
        ));
        assert_eq!(started.elapsed(), PRODUCTION_SHUTDOWN_TIMEOUT);
        abort.abort();
    }
}
