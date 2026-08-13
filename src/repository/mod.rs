use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    io::{BufRead, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, UNIX_EPOCH},
};

use command_group::CommandGroup;
use ignore::WalkBuilder;
use tokio_util::sync::CancellationToken;
use wait_timeout::ChildExt;

use crate::config::DiscoveryLimits;
use crate::error::IndexLimitKind;
use crate::{Error, Result};

#[path = "discovery.rs"]
mod discovery;
#[path = "git/command.rs"]
mod git_command;
#[path = "git/diff.rs"]
mod git_diff;
#[path = "git/models.rs"]
mod git_models;
#[path = "git/objects.rs"]
mod git_objects;
#[path = "git/status.rs"]
mod git_status;
#[path = "path.rs"]
mod path;
#[path = "scope.rs"]
mod scope;

pub(crate) use discovery::*;
pub(crate) use git_command::*;
pub(crate) use git_diff::*;
pub(crate) use git_models::*;
pub(crate) use git_objects::*;
pub(crate) use git_status::*;
pub(crate) use path::*;

pub use discovery::{
    DiscoveredFile, DiscoveryPolicy, DiscoveryResult, DiscoveryStats, discover_files,
    discover_files_cancellable, discover_files_with_limits, discover_files_with_limits_and_policy,
    discover_files_with_limits_cancellable,
};
pub use git_diff::{
    git_diff_hunks, git_diff_hunks_between, git_diff_paths, git_diff_paths_between,
};
pub use git_models::{GitDiffResult, GitHunkRange};
pub use git_status::git_changed_paths;
pub use path::{
    RepositoryPath, RepositoryPattern, RepositoryPatternSet, normalize_relative, resolve_existing,
    slash_path, validate_relative,
};
pub use scope::IndexScope;

#[cfg(all(test, unix))]
mod tests;
