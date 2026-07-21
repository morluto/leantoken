use std::cell::RefCell;
use std::ops::ControlFlow;
use std::path::Path;

use tokio_util::sync::CancellationToken;
use tree_sitter::{
    Language, Node, ParseOptions, Parser, Query, QueryCursor, QueryCursorOptions, QueryMatch,
    StreamingIterator, Tree,
};

use crate::model::{Import, Reference, ReferenceRole, Symbol};
use crate::{Error, Result};

const RUST_DEFS_QUERY: &str = r#"
(const_item
  name: (identifier) @name) @definition.constant

(static_item
  name: (identifier) @name) @definition.static
"#;

const GO_DEFS_QUERY: &str = r#"
(package_clause "package" (package_identifier) @name) @definition.module

(var_declaration (var_spec name: (identifier) @name)) @definition.variable

(const_declaration (const_spec name: (identifier) @name)) @definition.constant
"#;

const PHP_REFS_QUERY: &str = r#"
(function_call_expression
  function: (name) @name) @reference.call
"#;

const RUST_IMPORT_QUERY: &str = r#"
(use_declaration
  argument: (_) @raw) @import
"#;

const PYTHON_IMPORT_QUERY: &str = r#"
(import_statement
  name: (_) @raw) @import

(import_from_statement
  module_name: (_) @raw) @import

(import_from_statement
  name: (_) @raw) @import
"#;

const JS_IMPORT_QUERY: &str = r#"
(import_statement
  source: (string) @raw) @import

(export_statement
  source: (string) @raw) @import

(call_expression
  function: (identifier) @fn
  arguments: (arguments (string) @raw)
  (#eq? @fn "require")) @import
"#;

const GO_IMPORT_QUERY: &str = r#"
(import_spec
  path: (interpreted_string_literal) @raw) @import

(import_spec
  path: (raw_string_literal) @raw) @import
"#;

#[derive(Debug, Clone)]
pub struct ParseOutput {
    pub language: Option<String>,
    pub structurally_complete: bool,
    pub symbols: Vec<Symbol>,
    pub references: Vec<Reference>,
    pub imports: Vec<Import>,
}

/// Detect the parser language from a file path based on its extension.
pub fn language_by_path(path: impl AsRef<Path>) -> Option<String> {
    let ext = path.as_ref().extension()?.to_str()?;
    Some(match ext.to_lowercase().as_str() {
        "c" => "c".to_string(),
        "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" | "inl" | "ipp" | "tpp" => {
            "cpp".to_string()
        }
        "java" => "java".to_string(),
        "rs" => "rust".to_string(),
        "py" | "pyi" => "python".to_string(),
        "php" => "php".to_string(),
        "rb" => "ruby".to_string(),
        "js" | "jsx" | "mjs" | "cjs" => "javascript".to_string(),
        "ts" | "mts" | "cts" => "typescript".to_string(),
        "tsx" => "tsx".to_string(),
        "go" => "go".to_string(),
        _ => return None,
    })
}

/// Parse a source file given its repository path and full text.
///
/// Files whose language is not supported still return `Ok`, but with an empty
/// parse and `language: None` so callers can fall back to plain text indexing.
pub fn parse(path: impl AsRef<Path>, source: &str) -> Result<ParseOutput> {
    parse_with_cancellation(path, source, &CancellationToken::new())
}

pub(crate) fn parse_with_cancellation(
    path: impl AsRef<Path>,
    source: &str,
    cancellation: &CancellationToken,
) -> Result<ParseOutput> {
    match language_by_path(path) {
        Some(lang) => {
            parse_language_with_cancellation(&lang, source, || cancellation.is_cancelled())
        }
        None if cancellation.is_cancelled() => Err(Error::Cancelled),
        None => Ok(empty_parse()),
    }
}

/// Parse source text for a known language name.
pub fn parse_language(language: &str, source: &str) -> Result<ParseOutput> {
    parse_language_with_cancellation(language, source, || false)
}

// Per-thread cache of a configured tree-sitter `Parser` and compiled query
// objects, keyed by language name. `Parser` and `Query` are not `Send`, so
// they cannot be shared across the rayon worker pool. A thread-local avoids
// recreating and recompiling them for every source file parsed on the same
// thread.
thread_local! {
    static PARSER_CACHE: RefCell<Option<ParserCache>> = const { RefCell::new(None) };
}

struct ParserCache {
    language: String,
    parser: Parser,
    tags_query: Query,
    import_query: Option<Query>,
}

fn parse_language_with_cancellation(
    language: &str,
    source: &str,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<ParseOutput> {
    let lang = language_object(language)
        .ok_or_else(|| Error::UnsupportedLanguage(language.to_string()))?;

    PARSER_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        let needs_init = match &*cache {
            Some(c) => c.language != language,
            None => true,
        };
        if needs_init {
            let mut parser = Parser::new();
            parser
                .set_language(&lang)
                .map_err(Error::TreeSitterLanguage)?;
            let tags_query = build_tags_query(language, &lang)?;
            let import_query = build_import_query(language, &lang)?;
            *cache = Some(ParserCache {
                language: language.to_string(),
                parser,
                tags_query,
                import_query,
            });
        }
        let cache = cache.as_mut().expect("cache was just initialized");

        let tree = parse_tree(&mut cache.parser, source, &mut is_cancelled)?;
        let root = tree.root_node();
        let structurally_complete = !root.has_error();

        let mut symbols = Vec::new();
        let mut references = Vec::new();
        let mut imports = Vec::new();

        run_query(source, &cache.tags_query, root, &mut is_cancelled, |qm| {
            process_tags_match(source, &cache.tags_query, qm, &mut symbols, &mut references);
        })?;
        if let Some(import_query) = &cache.import_query {
            run_query(source, import_query, root, &mut is_cancelled, |qm| {
                process_imports_match(source, import_query, qm, &mut imports);
            })?;
        }

        if is_cancelled() {
            return Err(Error::Cancelled);
        }

        compute_symbol_parents(&mut symbols);
        compute_reference_enclosing(&symbols, &mut references);

        Ok(ParseOutput {
            language: Some(language.to_string()),
            structurally_complete,
            symbols,
            references,
            imports,
        })
    })
}

fn empty_parse() -> ParseOutput {
    ParseOutput {
        language: None,
        structurally_complete: false,
        symbols: Vec::new(),
        references: Vec::new(),
        imports: Vec::new(),
    }
}

fn parse_tree(
    parser: &mut Parser,
    source: &str,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<Tree> {
    if is_cancelled() {
        return Err(Error::Cancelled);
    }

    let bytes = source.as_bytes();
    let mut input = |offset: usize, _| bytes.get(offset..).unwrap_or_default();
    let tree = {
        let mut progress = |_: &tree_sitter::ParseState| {
            if is_cancelled() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let options = ParseOptions::new().progress_callback(&mut progress);
        parser.parse_with_options(&mut input, None, Some(options))
    };

    match tree {
        Some(tree) => Ok(tree),
        None if is_cancelled() => Err(Error::Cancelled),
        None => Err(Error::InvalidRequest("parser returned None".into())),
    }
}

fn language_object(name: &str) -> Option<Language> {
    Some(match name {
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        _ => return None,
    })
}

fn build_tags_query(language: &str, lang: &Language) -> Result<Query> {
    let base = match language {
        "c" => tree_sitter_c::TAGS_QUERY,
        "cpp" => tree_sitter_cpp::TAGS_QUERY,
        "java" => tree_sitter_java::TAGS_QUERY,
        "rust" => tree_sitter_rust::TAGS_QUERY,
        "python" => tree_sitter_python::TAGS_QUERY,
        "php" => tree_sitter_php::TAGS_QUERY,
        "ruby" => tree_sitter_ruby::TAGS_QUERY,
        "javascript" => tree_sitter_javascript::TAGS_QUERY,
        // The TypeScript crate's query contains only TypeScript-specific
        // additions. Its grammar inherits JavaScript definitions, so both
        // query sets are required.
        "typescript" | "tsx" => tree_sitter_javascript::TAGS_QUERY,
        "go" => tree_sitter_go::TAGS_QUERY,
        _ => return Err(Error::UnsupportedLanguage(language.to_string())),
    };

    let mut source = base.to_string();
    match language {
        "rust" => source.push_str(RUST_DEFS_QUERY),
        "go" => source.push_str(GO_DEFS_QUERY),
        "php" => source.push_str(PHP_REFS_QUERY),
        "typescript" | "tsx" => source.push_str(tree_sitter_typescript::TAGS_QUERY),
        _ => {}
    }

    Query::new(lang, &source).map_err(Error::TreeSitterQuery)
}

fn build_import_query(language: &str, lang: &Language) -> Result<Option<Query>> {
    let src = match language {
        "rust" => RUST_IMPORT_QUERY,
        "python" => PYTHON_IMPORT_QUERY,
        "javascript" | "typescript" | "tsx" => JS_IMPORT_QUERY,
        "go" => GO_IMPORT_QUERY,
        "c" | "cpp" | "java" | "php" | "ruby" => return Ok(None),
        _ => return Err(Error::UnsupportedLanguage(language.to_string())),
    };

    Query::new(lang, src)
        .map(Some)
        .map_err(Error::TreeSitterQuery)
}

fn run_query<F>(
    source: &str,
    query: &Query,
    root: Node,
    is_cancelled: &mut impl FnMut() -> bool,
    mut f: F,
) -> Result<()>
where
    F: FnMut(&QueryMatch),
{
    if is_cancelled() {
        return Err(Error::Cancelled);
    }

    let mut cursor = QueryCursor::new();
    {
        let mut progress = |_: &tree_sitter::QueryCursorState| {
            if is_cancelled() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let options = QueryCursorOptions::new().progress_callback(&mut progress);
        let mut matches = cursor.matches_with_options(query, root, source.as_bytes(), options);
        while let Some(qm) = matches.next() {
            f(qm);
        }
    }

    if is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn process_tags_match(
    source: &str,
    query: &Query,
    qm: &QueryMatch,
    symbols: &mut Vec<Symbol>,
    references: &mut Vec<Reference>,
) {
    let capture_names = query.capture_names();
    let mut name_node: Option<Node> = None;
    let mut kind_captures = Vec::new();

    for cap in qm.captures {
        let cap_name = capture_names[cap.index as usize];
        if cap_name == "name" {
            name_node = Some(cap.node);
        } else if let Some(prefix) = cap_name.strip_prefix("definition.") {
            kind_captures.push((true, prefix, cap.node));
        } else if let Some(prefix) = cap_name.strip_prefix("reference.") {
            kind_captures.push((false, prefix, cap.node));
        }
    }

    let Some(name_node) = name_node else {
        return;
    };

    let name = node_text(source, name_node);

    for (is_definition, kind, kind_node) in kind_captures {
        let kind = kind.to_string();
        if is_definition {
            let kind_node = definition_extent(kind_node);
            let (start_line, end_line, start_byte, end_byte) = range_from_node(kind_node);
            symbols.push(Symbol {
                name: name.clone(),
                kind,
                parent: None,
                signature: signature_from_node(source, kind_node),
                start_line,
                end_line,
                start_byte,
                end_byte,
            });
        } else {
            let (start_line, end_line, start_byte, end_byte) = range_from_node(name_node);
            references.push(Reference {
                name: name.clone(),
                kind,
                role: ReferenceRole::Reference,
                enclosing_symbol: None,
                start_line,
                end_line,
                start_byte,
                end_byte,
            });
        }
    }
}

fn definition_extent(node: Node<'_>) -> Node<'_> {
    if node.kind() != "function_declarator" {
        return node;
    }
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "function_definition" {
            return parent;
        }
        if matches!(parent.kind(), "declaration" | "translation_unit") {
            break;
        }
        current = parent;
    }
    node
}

fn signature_from_node(source: &str, node: Node) -> Option<String> {
    const MAX_SIGNATURE_CHARS: usize = 512;

    let end = first_body_start(node).unwrap_or_else(|| node.end_byte());
    let raw = source.get(node.start_byte()..end)?.trim();
    if raw.is_empty() {
        return None;
    }

    let mut compact = String::with_capacity(raw.len().min(MAX_SIGNATURE_CHARS));
    for part in raw.split_whitespace() {
        if !compact.is_empty() {
            compact.push(' ');
        }
        let remaining = MAX_SIGNATURE_CHARS.saturating_sub(compact.chars().count());
        if remaining == 0 {
            break;
        }
        compact.extend(part.chars().take(remaining));
    }
    (!compact.is_empty()).then_some(compact)
}

fn first_body_start(node: Node<'_>) -> Option<usize> {
    let mut pending = vec![node];
    let mut earliest = None;
    while let Some(current) = pending.pop() {
        if let Some(body) = current.child_by_field_name("body") {
            earliest = Some(earliest.map_or(body.start_byte(), |start: usize| {
                start.min(body.start_byte())
            }));
            continue;
        }
        let mut cursor = current.walk();
        pending.extend(current.named_children(&mut cursor));
    }
    earliest
}

fn process_imports_match(source: &str, query: &Query, qm: &QueryMatch, imports: &mut Vec<Import>) {
    let capture_names = query.capture_names();
    for cap in qm.captures {
        let cap_name = capture_names[cap.index as usize];
        if cap_name != "raw" {
            continue;
        }
        let raw = node_text(source, cap.node);
        let raw = unquote(&raw);
        if !raw.is_empty() {
            let line = cap.node.start_position().row + 1;
            imports.push(Import {
                raw_target: raw.to_string(),
                resolved_path: None,
                line,
            });
        }
    }
}

fn compute_symbol_parents(symbols: &mut [Symbol]) {
    if symbols.is_empty() {
        return;
    }

    let mut indices: Vec<usize> = (0..symbols.len()).collect();
    indices.sort_by(|&a, &b| {
        symbols[a]
            .start_byte
            .cmp(&symbols[b].start_byte)
            .then_with(|| symbols[b].end_byte.cmp(&symbols[a].end_byte))
    });

    let mut stack: Vec<usize> = Vec::new();
    for i in indices {
        while let Some(&top) = stack.last() {
            if symbols[top].end_byte <= symbols[i].start_byte {
                stack.pop();
            } else {
                break;
            }
        }
        symbols[i].parent = stack.last().map(|&top| symbols[top].name.clone());
        stack.push(i);
    }

    symbols.sort_by(|a, b| {
        a.start_byte
            .cmp(&b.start_byte)
            .then_with(|| a.end_byte.cmp(&b.end_byte))
    });
}

fn compute_reference_enclosing(symbols: &[Symbol], references: &mut [Reference]) {
    if symbols.is_empty() || references.is_empty() {
        references.sort_by(|a, b| {
            a.start_byte
                .cmp(&b.start_byte)
                .then_with(|| a.end_byte.cmp(&b.end_byte))
        });
        return;
    }

    let mut sym_indices: Vec<usize> = (0..symbols.len()).collect();
    sym_indices.sort_by(|&a, &b| {
        symbols[a]
            .start_byte
            .cmp(&symbols[b].start_byte)
            .then_with(|| symbols[b].end_byte.cmp(&symbols[a].end_byte))
    });

    let mut ref_indices: Vec<usize> = (0..references.len()).collect();
    ref_indices.sort_by_key(|&i| references[i].start_byte);

    let mut sym_idx = 0;
    let mut stack: Vec<usize> = Vec::new();

    for ri in ref_indices {
        let ref_start = references[ri].start_byte;
        let ref_end = references[ri].end_byte;

        while let Some(&top) = stack.last() {
            if symbols[top].end_byte <= ref_start {
                stack.pop();
            } else {
                break;
            }
        }

        while sym_idx < sym_indices.len() {
            let si = sym_indices[sym_idx];
            if symbols[si].start_byte > ref_start {
                break;
            }
            if symbols[si].end_byte > ref_start {
                stack.push(si);
            }
            sym_idx += 1;
        }

        while let Some(&top) = stack.last() {
            if symbols[top].end_byte < ref_end {
                stack.pop();
            } else {
                break;
            }
        }

        if let Some(&top) = stack.last() {
            references[ri].enclosing_symbol = Some(symbols[top].name.clone());
        }
    }

    references.sort_by(|a, b| {
        a.start_byte
            .cmp(&b.start_byte)
            .then_with(|| a.end_byte.cmp(&b.end_byte))
    });
}

fn range_from_node(node: Node) -> (usize, usize, usize, usize) {
    let start = node.start_position();
    let end = node.end_position();
    (
        start.row + 1,
        end.row + 1,
        node.start_byte(),
        node.end_byte(),
    )
}

fn node_text(source: &str, node: Node) -> String {
    let bytes = source.as_bytes();
    let start = node.start_byte();
    let end = node.end_byte();
    if start <= end && end <= bytes.len() {
        String::from_utf8_lossy(&bytes[start..end]).into_owned()
    } else {
        String::new()
    }
}

fn unquote(s: &str) -> &str {
    let mut chars = s.chars();
    if let (Some(first), Some(last)) = (chars.next(), chars.next_back())
        && first == last
        && (first == '"' || first == '\'' || first == '`')
    {
        return chars.as_str();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_SRC: &str = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn distance(&self, other: &Point) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}
"#;

    const PYTHON_SRC: &str = r#"
import os
from collections import defaultdict

class Greeter:
    def __init__(self, name):
        self.name = name

    def greet(self):
        print(f"Hello, {self.name}")
"#;

    const JS_SRC: &str = r#"
import { helper } from "./helper.js";
import * as utils from "./utils";

function greet(name) {
    console.log(helper(name));
}

app.render = function render(name) {
    helper(name);
};

const x = 1;
"#;

    const TS_SRC: &str = r#"
import { Point } from "./point";

export class Box {
    constructor(private p: Point) {}
    area(): number {
        return this.p.x * this.p.y;
    }
}
"#;

    const GO_SRC: &str = r#"
package main

import (
    "fmt"
    "strings"
)

type Point struct {
    X, Y float64
}

func (p Point) Distance(other Point) float64 {
    dx := p.X - other.X
    dy := p.Y - other.Y
    return (dx*dx + dy*dy)
}

func main() {
    p := Point{X: 1, Y: 2}
    fmt.Println(p.Distance(Point{X: 0, Y: 0}))
}
"#;

    const C_SRC: &str = r#"
struct Point { int x; };

int add(int left, int right) {
    return left + right;
}
"#;

    const CPP_SRC: &str = r#"
class Formatter {
public:
    int format() { return helper(); }
};
"#;

    const JAVA_SRC: &str = r#"
class Formatter {
    int format() {
        return helper();
    }
}
"#;

    const PHP_SRC: &str = r#"<?php
class Formatter {
    public function format() {
        return helper();
    }
}
"#;

    const RUBY_SRC: &str = r#"
class Formatter
  def format
    helper
  end
end
"#;

    fn symbol_names(output: &ParseOutput) -> Vec<&str> {
        output.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    fn reference_names(output: &ParseOutput) -> Vec<&str> {
        output.references.iter().map(|r| r.name.as_str()).collect()
    }

    fn import_targets(output: &ParseOutput) -> Vec<&str> {
        output
            .imports
            .iter()
            .map(|i| i.raw_target.as_str())
            .collect()
    }

    #[test]
    fn tree_sitter_progress_callback_interrupts_parsing() {
        let source = (0..20_000)
            .map(|index| format!("fn item_{index}() {{ let value = {index}; }}\n"))
            .collect::<String>();
        let language = language_object("rust").expect("rust language");
        let mut parser = Parser::new();
        parser.set_language(&language).expect("set language");
        let mut checks = 0usize;

        let error = parse_tree(&mut parser, &source, &mut || {
            checks += 1;
            checks > 1
        })
        .expect_err("progress callback should cancel parsing");

        assert!(matches!(error, Error::Cancelled));
        assert!(checks > 1, "tree-sitter never polled parse progress");
    }

    #[test]
    fn unknown_language_returns_empty_parse() -> Result<()> {
        let out = parse("data/config.json", "{}")?;
        assert_eq!(out.language, None);
        assert!(out.symbols.is_empty());
        assert!(out.references.is_empty());
        assert!(out.imports.is_empty());
        assert!(!out.structurally_complete);
        Ok(())
    }

    #[test]
    fn development_languages_are_detected_by_path() {
        for (path, expected) in [
            ("src/value.c", "c"),
            ("include/value.h", "cpp"),
            ("src/value.cpp", "cpp"),
            ("include/value.hpp", "cpp"),
            ("src/Value.java", "java"),
            ("src/value.php", "php"),
            ("lib/value.rb", "ruby"),
        ] {
            assert_eq!(language_by_path(path).as_deref(), Some(expected), "{path}");
        }
    }

    #[test]
    fn c_and_cpp_definitions_keep_function_bodies() -> Result<()> {
        let c = parse_language("c", C_SRC)?;
        assert!(c.structurally_complete);
        assert!(symbol_names(&c).contains(&"Point"));
        let add = c
            .symbols
            .iter()
            .find(|symbol| symbol.name == "add")
            .expect("C function");
        assert!(add.end_line > add.start_line, "symbol: {add:?}");

        let c_header_as_cpp = parse_language("cpp", C_SRC)?;
        assert!(symbol_names(&c_header_as_cpp).contains(&"add"));

        let cpp = parse_language("cpp", CPP_SRC)?;
        assert!(cpp.structurally_complete);
        assert!(symbol_names(&cpp).contains(&"Formatter"));
        let format = cpp
            .symbols
            .iter()
            .find(|symbol| symbol.name == "format")
            .expect("C++ method");
        assert!(
            format
                .signature
                .as_deref()
                .is_some_and(|value| value.contains("format"))
        );
        Ok(())
    }

    #[test]
    fn java_php_and_ruby_parse_definitions_and_calls() -> Result<()> {
        for (language, source) in [("java", JAVA_SRC), ("php", PHP_SRC), ("ruby", RUBY_SRC)] {
            let output = parse_language(language, source)?;
            assert!(output.structurally_complete, "{language}");
            let names = symbol_names(&output);
            assert!(
                names.contains(&"Formatter"),
                "{language} symbols: {names:?}"
            );
            assert!(names.contains(&"format"), "{language} symbols: {names:?}");
            let references = reference_names(&output);
            assert!(
                references.contains(&"helper"),
                "{language} references: {references:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn rust_parses_definitions_references_and_parent() -> Result<()> {
        let out = parse_language("rust", RUST_SRC)?;
        assert_eq!(out.language.as_deref(), Some("rust"));
        assert!(out.structurally_complete);

        let names = symbol_names(&out);
        assert!(names.contains(&"add"), "symbols: {names:?}");
        assert!(names.contains(&"Point"), "symbols: {names:?}");
        assert!(names.contains(&"distance"), "symbols: {names:?}");

        // `Point` is defined as a struct and referenced in `impl Point`.
        let refs = reference_names(&out);
        assert!(refs.contains(&"Point"), "references: {refs:?}");
        assert!(refs.contains(&"sqrt"), "references: {refs:?}");

        // Struct fields should be parented to the struct.
        let point = out.symbols.iter().find(|s| s.name == "Point").unwrap();
        assert_eq!(point.kind, "class");
        Ok(())
    }

    #[test]
    fn python_parses_class_function_imports() -> Result<()> {
        let out = parse_language("python", PYTHON_SRC)?;
        assert_eq!(out.language.as_deref(), Some("python"));
        assert!(out.structurally_complete);

        let names = symbol_names(&out);
        assert!(names.contains(&"Greeter"), "symbols: {names:?}");
        assert!(names.contains(&"__init__"), "symbols: {names:?}");
        assert!(names.contains(&"greet"), "symbols: {names:?}");

        let refs = reference_names(&out);
        assert!(refs.contains(&"print"), "references: {refs:?}");

        let imports = import_targets(&out);
        assert!(imports.contains(&"os"), "imports: {imports:?}");
        assert!(imports.contains(&"collections"), "imports: {imports:?}");

        let init = out.symbols.iter().find(|s| s.name == "__init__").unwrap();
        assert_eq!(init.parent.as_deref(), Some("Greeter"));
        assert!(
            init.signature
                .as_deref()
                .is_some_and(|value| value.starts_with("def __init__"))
        );
        Ok(())
    }

    #[test]
    fn javascript_parses_imports_and_calls() -> Result<()> {
        let out = parse_language("javascript", JS_SRC)?;
        assert_eq!(out.language.as_deref(), Some("javascript"));
        assert!(out.structurally_complete);

        let names = symbol_names(&out);
        assert!(names.contains(&"greet"), "symbols: {names:?}");

        let refs = reference_names(&out);
        assert!(refs.contains(&"log"), "references: {refs:?}");
        assert!(refs.contains(&"helper"), "references: {refs:?}");

        let imports = import_targets(&out);
        assert!(imports.contains(&"./helper.js"), "imports: {imports:?}");
        assert!(imports.contains(&"./utils"), "imports: {imports:?}");
        let render = out
            .symbols
            .iter()
            .find(|symbol| {
                symbol.name == "render"
                    && symbol
                        .signature
                        .as_deref()
                        .is_some_and(|signature| signature.starts_with("app.render"))
            })
            .expect("assigned render symbol");
        assert_eq!(
            render.signature.as_deref(),
            Some("app.render = function render(name)")
        );
        Ok(())
    }

    #[test]
    fn typescript_parses_class_and_type_references() -> Result<()> {
        let out = parse_language("typescript", TS_SRC)?;
        assert_eq!(out.language.as_deref(), Some("typescript"));
        assert!(out.structurally_complete);

        let names = symbol_names(&out);
        assert!(names.contains(&"Box"), "symbols: {names:?}");
        assert!(names.contains(&"area"), "symbols: {names:?}");
        assert!(
            out.symbols
                .iter()
                .find(|symbol| symbol.name == "area")
                .and_then(|symbol| symbol.signature.as_deref())
                .is_some_and(|signature| signature.contains("area"))
        );

        let refs = reference_names(&out);
        assert!(refs.contains(&"Point"), "references: {refs:?}");

        let imports = import_targets(&out);
        assert!(imports.contains(&"./point"), "imports: {imports:?}");
        Ok(())
    }

    #[test]
    fn go_parses_package_types_methods_and_imports() -> Result<()> {
        let out = parse_language("go", GO_SRC)?;
        assert_eq!(out.language.as_deref(), Some("go"));
        assert!(out.structurally_complete);

        let names = symbol_names(&out);
        assert!(names.contains(&"Point"), "symbols: {names:?}");
        assert!(names.contains(&"Distance"), "symbols: {names:?}");
        assert!(names.contains(&"main"), "symbols: {names:?}");

        let refs = reference_names(&out);
        assert!(refs.contains(&"Println"), "references: {refs:?}");

        let imports = import_targets(&out);
        assert!(imports.contains(&"fmt"), "imports: {imports:?}");
        assert!(imports.contains(&"strings"), "imports: {imports:?}");
        Ok(())
    }

    #[test]
    fn malformed_source_is_marked_incomplete() -> Result<()> {
        let out = parse_language("rust", "fn broken(")?;
        assert!(!out.structurally_complete);
        Ok(())
    }
}
