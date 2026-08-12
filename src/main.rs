use std::{
    ffi::{OsStr, OsString},
    sync::Arc,
};

use clap::Parser;
use leantoken::{
    Result,
    cli::{AppRequest, Cli, SearchProjectionArg},
    mcp,
    model::{IndexConsistency, SearchOccurrenceOutput},
    services::{ServiceCallOptions, Services},
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

fn service_call_options(max_response_tokens: Option<usize>) -> ServiceCallOptions {
    max_response_tokens.map_or_else(ServiceCallOptions::new, |limit| {
        ServiceCallOptions::new().with_max_response_tokens(limit)
    })
}

fn mcp_index_worker_limit(configured: usize, explicitly_configured: bool) -> usize {
    if explicitly_configured { configured } else { 1 }
}

#[tokio::main]
async fn main() {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let json_requested = cli_json_requested(&arguments);
    let cli = Cli::try_parse_from(arguments.clone())
        .unwrap_or_else(|error| exit_cli_error(error, json_requested));
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

#[path = "main/dispatch.rs"]
mod dispatch;
#[path = "main/mcp_runtime.rs"]
mod mcp_runtime;
#[path = "main/output.rs"]
mod output;

use dispatch::*;
use mcp_runtime::*;
use output::*;
