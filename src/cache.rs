//! Explicit inspection and pruning of centrally managed repository caches.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use crate::config::{
    INDEX_CONTENT_VERSION, ManagedCacheIdentity, managed_cache_id_matches_root, managed_cache_root,
    parse_managed_cache_id,
};
use crate::coordination::{
    COORDINATION_LOCK_SUFFIXES, DEFAULT_INDEX_DATABASE_NAME, IndexCoordination, LEASE_LOCK_SUFFIX,
    coordination_sidecar_path, is_coordination_sidecar_for_database,
};
use crate::storage::{CURRENT_MIGRATION_VERSION, CURRENT_SCHEMA_VERSION};
use crate::{Error, Result};

// Cache policy, inspection, mutation, and adapter rendering retain the
// existing public facade while living under distinct physical owners.
include!("cache/models.rs");
include!("cache/api.rs");
include!("cache/list.rs");
include!("cache/prune.rs");
include!("cache/inspection.rs");
include!("cache/artifacts.rs");
include!("cache/cursor.rs");
include!("cache/prune_policy.rs");
include!("cache/output.rs");

#[cfg(test)]
mod tests;
