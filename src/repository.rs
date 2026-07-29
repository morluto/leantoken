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

// Git owners intentionally share the bounded command runner and concrete
// result types; no VCS abstraction or extra subprocess path is introduced.
include!("repository/scope.rs");
include!("repository/discovery.rs");
include!("repository/path.rs");
include!("repository/git/models.rs");
include!("repository/git/command.rs");
include!("repository/git/status.rs");
include!("repository/git/objects.rs");
include!("repository/git/diff.rs");

#[cfg(all(test, unix))]
mod tests;
