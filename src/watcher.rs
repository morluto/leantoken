use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use notify::event::{CreateKind, Event, EventKind, ModifyKind, RemoveKind, RenameMode};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::{
    sync::{mpsc, mpsc::error::TrySendError, oneshot},
    task::JoinHandle,
    time::{Instant, interval_at, sleep, timeout},
};
use tokio_util::sync::CancellationToken;

use crate::repository::{DiscoveryPolicy, checked_slash_path};
use crate::{Error, Result};

// The scheduler is a pure state machine; backend lifecycle, event
// normalization, and bounded delivery remain separate physical owners.
include!("watcher/types.rs");
include!("watcher/scheduler.rs");
include!("watcher/runtime.rs");
include!("watcher/discovery.rs");
include!("watcher/events.rs");
include!("watcher/delivery.rs");

#[cfg(test)]
mod tests;
