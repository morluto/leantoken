use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
#[cfg(test)]
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior,
    config::DbConfig,
};
use rusqlite_migration::{M, Migrations};
use serde::{Deserialize, Serialize};

use crate::model::{
    IndexingMode, ReferenceRole, ResponseMeta, TokenAccountingOperation, TokenSavingsRequestClass,
};
use crate::{Error, Result, RetrievalLimitKind};

mod accounting;
mod api;
mod diagnostics;
mod helpers;
mod mapping;
mod models;
mod open;
mod publication;
mod query_receipts;
#[path = "read/counts.rs"]
mod read_counts;
mod read_delta;
#[path = "read/files.rs"]
mod read_files;
#[path = "read/imports.rs"]
mod read_imports;
#[path = "read/meta.rs"]
mod read_meta;
#[path = "read/search.rs"]
mod read_search;
#[path = "read/syntax.rs"]
mod read_syntax;
mod receipts;
mod runtime;
mod schema;
mod scoped_regex;
mod session;
mod snapshot;
mod staging;
mod writer;

pub(crate) use diagnostics::*;
pub(crate) use helpers::*;
pub(crate) use models::*;
pub(crate) use receipts::*;
pub(crate) use runtime::*;
pub(crate) use rusqlite::params;
pub(crate) use schema::*;
pub(crate) use scoped_regex::*;
pub(crate) use snapshot::RepositoryGeneration;
pub(crate) use staging::PreparedReconciliation;

pub use diagnostics::{
    DEFAULT_MAX_RESULTS, FtsStorageFootprint, HARD_MAX_RESULTS, PublicationDiagnostics,
};
pub use models::{
    ChunkHit, ChunkInput, ChunkRecord, FileRecord, ImportInput, ImportRecord, IndexedFile,
    MetaRecord, ReferenceInput, ReferenceRecord, Storage, StorageCounts, SymbolHit, SymbolInput,
    SymbolRecord,
};
pub use runtime::ReadSession;

#[cfg(test)]
mod tests;
