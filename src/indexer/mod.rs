use std::collections::{BTreeMap, HashMap, HashSet};
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
use crate::model::{
    IndexProgressPhase, IndexProgressSnapshot, IndexReport, IndexResponse, IndexSkipReasonCounts,
};
use crate::parser::{self, ParseOutput};
use crate::repository::{
    DiscoveredFile, discover_files_with_limits_policy_and_filter,
    discover_files_with_limits_policy_filter_and_progress, enforce_limit, slash_path,
    validate_relative,
};
use crate::storage::{
    ChunkInput, ImportInput, ImportProjection, IndexedFile, PublicationDiagnostics,
    ReconciliationPublicationPhase, ReconciliationWriter, ReferenceInput, Storage, SymbolInput,
    process_write_bytes,
};
use crate::text::{PreparedText, TextKind, hash_bytes};
use crate::{Config, Error, Result};

mod import_resolution;

use import_resolution::*;
#[cfg(test)]
#[allow(unused_imports)]
use orchestrator::observe_publication_phase;
use prepare::*;
pub(crate) use progress::index_progress_cache_namespace;
use progress::*;
use publish::*;
pub use types::*;
mod imports;
mod incremental;
mod orchestrator;
mod plan;
mod preparation;
mod prepare;
mod progress;
mod publish;
mod types;

#[cfg(test)]
mod tests;
