use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use cap_std::fs::Dir;
use rayon::ThreadPool;
use rayon::prelude::*;
use tokio_util::sync::CancellationToken;

use crate::config::INDEX_CONTENT_VERSION;
use crate::error::RetryableOperation;
use crate::model::{IndexReport, IndexResponse, IndexSkipReasonCounts};
use crate::parser::{self, ParseOutput};
use crate::repository::{
    DiscoveredFile, discover_files_with_limits_policy_and_filter, enforce_limit, slash_path,
    validate_relative,
};
use crate::storage::{
    ChunkInput, ImportInput, ImportProjection, IndexedFile, PublicationDiagnostics,
    ReconciliationWriter, ReferenceInput, Storage, SymbolInput, process_write_bytes,
};
use crate::text::{PreparedText, TextKind, hash_bytes};
use crate::{Config, Error, Result};

// Full and incremental reconciliation retain their existing publication
// semantics; these physical owners share one concrete Indexer implementation.
include!("indexer/types.rs");
include!("indexer/orchestrator.rs");
include!("indexer/incremental.rs");
include!("indexer/preparation.rs");
include!("indexer/imports.rs");
include!("indexer/plan.rs");
include!("indexer/publish.rs");
include!("indexer/prepare.rs");
include!("indexer/import_resolution.rs");

#[cfg(test)]
mod tests;
