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
    config::DbConfig, params,
};
use rusqlite_migration::{M, Migrations};
use serde::{Deserialize, Serialize};

use crate::model::{
    ReferenceRole, ResponseMeta, TokenAccountingOperation, TokenSavingsRequestClass,
};
use crate::{Error, Result, RetrievalLimitKind};

// These files are physically separated by storage responsibility while
// remaining in one Rust module. Keeping the shared private scope avoids
// widening SQL helpers and transaction internals into crate APIs.
include!("storage/diagnostics.rs");
include!("storage/schema.rs");
include!("storage/models.rs");
include!("storage/writer.rs");
include!("storage/receipts.rs");
include!("storage/query_receipts.rs");
include!("storage/read_delta.rs");
include!("storage/runtime.rs");
include!("storage/open.rs");
include!("storage/publication.rs");
include!("storage/api.rs");
include!("storage/accounting.rs");
include!("storage/session.rs");
include!("storage/mapping.rs");
include!("storage/read/meta.rs");
include!("storage/read/files.rs");
include!("storage/read/imports.rs");
include!("storage/read/syntax.rs");
include!("storage/read/search.rs");
include!("storage/read/counts.rs");
include!("storage/scoped_regex.rs");
include!("storage/helpers.rs");

#[cfg(test)]
mod tests;
