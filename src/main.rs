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
    doctor, episode, mcp,
    model::{IndexConsistency, IndexState},
    services::{ServiceCallOptions, Services},
    setup::{self, SetupOperation},
    upgrade,
    watcher::{RepositoryWatcher, WatcherAction, WatcherMessage, WatcherReconciliationScheduler},
};
use serde::Serialize;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

fn service_call_options(max_response_tokens: Option<usize>) -> ServiceCallOptions {
    max_response_tokens.map_or_else(ServiceCallOptions::new, |limit| {
        ServiceCallOptions::new().with_max_response_tokens(limit)
    })
}

mod savings;

const WATCHER_QUEUE_CAPACITY: usize = 1;
const INDEX_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(500);
const INDEX_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const LEADERSHIP_POLL_INITIAL_DELAY: Duration = Duration::from_millis(500);
const LEADERSHIP_POLL_MAX_DELAY: Duration = Duration::from_secs(8);

fn mcp_index_worker_limit(configured: usize, explicitly_configured: bool) -> usize {
    if explicitly_configured { configured } else { 1 }
}

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
    let cli = Cli::try_parse_from(arguments.clone())
        .unwrap_or_else(|error| exit_cli_error(error, json_requested));
    if let Err(error) = cli.validate_option_scope(&arguments) {
        exit_cli_error(error, json_requested);
    }
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

fn exit_cli_error(error: clap::Error, json_requested: bool) -> ! {
    if json_requested && error.use_stderr() {
        let exit_code = error.exit_code();
        eprintln!("{}", serde_json::json!(cli_parse_error_response(&error)));
        std::process::exit(exit_code);
    }
    error.exit()
}

// Binary orchestration is split by lifecycle while sharing this private
// module scope; application policy remains owned by Services.
include!("main/dispatch.rs");
include!("main/mcp_runtime.rs");
include!("main/index_runtime.rs");
include!("main/output.rs");

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
