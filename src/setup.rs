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

#[path = "setup/launcher.rs"]
mod launcher;

use launcher::McpLauncher;

// The setup transaction remains one coordinator. Format-specific editors and
// presentation are physical owners in the same private namespace.
include!("setup/client.rs");
include!("setup/model.rs");
include!("setup/environment.rs");
include!("setup/prompt.rs");
include!("setup/runtime.rs");
include!("setup/plan.rs");
include!("setup/transaction.rs");
include!("setup/edit/common.rs");
include!("setup/edit/json.rs");
include!("setup/edit/toml.rs");
include!("setup/apply.rs");
include!("setup/execution.rs");
include!("setup/output.rs");

#[cfg(test)]
mod tests;
