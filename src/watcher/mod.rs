use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use notify::event::{CreateKind, Event, EventKind, ModifyKind, RemoveKind};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::{
    sync::{mpsc, mpsc::error::TrySendError, oneshot},
    task::JoinHandle,
    time::{Instant, interval_at, sleep, timeout},
};
use tokio_util::sync::CancellationToken;

use crate::repository::{DiscoveryPolicy, checked_slash_path};
use crate::{Error, Result};

mod delivery;

use delivery::*;
use discovery::*;
use events::*;
pub use runtime::*;
pub use scheduler::*;
pub use types::*;
mod discovery;
mod events;
mod runtime;
mod scheduler;
mod types;

#[cfg(test)]
mod tests;
