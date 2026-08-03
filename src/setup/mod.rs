//! Global MCP client registration and removal.

use std::{
    fmt, fs,
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
};

use directories::{BaseDirs, ProjectDirs};
use inquire::{Confirm, InquireError, MultiSelect};
use jsonc_parser::{ParseOptions, cst::CstInputValue, cst::CstRootNode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::NamedTempFile;
use toml_edit::{Array, DocumentMut, Item, Table, value};

use crate::{Error, Result};

#[path = "launcher.rs"]
mod launcher;

use launcher::McpLauncher;

mod apply;
mod client;
#[path = "edit/common.rs"]
mod edit_common;
#[path = "edit/json.rs"]
mod edit_json;
#[path = "edit/toml.rs"]
mod edit_toml;
mod environment;
mod execution;
mod model;
mod output;

use apply::*;
pub use client::*;
use edit_common::*;
use edit_json::*;
use edit_toml::*;
use environment::*;
pub(crate) use environment::{configured_registration, diagnostic_state};
pub use execution::run;
pub use model::*;
pub use output::print_report;
use output::*;
use plan::*;
use prompt::*;
pub use runtime::*;
use transaction::*;
mod plan;
mod prompt;
mod runtime;
mod transaction;

#[cfg(test)]
mod tests;

#[cfg(test)]
use execution::*;
