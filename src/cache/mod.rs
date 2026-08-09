//! Explicit inspection and pruning of centrally managed repository caches.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    io::Write,
    num::NonZeroU64,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use crate::config::{
    INDEX_CONTENT_VERSION, ManagedCacheIdentity, managed_cache_identity_matches_root,
    managed_cache_root, parse_managed_cache_id,
};
use crate::coordination::{
    DEFAULT_INDEX_DATABASE_NAME, IndexCoordination, LEASE_LOCK_SUFFIX, coordination_sidecar_path,
    is_coordination_sidecar_for_database,
};
use crate::model::IndexScopeMode;
use crate::mutation::MutationMode;
use crate::storage::{CURRENT_MIGRATION_VERSION, CURRENT_SCHEMA_VERSION};
use crate::{Error, Result};

mod api;
mod artifacts;
mod cursor;
mod inspection;
mod list;
mod models;
mod output;

pub use api::*;
use artifacts::*;
use cursor::*;
pub use models::*;
pub use output::*;
use prune_policy::*;
mod prune;
mod prune_policy;

#[cfg(test)]
mod tests;
