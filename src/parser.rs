use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::Path;

use pulldown_cmark::{Event as MarkdownEvent, HeadingLevel, Parser as MarkdownParser, Tag, TagEnd};
use tokio_util::sync::CancellationToken;
use tree_sitter::{
    Language, Node, ParseOptions, Parser, Query, QueryCursor, QueryCursorOptions, QueryMatch,
    StreamingIterator, Tree,
};

use crate::model::{Import, Reference, ReferenceRole, Symbol};
use crate::text::{byte_range_to_line_range, byte_to_line, line_starts};
use crate::{Error, Result};

// These files share the parser's private caches and tree-sitter node helpers;
// physical owners keep those zero-cost concrete types without trait indirection.
include!("parser/queries.rs");
include!("parser/api.rs");
include!("parser/markdown.rs");
include!("parser/latex.rs");
include!("parser/tree_sitter.rs");
include!("parser/languages/javascript.rs");
include!("parser/languages/csharp.rs");
include!("parser/languages/css.rs");
include!("parser/languages/html.rs");
include!("parser/languages/swift.rs");
include!("parser/imports.rs");
include!("parser/hierarchy.rs");

#[cfg(test)]
mod tests;
