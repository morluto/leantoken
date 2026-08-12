use std::{
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::config::DEFAULT_CONTEXT_TOKENS;
use crate::mcp::McpResultMode;
use crate::model::{
    ContextRequest, ContextWorkflow, OutlineRequest, ReadRequest, SearchRequest, WorkflowEvidence,
};
use crate::tokens::Tokenizer;
use crate::{Config, Result};

fn parse_positive_usize(value: &str) -> std::result::Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| "value must be a positive integer".to_owned())
}

#[derive(Debug, Clone, Parser)]
#[command(name = "leantoken", version, about = "Immutable repository retrieval")]
pub struct Cli {
    #[arg(long, value_name = "PATH", global = true, default_value = ".")]
    pub root: PathBuf,
    #[arg(long, global = true)]
    pub allow_broad_root: bool,
    #[arg(long, global = true)]
    pub include_generated: bool,
    #[arg(long = "index-include", global = true, action = clap::ArgAction::Append)]
    pub index_include: Vec<String>,
    #[arg(long = "index-exclude", global = true, action = clap::ArgAction::Append)]
    pub index_exclude: Vec<String>,
    #[arg(long, global = true)]
    pub max_walk_entries: Option<NonZeroU64>,
    #[arg(long, global = true)]
    pub max_files: Option<NonZeroU64>,
    #[arg(long, global = true)]
    pub max_total_source_bytes: Option<NonZeroU64>,
    #[arg(long, global = true)]
    pub max_depth: Option<NonZeroUsize>,
    #[arg(long, global = true)]
    pub max_file_bytes: Option<NonZeroU64>,
    #[arg(long, global = true)]
    pub max_prepare_batch_files: Option<NonZeroUsize>,
    #[arg(long, global = true)]
    pub max_prepare_batch_bytes: Option<NonZeroU64>,
    #[arg(long, global = true)]
    pub max_index_workers: Option<NonZeroUsize>,
    #[arg(long, value_name = "PATH", global = true)]
    pub database: Option<PathBuf>,
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(long, value_enum, default_value_t = Tokenizer::default(), global = true)]
    pub tokenizer: Tokenizer,
    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
    pub fn config(&self) -> Result<Config> {
        let scope = crate::IndexScope::new(self.index_include.clone(), self.index_exclude.clone())?;
        let mut config = Config::discover_scoped_with_broad_root(
            &self.root,
            self.database.clone(),
            self.allow_broad_root,
            scope,
        )?;
        if let Some(value) = self.max_walk_entries {
            config.max_walk_entries = value.get();
        }
        if let Some(value) = self.max_files {
            config.max_files = value.get();
        }
        if let Some(value) = self.max_total_source_bytes {
            config.max_total_source_bytes = value.get();
        }
        if let Some(value) = self.max_depth {
            config.max_depth = value.get();
        }
        if let Some(value) = self.max_file_bytes {
            config.max_file_bytes = value.get();
        }
        if let Some(value) = self.max_prepare_batch_files {
            config.max_prepare_batch_files = value.get();
        }
        if let Some(value) = self.max_prepare_batch_bytes {
            config.max_prepare_batch_bytes = value.get();
        }
        if let Some(value) = self.max_index_workers {
            config.max_index_workers = value.get();
        }
        config.include_generated = self.include_generated;
        config.tokenizer = self.tokenizer;
        config.discovery_limits().validate()?;
        Ok(config)
    }

    #[must_use]
    pub fn app_request(&self) -> AppRequest {
        match &self.command {
            Commands::Refresh => AppRequest::Refresh,
            Commands::Search(args) => AppRequest::Search {
                request: args.clone().into(),
                projection: args.projection,
                max_response_tokens: args.max_response_tokens,
            },
            Commands::Outline(args) => AppRequest::Outline {
                request: args.clone().into(),
                max_response_tokens: args.max_response_tokens,
            },
            Commands::Read(args) => AppRequest::Read {
                request: args.clone().into(),
                max_response_tokens: args.max_response_tokens,
            },
            Commands::Context(args) => AppRequest::Context {
                request: Box::new(args.request()),
                workflow: args.workflow.into(),
                workflow_evidence: args.workflow_evidence(),
                max_response_tokens: args.max_response_tokens,
                response_profile: args.response_profile.map(Into::into),
            },
            Commands::Mcp(args) => AppRequest::Mcp {
                result_mode: args.result_mode,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub enum AppRequest {
    Refresh,
    Search {
        request: SearchRequest,
        projection: SearchProjectionArg,
        max_response_tokens: Option<usize>,
    },
    Outline {
        request: OutlineRequest,
        max_response_tokens: Option<usize>,
    },
    Read {
        request: ReadRequest,
        max_response_tokens: Option<usize>,
    },
    Context {
        request: Box<ContextRequest>,
        workflow: ContextWorkflow,
        workflow_evidence: WorkflowEvidence,
        max_response_tokens: Option<usize>,
        response_profile: Option<crate::model::ContextResponseProfile>,
    },
    Mcp {
        result_mode: McpResultMode,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum Commands {
    /// Build and atomically publish a complete repository generation.
    Refresh,
    /// Search the published generation.
    Search(SearchArgs),
    /// Outline files in the published generation.
    Outline(OutlineArgs),
    /// Read indexed content from the published generation.
    Read(ReadArgs),
    /// Orchestrate bounded retrieval over the published generation.
    Context(Box<ContextArgs>),
    /// Serve the five-tool MCP projection over stdio.
    Mcp(McpArgs),
}

#[derive(Debug, Clone, Args)]
pub struct McpArgs {
    #[arg(long, value_enum, default_value_t = McpResultMode::Structured)]
    pub result_mode: McpResultMode,
}

mod context;
mod outline;
mod read;
mod retrieval;
mod search;

use context::*;
use outline::*;
use read::*;
use retrieval::*;
use search::SearchArgs;
pub use search::SearchProjectionArg;
