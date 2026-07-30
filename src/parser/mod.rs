use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::Path;

use ::tree_sitter::{
    Language, Node, ParseOptions, Parser, Query, QueryCursor, QueryCursorOptions, QueryMatch,
    StreamingIterator, Tree,
};
use pulldown_cmark::{Event as MarkdownEvent, HeadingLevel, Parser as MarkdownParser, Tag, TagEnd};
use tokio_util::sync::CancellationToken;

use crate::model::{Import, Reference, ReferenceRole, Symbol};
use crate::text::{byte_range_to_line_range, byte_to_line, line_starts};
use crate::{Error, Result};

mod api;
#[path = "languages/csharp.rs"]
mod csharp;
#[path = "languages/css.rs"]
mod css;
mod hierarchy;

pub use api::*;
use csharp::*;
use css::*;
use hierarchy::*;
use html::*;
use imports::*;
use javascript::*;
use latex::*;
use markdown::*;
use queries::*;
use tree_sitter::*;
#[path = "languages/html.rs"]
mod html;
mod imports;
#[path = "languages/javascript.rs"]
mod javascript;
mod latex;
mod markdown;
mod queries;
mod tree_sitter;

#[cfg(test)]
mod tests;
