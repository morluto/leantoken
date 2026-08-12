//! Bounded, read-only Git queries for repository intelligence.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    io::{BufRead, Write},
    path::{Component, Path},
    process::{Command, Stdio},
    time::Duration,
};

use command_group::CommandGroup;
use wait_timeout::ChildExt;

const GIT_PATH_OUTPUT_BYTES_PER_RESULT: usize = 4_096;
const GIT_HUNK_OUTPUT_BYTES_PER_RESULT: usize = 64 * 1024;
const MAX_GIT_DISCOVERY_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Failure from a bounded Git query.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid {field}: {reason}")]
    InvalidInput {
        field: &'static str,
        reason: &'static str,
    },
    #[error("{field} exceeds its configured limit: requested {requested}, limit {limit}")]
    RequestLimitExceeded {
        field: &'static str,
        requested: usize,
        limit: usize,
    },
    #[error("Git operation failed: {0}")]
    OperationFailure(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Result returned by a bounded Git query.
pub type Result<T> = std::result::Result<T, Error>;

mod command;
mod diff;
mod models;
mod objects;
mod status;

pub use command::*;
pub use diff::*;
pub use models::*;
pub use objects::*;
pub use status::*;

#[cfg(all(test, unix))]
mod tests;
