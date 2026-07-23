use std::{
    ffi::{OsStr, OsString},
    io::Write,
    sync::Arc,
    time::Duration,
};

use clap::Parser;
use leantoken::{
    Result, cache,
    cli::{AppRequest, Cli},
    doctor, mcp,
    services::Services,
    setup::{self, SetupOperation},
    upgrade,
    watcher::{RepositoryWatcher, WatcherAction, WatcherMessage, WatcherReconciliationScheduler},
};
use serde::Serialize;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

mod savings;

const WATCHER_QUEUE_CAPACITY: usize = 1;
const INDEX_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(500);
const INDEX_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const LEADERSHIP_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug)]
struct RetryBackoff {
    initial: Duration,
    maximum: Duration,
    next: Duration,
}

impl RetryBackoff {
    fn new(initial: Duration, maximum: Duration) -> Self {
        Self {
            initial,
            maximum,
            next: initial,
        }
    }

    fn failure_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = self.next.saturating_mul(2).min(self.maximum);
        delay
    }

    fn reset(&mut self) {
        self.next = self.initial;
    }
}

#[tokio::main]
async fn main() {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let json_requested = cli_json_requested(&arguments);
    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error) if json_requested && error.use_stderr() => {
            let exit_code = error.exit_code();
            eprintln!("{}", serde_json::json!(cli_parse_error_response(&error)));
            std::process::exit(exit_code);
        }
        Err(error) => error.exit(),
    };
    let json = cli.json;
    init_tracing(json);
    if let Err(error) = run(cli).await {
        if json {
            eprintln!("{}", serde_json::json!(cli_error_response(&error)));
        } else {
            let message = cli_error_message(&error);
            eprintln!("Error: {message}");
        }
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let json = cli.json;

    if matches!(
        &cli.command,
        leantoken::cli::Commands::Update(_) | leantoken::cli::Commands::Upgrade(_)
    ) {
        let AppRequest::Upgrade { check, yes } = cli.app_request() else {
            unreachable!("upgrade command checked above")
        };
        upgrade::run(upgrade::UpgradeOptions { check, yes, json })?;
        return Ok(());
    }

    if matches!(&cli.command, leantoken::cli::Commands::Cache(_)) {
        match cli.app_request() {
            AppRequest::CacheList => {
                let report = cache::list()?;
                cache::print_list(&report, json)?;
            }
            AppRequest::CachePrune(request) => {
                let report = cache::prune(&request)?;
                cache::print_prune(&report, json)?;
                if report.has_failures() {
                    return Err(leantoken::Error::InternalFailure(
                        "one or more managed caches could not be pruned".into(),
                    ));
                }
            }
            _ => unreachable!("cache command checked above"),
        }
        return Ok(());
    }

    if matches!(
        &cli.command,
        leantoken::cli::Commands::Setup(_) | leantoken::cli::Commands::Remove(_)
    ) {
        let (operation, request) = match cli.app_request() {
            AppRequest::Setup(request) => (SetupOperation::Setup, request),
            AppRequest::Remove(request) => (SetupOperation::Remove, request),
            _ => unreachable!("integration command checked above"),
        };
        let report = setup::run(operation, request, json)?;
        setup::print_report(&report, json)?;
        if report.has_failures() {
            return Err(leantoken::Error::InternalFailure(
                "one or more MCP client configurations failed".into(),
            ));
        }
        return Ok(());
    }

    if let leantoken::cli::Commands::Mcp(args) = &cli.command {
        let result_mode = args.result_mode;
        return run_mcp(cli, result_mode).await;
    }

    let config = cli.config()?;
    let request = cli.app_request();

    if let AppRequest::Doctor = request {
        if !json {
            doctor::print_progress()?;
        }
        let report = doctor::run(&config)?;
        doctor::print_report(&report, json)?;
        return Ok(());
    }

    if matches!(&request, AppRequest::Status) {
        return print(&Services::status_without_initializing(config)?, json);
    }

    let services = Arc::new(Services::open(config)?);

    match request {
        AppRequest::Index { rebuild } => print(&services.index_report(rebuild).await?, json),
        AppRequest::Status => unreachable!("handled before service setup"),
        AppRequest::Savings => savings::print_report(&services.token_savings().await?, json),
        AppRequest::Files(request) => print(&services.files(request).await?, json),
        AppRequest::Search(request) => print(&services.search(request).await?, json),
        AppRequest::Outline(request) => print(&services.outline(request).await?, json),
        AppRequest::Read(request) => print(&services.read(request).await?, json),
        AppRequest::Context(request) => print(&services.context(request).await?, json),
        AppRequest::Doctor => unreachable!("handled before service setup"),
        AppRequest::Mcp { .. } => unreachable!("handled before service setup"),
        AppRequest::Setup(_) | AppRequest::Remove(_) => {
            unreachable!("handled before service setup")
        }
        AppRequest::CacheList | AppRequest::CachePrune(_) => {
            unreachable!("handled before repository setup")
        }
        AppRequest::Upgrade { .. } => unreachable!("handled before repository setup"),
    }
}

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
            failure_state.set_failed();
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
            let config = cli.config()?;
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

    loop {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        let services_for_leadership = Arc::clone(&services);
        let leader = tokio::task::spawn_blocking(move || {
            services_for_leadership.try_acquire_index_leadership()
        })
        .await??;

        let mut retry_delay = LEADERSHIP_POLL_INTERVAL;
        if let Some(leader) = leader {
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
            }
        }

        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            _ = tokio::time::sleep(retry_delay) => {}
        }
    }
}

async fn run_index_leader(services: Arc<Services>, cancellation: CancellationToken) -> Result<()> {
    let (watcher, changes) = RepositoryWatcher::start_with_policy(
        &services.config().root,
        WATCHER_QUEUE_CAPACITY,
        services.config().watcher_debounce,
        services.config().discovery_policy(),
        cancellation.child_token(),
    )
    .await?;

    let result = run_index_leader_until_shutdown(services, changes, cancellation).await;
    let shutdown = watcher.shutdown().await;
    match (result, shutdown) {
        (Err(error), Err(shutdown_error)) => {
            tracing::warn!(%shutdown_error, "watcher shutdown failed after index error");
            Err(error)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), shutdown) => shutdown,
    }
}

async fn run_index_leader_until_shutdown(
    services: Arc<Services>,
    changes: tokio::sync::mpsc::Receiver<WatcherMessage>,
    cancellation: CancellationToken,
) -> Result<()> {
    // The watcher is registered before the scan. Events queued during the scan
    // are applied afterward, closing the startup gap without a second walk.
    let indexed = services
        .index_cancellable(false, cancellation.clone())
        .await;
    let indexed = match indexed {
        Ok(indexed) => indexed,
        Err(leantoken::Error::Cancelled) if cancellation.is_cancelled() => return Ok(()),
        Err(error) => return Err(error),
    };
    for warning in &indexed.warnings {
        tracing::warn!(%warning, "index warning");
    }

    run_watcher_reconciliations(services, changes, cancellation).await
}

async fn run_watcher_reconciliations(
    services: Arc<Services>,
    mut changes: tokio::sync::mpsc::Receiver<WatcherMessage>,
    cancellation: CancellationToken,
) -> Result<()> {
    let mut scheduler = WatcherReconciliationScheduler::new(services.config().watcher_debounce);

    loop {
        let changes_open = drain_watcher_messages(&mut scheduler, &services, &mut changes);

        let Some(deadline) = scheduler.next_deadline() else {
            if !changes_open {
                break;
            }
            tokio::select! {
                _ = cancellation.cancelled() => break,
                message = changes.recv() => match message {
                    Some(message) => schedule_watcher_message(&mut scheduler, &services, message),
                    None => break,
                }
            }
            continue;
        };

        tokio::select! {
            _ = cancellation.cancelled() => break,
            message = changes.recv(), if changes_open => match message {
                Some(message) => schedule_watcher_message(&mut scheduler, &services, message),
                None => continue,
            },
            _ = tokio::time::sleep_until(deadline) => {
                let Some(action) = scheduler.take_ready(Instant::now()) else {
                    continue;
                };
                if !execute_watcher_action(
                    &mut scheduler,
                    Arc::clone(&services),
                    action,
                    cancellation.clone(),
                ).await? {
                    break;
                }
            }
        }
    }

    Ok(())
}

fn drain_watcher_messages(
    scheduler: &mut WatcherReconciliationScheduler,
    services: &Services,
    changes: &mut tokio::sync::mpsc::Receiver<WatcherMessage>,
) -> bool {
    loop {
        match changes.try_recv() {
            Ok(message) => schedule_watcher_message(scheduler, services, message),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return true,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return false,
        }
    }
}

async fn execute_watcher_action(
    scheduler: &mut WatcherReconciliationScheduler,
    services: Arc<Services>,
    action: WatcherAction,
    cancellation: CancellationToken,
) -> Result<bool> {
    match reconcile_watcher_action(services, &action, cancellation.clone()).await {
        Ok(indexed) => {
            scheduler.finish_success(&action, Instant::now());
            for warning in &indexed.warnings {
                tracing::warn!(%warning, "index warning");
            }
            Ok(true)
        }
        Err(leantoken::Error::Cancelled) if cancellation.is_cancelled() => Ok(false),
        Err(error) if is_terminal_index_error(&error) => Err(error),
        Err(error) => {
            scheduler.finish_failure(action, Instant::now());
            let retry_at = scheduler.next_deadline();
            tracing::error!(
                %error,
                retry_delay_ms = retry_at.map_or(0, |at| at.saturating_duration_since(Instant::now()).as_millis()),
                "background reconciliation failed; retained for retry"
            );
            Ok(true)
        }
    }
}

fn is_terminal_index_error(error: &leantoken::Error) -> bool {
    matches!(
        error,
        leantoken::Error::RootNotFound(_)
            | leantoken::Error::UnsafeRepositoryRoot(_)
            | leantoken::Error::IndexLimitExceeded { .. }
            | leantoken::Error::InvalidConfiguration(_)
            | leantoken::Error::RepositoryMismatch { .. }
            | leantoken::Error::RuntimeCapabilityUnavailable { .. }
    )
}

fn schedule_watcher_message(
    scheduler: &mut WatcherReconciliationScheduler,
    services: &Services,
    message: WatcherMessage,
) {
    let message = match message {
        WatcherMessage::Changed { paths } => {
            let paths = paths
                .into_iter()
                .filter(|path| !services.config().is_database_artifact(path))
                .collect::<Vec<_>>();
            if paths.is_empty() {
                return;
            }
            WatcherMessage::Changed { paths }
        }
        WatcherMessage::ReconcileRequired => WatcherMessage::ReconcileRequired,
    };
    scheduler.enqueue(message, Instant::now());
}

async fn reconcile_watcher_action(
    services: Arc<Services>,
    action: &WatcherAction,
    cancellation: CancellationToken,
) -> Result<leantoken::model::IndexResponse> {
    match action {
        WatcherAction::Paths(paths) => {
            tracing::debug!(changed_paths = paths.len(), "repository change detected");
            services
                .index_paths_cancellable(paths.clone(), cancellation)
                .await
        }
        WatcherAction::Full => {
            tracing::warn!("watcher scheduled bounded full reconciliation");
            services.index_cancellable(false, cancellation).await
        }
    }
}

fn print<T: Serialize>(value: &T, compact: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if compact {
        serde_json::to_writer(&mut lock, value)?;
    } else {
        serde_json::to_writer_pretty(&mut lock, value)?;
    }
    lock.write_all(b"\n")?;
    Ok(())
}

fn cli_error_message(error: &leantoken::Error) -> String {
    match error {
        leantoken::Error::IndexNotReady => "repository index is not ready; run `leantoken index` \
            for direct CLI use or `leantoken doctor` to verify MCP readiness"
            .into(),
        _ => error.to_string(),
    }
}

fn cli_json_requested(arguments: &[OsString]) -> bool {
    arguments
        .iter()
        .skip(1)
        .take_while(|argument| argument.as_os_str() != OsStr::new("--"))
        .any(|argument| argument.as_os_str() == OsStr::new("--json"))
}

#[derive(Debug, Serialize)]
struct CliErrorResponse {
    error: String,
    category: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

fn cli_parse_error_response(error: &clap::Error) -> CliErrorResponse {
    use clap::error::ErrorKind;

    let category = match error.kind() {
        ErrorKind::InvalidValue
        | ErrorKind::UnknownArgument
        | ErrorKind::InvalidSubcommand
        | ErrorKind::NoEquals
        | ErrorKind::ValueValidation
        | ErrorKind::TooManyValues
        | ErrorKind::TooFewValues
        | ErrorKind::WrongNumberOfValues
        | ErrorKind::ArgumentConflict
        | ErrorKind::MissingRequiredArgument
        | ErrorKind::MissingSubcommand
        | ErrorKind::InvalidUtf8
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => "invalid_input",
        _ => "internal_error",
    };

    CliErrorResponse {
        error: error.to_string().trim_end().to_owned(),
        category,
        field: None,
        requested: None,
        limit: None,
    }
}

fn cli_error_response(error: &leantoken::Error) -> CliErrorResponse {
    let (category, field, requested, limit) = match error {
        leantoken::Error::InvalidInput { field, .. } => ("invalid_input", Some(*field), None, None),
        leantoken::Error::InputTooLong { field, max_bytes } => {
            ("input_too_long", Some(*field), None, Some(*max_bytes))
        }
        leantoken::Error::RequestLimitExceeded {
            field,
            requested,
            limit,
        } => (
            "request_limit_exceeded",
            Some(*field),
            Some(*requested),
            Some(*limit),
        ),
        leantoken::Error::LimitExceeded => ("request_limit_exceeded", None, None, None),
        leantoken::Error::NotIndexed(_) => ("not_indexed", None, None, None),
        leantoken::Error::IndexNotReady => ("index_not_ready", None, None, None),
        leantoken::Error::StaleCursor => ("stale_cursor", None, None, None),
        leantoken::Error::Cancelled => ("request_cancelled", None, None, None),
        leantoken::Error::PathOutsideRoot(_) => ("path_outside_root", None, None, None),
        leantoken::Error::UnsupportedLanguage(_) => ("unsupported_language", None, None, None),
        leantoken::Error::InvalidRequest(_) => ("invalid_request", None, None, None),
        leantoken::Error::Regex(_) => ("invalid_regex", None, None, None),
        leantoken::Error::Glob(_) => ("invalid_glob", None, None, None),
        leantoken::Error::RootNotFound(_)
        | leantoken::Error::UnsafeRepositoryRoot(_)
        | leantoken::Error::RepositoryMismatch { .. }
        | leantoken::Error::InvalidConfiguration(_) => {
            ("repository_configuration", None, None, None)
        }
        leantoken::Error::IndexLimitExceeded { .. } => ("repository_index_limit", None, None, None),
        leantoken::Error::RuntimeCapabilityUnavailable { .. } => {
            ("runtime_unavailable", None, None, None)
        }
        leantoken::Error::StaleReconciliation { .. } | leantoken::Error::RetryableConflict(_) => {
            ("retryable_conflict", None, None, None)
        }
        _ => ("internal_error", None, None, None),
    };

    CliErrorResponse {
        error: cli_error_message(error),
        category,
        field,
        requested,
        limit,
    }
}

fn init_tracing(json: bool) {
    if json {
        return;
    }

    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::filter::FilterFn;
    use tracing_subscriber::prelude::*;

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));

    // Safety-net filter: reject any log event that carries a field name which
    // could contain source content.  By design, LeanToken logs only paths,
    // counts, hashes, timings, and error summaries.  This filter acts as a
    // structural invariant that prevents source bodies from ever appearing in
    // structured log output.
    let scrub_fields = FilterFn::new(|meta: &tracing::Metadata<'_>| -> bool {
        let forbidden = [
            "source_body",
            "source_text",
            "file_content",
            "body",
            "token_text",
        ];
        !meta.fields().iter().any(|f| forbidden.contains(&f.name()))
    });

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .with_filter(env_filter)
                .with_filter(scrub_fields),
        )
        .init();
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use leantoken::error::IndexLimitKind;

    use super::*;

    #[test]
    fn retry_backoff_is_exponential_and_capped() {
        let mut backoff = RetryBackoff::new(Duration::from_millis(10), Duration::from_millis(25));
        assert_eq!(backoff.failure_delay(), Duration::from_millis(10));
        assert_eq!(backoff.failure_delay(), Duration::from_millis(20));
        assert_eq!(backoff.failure_delay(), Duration::from_millis(25));
        assert_eq!(backoff.failure_delay(), Duration::from_millis(25));
        backoff.reset();
        assert_eq!(backoff.failure_delay(), Duration::from_millis(10));
    }

    #[test]
    fn configuration_and_safety_errors_are_terminal_but_io_is_retryable() {
        let terminal = [
            leantoken::Error::RootNotFound(PathBuf::from("missing")),
            leantoken::Error::UnsafeRepositoryRoot(PathBuf::from("broad")),
            leantoken::Error::IndexLimitExceeded {
                kind: IndexLimitKind::Files,
                observed: 2,
                limit: 1,
            },
            leantoken::Error::InvalidConfiguration("invalid".into()),
            leantoken::Error::RuntimeCapabilityUnavailable {
                capability: "fts5",
                source: None,
            },
        ];
        assert!(terminal.iter().all(is_terminal_index_error));
        assert!(!is_terminal_index_error(&leantoken::Error::Io(
            std::io::Error::other("transient")
        )));
    }

    #[test]
    fn cli_error_json_has_exact_safe_metadata() {
        let cases = [
            (
                leantoken::Error::InvalidInput {
                    field: "query",
                    reason: "is required for find",
                },
                serde_json::json!({
                    "error": "invalid query: is required for find",
                    "category": "invalid_input",
                    "field": "query"
                }),
            ),
            (
                leantoken::Error::RequestLimitExceeded {
                    field: "max_results",
                    requested: 101,
                    limit: 100,
                },
                serde_json::json!({
                    "error": "max_results exceeds its configured limit: requested 101, limit 100",
                    "category": "request_limit_exceeded",
                    "field": "max_results",
                    "requested": 101,
                    "limit": 100
                }),
            ),
            (
                leantoken::Error::LimitExceeded,
                serde_json::json!({
                    "error": "requested content exceeds the configured limit",
                    "category": "request_limit_exceeded"
                }),
            ),
            (
                leantoken::Error::InputTooLong {
                    field: "query",
                    max_bytes: 65_536,
                },
                serde_json::json!({
                    "error": "query exceeds 65536 bytes",
                    "category": "input_too_long",
                    "field": "query",
                    "limit": 65_536
                }),
            ),
            (
                leantoken::Error::NotIndexed("missing.rs".into()),
                serde_json::json!({
                    "error": "path is not indexed: missing.rs",
                    "category": "not_indexed"
                }),
            ),
            (
                leantoken::Error::IndexNotReady,
                serde_json::json!({
                    "error": "repository index is not ready; run `leantoken index` for direct CLI use or `leantoken doctor` to verify MCP readiness",
                    "category": "index_not_ready"
                }),
            ),
            (
                leantoken::Error::StaleCursor,
                serde_json::json!({
                    "error": "stale cursor",
                    "category": "stale_cursor"
                }),
            ),
            (
                leantoken::Error::Cancelled,
                serde_json::json!({
                    "error": "request cancelled",
                    "category": "request_cancelled"
                }),
            ),
            (
                leantoken::Error::Io(std::io::Error::other("private descriptor")),
                serde_json::json!({
                    "error": "I/O error: private descriptor",
                    "category": "internal_error"
                }),
            ),
            (
                leantoken::Error::InvalidRequest("bad flags".into()),
                serde_json::json!({
                    "error": "invalid request: bad flags",
                    "category": "invalid_request"
                }),
            ),
            (
                leantoken::Error::InternalFailure("parser returned None".into()),
                serde_json::json!({
                    "error": "invalid request: parser returned None",
                    "category": "internal_error"
                }),
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(
                serde_json::to_value(cli_error_response(&error))
                    .expect("CLI error response is serializable"),
                expected
            );
        }
    }

    #[test]
    fn cli_parse_error_json_preserves_plain_clap_message() {
        let error = Cli::try_parse_from([
            "leantoken",
            "--json",
            "files",
            "tree",
            "--max-results",
            "nope",
        ])
        .expect_err("invalid numeric argument");

        assert_eq!(
            serde_json::to_value(cli_parse_error_response(&error))
                .expect("CLI parse error response is serializable"),
            serde_json::json!({
                "error": error.to_string().trim_end(),
                "category": "invalid_input"
            })
        );
    }

    #[test]
    fn cli_json_detection_ignores_values_after_the_argument_separator() {
        let arguments = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();

        assert!(cli_json_requested(&arguments(&[
            "leantoken",
            "files",
            "tree",
            "--json"
        ])));
        assert!(!cli_json_requested(&arguments(&[
            "leantoken",
            "search",
            "--",
            "--json"
        ])));
    }
}
