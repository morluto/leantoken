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
use crate::text::byte_range_to_line_range;
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
  module_name: (_) @python_module
  name: (_) @python_member) @import

(import_from_statement
  module_name: (_) @python_module
  (wildcard_import) @python_wildcard) @import
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
        "cs" => "csharp".to_string(),
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
        "html" | "htm" => "html".to_string(),
        "css" => "css".to_string(),
        "md" | "markdown" => "markdown".to_string(),
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
    parser: Parser,
    language: Option<String>,
    queries: HashMap<String, CompiledQueries>,
}

struct CompiledQueries {
    tags_query: Option<Query>,
    import_query: Option<Query>,
}

fn parse_language_with_cancellation(
    language: &str,
    source: &str,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<ParseOutput> {
    if language == "markdown" {
        return parse_markdown(source, &mut is_cancelled);
    }
    let lang = language_object(language)
        .ok_or_else(|| Error::UnsupportedLanguage(language.to_string()))?;

    PARSER_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        let cache = cache.get_or_insert_with(|| ParserCache {
            parser: Parser::new(),
            language: None,
            queries: HashMap::new(),
        });
        if !cache.queries.contains_key(language) {
            let tags_query = build_tags_query(language, &lang)?;
            let import_query = build_import_query(language, &lang)?;
            cache.queries.insert(
                language.to_string(),
                CompiledQueries {
                    tags_query,
                    import_query,
                },
            );
        }
        if cache.language.as_deref() != Some(language) {
            cache
                .parser
                .set_language(&lang)
                .map_err(Error::TreeSitterLanguage)?;
            cache.language = Some(language.to_string());
        }

        let tree = parse_tree(&mut cache.parser, source, &mut is_cancelled)?;
        let root = tree.root_node();
        let structurally_complete = !root.has_error();

        let queries = cache
            .queries
            .get(language)
            .expect("queries were just initialized");
        let mut symbols = Vec::new();
        let mut references = Vec::new();
        let mut imports = Vec::new();

        if let Some(tags_query) = &queries.tags_query {
            run_query(source, tags_query, root, &mut is_cancelled, |qm| {
                process_tags_match(
                    language,
                    source,
                    tags_query,
                    qm,
                    &mut symbols,
                    &mut references,
                );
            })?;
        }
        if let Some(import_query) = &queries.import_query {
            run_query(source, import_query, root, &mut is_cancelled, |qm| {
                process_imports_match(source, import_query, qm, &mut imports);
            })?;
        }
        if matches!(language, "javascript" | "typescript" | "tsx") {
            append_javascript_bindings(source, root, &mut symbols, &mut is_cancelled)?;
        }
        match language {
            "csharp" => {
                append_csharp_structure(
                    source,
                    root,
                    &mut symbols,
                    &mut references,
                    &mut imports,
                    &mut is_cancelled,
                )?;
                imports.sort_by(|a, b| {
                    a.line
                        .cmp(&b.line)
                        .then_with(|| a.raw_target.cmp(&b.raw_target))
                });
            }
            "css" => append_css_structure(
                source,
                root,
                &mut symbols,
                &mut references,
                &mut is_cancelled,
            )?,
            "html" => {
                append_html_structure(
                    source,
                    root,
                    &mut symbols,
                    &mut references,
                    &mut imports,
                    &mut is_cancelled,
                )?;
                imports.sort_by(|a, b| {
                    a.line
                        .cmp(&b.line)
                        .then_with(|| a.raw_target.cmp(&b.raw_target))
                });
            }
            _ => {}
        }

        if is_cancelled() {
            return Err(Error::Cancelled);
        }

        deduplicate_symbols(&mut symbols);
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

struct MarkdownHeading {
    level: usize,
    name: String,
    start_byte: usize,
}

fn parse_markdown(source: &str, is_cancelled: &mut impl FnMut() -> bool) -> Result<ParseOutput> {
    let mut headings = Vec::new();
    let mut pending = None::<MarkdownHeading>;
    for (event, range) in MarkdownParser::new(source).into_offset_iter() {
        if is_cancelled() {
            return Err(Error::Cancelled);
        }
        match event {
            MarkdownEvent::Start(Tag::Heading { level, .. }) => {
                pending = Some(MarkdownHeading {
                    level: markdown_heading_level(level),
                    name: String::new(),
                    start_byte: range.start,
                });
            }
            MarkdownEvent::Text(text)
            | MarkdownEvent::Code(text)
            | MarkdownEvent::InlineMath(text) => {
                if let Some(heading) = &mut pending {
                    heading.name.push_str(&text);
                }
            }
            MarkdownEvent::SoftBreak | MarkdownEvent::HardBreak => {
                if let Some(heading) = &mut pending {
                    heading.name.push(' ');
                }
            }
            MarkdownEvent::End(TagEnd::Heading(_)) => {
                if let Some(mut heading) = pending.take() {
                    heading.name = heading.name.trim().to_owned();
                    if !heading.name.is_empty() {
                        headings.push(heading);
                    }
                }
            }
            _ => {}
        }
    }
    if is_cancelled() {
        return Err(Error::Cancelled);
    }

    let mut symbols = Vec::with_capacity(headings.len());
    for (index, heading) in headings.iter().enumerate() {
        let end_byte = headings[index + 1..]
            .iter()
            .find(|next| next.level <= heading.level)
            .map_or(source.len(), |next| next.start_byte);
        let (start_line, end_line) = byte_range_to_line_range(source, heading.start_byte, end_byte);
        symbols.push(Symbol {
            name: heading.name.clone(),
            kind: "markdown_heading".into(),
            parent: None,
            signature: Some(format!("{} {}", "#".repeat(heading.level), heading.name)),
            start_line,
            end_line,
            start_byte: heading.start_byte,
            end_byte,
        });
    }
    compute_symbol_parents(&mut symbols);

    Ok(ParseOutput {
        language: Some("markdown".into()),
        structurally_complete: true,
        symbols,
        references: Vec::new(),
        imports: Vec::new(),
    })
}

fn markdown_heading_level(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
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
        None => Err(Error::InternalFailure("parser returned None".into())),
    }
}

fn language_object(name: &str) -> Option<Language> {
    Some(match name {
        "c" => tree_sitter_c::LANGUAGE.into(),
        "csharp" => tree_sitter_c_sharp::LANGUAGE.into(),
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
        "html" => tree_sitter_html::LANGUAGE.into(),
        "css" => tree_sitter_css::LANGUAGE.into(),
        _ => return None,
    })
}

fn build_tags_query(language: &str, lang: &Language) -> Result<Option<Query>> {
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
        "csharp" | "html" | "css" => return Ok(None),
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

    Query::new(lang, &source)
        .map(Some)
        .map_err(Error::TreeSitterQuery)
}

fn build_import_query(language: &str, lang: &Language) -> Result<Option<Query>> {
    let src = match language {
        "rust" => RUST_IMPORT_QUERY,
        "python" => PYTHON_IMPORT_QUERY,
        "javascript" | "typescript" | "tsx" => JS_IMPORT_QUERY,
        "go" => GO_IMPORT_QUERY,
        "c" | "csharp" | "cpp" | "java" | "php" | "ruby" | "html" | "css" => {
            return Ok(None);
        }
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
    language: &str,
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
        if is_definition {
            let kind_node = definition_extent(kind_node);
            let (kind, parent) = canonical_definition(language, source, kind, kind_node);
            let (start_line, end_line, start_byte, end_byte) = range_from_node(kind_node);
            symbols.push(Symbol {
                name: name.clone(),
                kind,
                parent,
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
                kind: kind.to_string(),
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

fn append_javascript_bindings(
    source: &str,
    root: Node<'_>,
    symbols: &mut Vec<Symbol>,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<()> {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if is_cancelled() {
            return Err(Error::Cancelled);
        }
        match child.kind() {
            "lexical_declaration" | "variable_declaration" => {
                append_javascript_declaration(source, child, symbols);
            }
            "export_statement" => {
                if let Some(declaration) = child.child_by_field_name("declaration")
                    && matches!(
                        declaration.kind(),
                        "lexical_declaration" | "variable_declaration"
                    )
                {
                    append_javascript_declaration(source, declaration, symbols);
                }
                if javascript_export_is_default(child)
                    && child
                        .child_by_field_name("value")
                        .is_some_and(javascript_is_data_expression)
                {
                    push_javascript_symbol(source, child, "default".into(), "constant", symbols);
                }
            }
            _ => {}
        }
    }

    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if is_cancelled() {
            return Err(Error::Cancelled);
        }
        if matches!(node.kind(), "field_definition" | "public_field_definition") {
            let name = node
                .child_by_field_name("property")
                .or_else(|| node.child_by_field_name("name"));
            if let Some(name) = name {
                let raw_name = node_text(source, name);
                let name = if name.kind() == "string" {
                    unquote(&raw_name).to_string()
                } else {
                    raw_name
                };
                push_javascript_symbol(source, node, name, "field", symbols);
            }
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Ok(())
}

fn append_javascript_declaration(source: &str, declaration: Node<'_>, symbols: &mut Vec<Symbol>) {
    let is_const = {
        let mut cursor = declaration.walk();
        declaration
            .children(&mut cursor)
            .any(|child| child.kind() == "const")
    };
    let kind = if declaration.kind() == "variable_declaration" {
        "variable"
    } else if is_const {
        "constant"
    } else {
        "variable"
    };

    let mut cursor = declaration.walk();
    for declarator in declaration.named_children(&mut cursor) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        let Some(name_node) = declarator.child_by_field_name("name") else {
            continue;
        };
        if name_node.kind() != "identifier" {
            continue;
        }
        let name = node_text(source, name_node);
        if symbols.iter().any(|symbol| {
            symbol.name == name
                && symbol.start_byte == declarator.start_byte()
                && symbol.end_byte == declarator.end_byte()
        }) {
            continue;
        }
        push_javascript_symbol(source, declarator, name, kind, symbols);
    }
}

fn javascript_export_is_default(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == "default")
}

fn javascript_is_data_expression(node: Node<'_>) -> bool {
    match node.kind() {
        "object" | "array" => true,
        "parenthesized_expression"
        | "as_expression"
        | "satisfies_expression"
        | "type_assertion"
        | "non_null_expression" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .any(javascript_is_data_expression)
        }
        _ => false,
    }
}

fn push_javascript_symbol(
    source: &str,
    node: Node<'_>,
    name: String,
    kind: &str,
    symbols: &mut Vec<Symbol>,
) {
    if name.is_empty() {
        return;
    }
    let (start_line, end_line, start_byte, end_byte) = range_from_node(node);
    symbols.push(Symbol {
        name,
        kind: kind.into(),
        parent: None,
        signature: signature_from_node(source, node),
        start_line,
        end_line,
        start_byte,
        end_byte,
    });
}

fn append_csharp_structure(
    source: &str,
    root: Node<'_>,
    symbols: &mut Vec<Symbol>,
    references: &mut Vec<Reference>,
    imports: &mut Vec<Import>,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<()> {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if is_cancelled() {
            return Err(Error::Cancelled);
        }

        if node.kind() == "file_scoped_namespace_declaration"
            && let Some(name) = node.child_by_field_name("name")
        {
            let (start_line, _, start_byte, _) = range_from_node(node);
            let (_, end_line, _, end_byte) = range_from_node(root);
            symbols.push(Symbol {
                name: node_text(source, name),
                kind: "module".into(),
                parent: None,
                signature: signature_from_node(source, node),
                start_line,
                end_line,
                start_byte,
                end_byte,
            });
        }

        let definition_kind = match node.kind() {
            "namespace_declaration" => Some("module"),
            "class_declaration" => Some("class"),
            "struct_declaration" => Some("struct"),
            "interface_declaration" => Some("interface"),
            "enum_declaration" => Some("enum"),
            "record_declaration" => Some("record"),
            "delegate_declaration" => Some("delegate"),
            "method_declaration" => Some("method"),
            "local_function_statement" => Some("function"),
            "constructor_declaration" => Some("constructor"),
            "destructor_declaration" => Some("destructor"),
            "property_declaration" => Some("property"),
            "event_declaration" => Some("event"),
            "enum_member_declaration" => Some("enum_member"),
            _ => None,
        };
        if let Some(kind) = definition_kind
            && let Some(name) = node.child_by_field_name("name")
        {
            push_structural_symbol(
                source,
                node,
                node_text(source, name),
                kind,
                signature_from_node(source, node),
                symbols,
            );
        }

        match node.kind() {
            "variable_declarator" => {
                append_csharp_field(source, node, symbols);
            }
            "indexer_declaration" => {
                push_structural_symbol(
                    source,
                    node,
                    "this[]".into(),
                    "indexer",
                    signature_from_node(source, node),
                    symbols,
                );
            }
            "operator_declaration" => {
                if let Some(operator) = node.child_by_field_name("operator") {
                    push_structural_symbol(
                        source,
                        node,
                        format!("operator {}", node_text(source, operator)),
                        "operator",
                        signature_from_node(source, node),
                        symbols,
                    );
                }
            }
            "conversion_operator_declaration" => {
                if let Some(target_type) = node.child_by_field_name("type") {
                    let declaration = node_text(source, node);
                    let modifier = if declaration
                        .split_whitespace()
                        .any(|part| part == "implicit")
                    {
                        "implicit"
                    } else {
                        "explicit"
                    };
                    push_structural_symbol(
                        source,
                        node,
                        format!("{modifier} operator {}", node_text(source, target_type)),
                        "operator",
                        signature_from_node(source, node),
                        symbols,
                    );
                }
            }
            "invocation_expression" => {
                if let Some(name) = node
                    .child_by_field_name("function")
                    .and_then(csharp_terminal_name)
                {
                    push_structural_reference(
                        node_text(source, name),
                        "call",
                        ReferenceRole::Reference,
                        name,
                        references,
                    );
                }
            }
            "object_creation_expression" => {
                if let Some(name) = node
                    .child_by_field_name("type")
                    .and_then(csharp_terminal_name)
                {
                    push_structural_reference(
                        node_text(source, name),
                        "class",
                        ReferenceRole::Reference,
                        name,
                        references,
                    );
                }
            }
            "base_list" => {
                let mut cursor = node.walk();
                for base in node.named_children(&mut cursor) {
                    let base_type = base.child_by_field_name("type").unwrap_or(base);
                    if let Some(name) = csharp_terminal_name(base_type) {
                        push_structural_reference(
                            node_text(source, name),
                            "type",
                            ReferenceRole::Reference,
                            name,
                            references,
                        );
                    }
                }
            }
            "variable_declaration" => {
                if let Some(name) = node
                    .child_by_field_name("type")
                    .and_then(csharp_terminal_name)
                {
                    push_structural_reference(
                        node_text(source, name),
                        "type",
                        ReferenceRole::Reference,
                        name,
                        references,
                    );
                }
            }
            "using_directive" => {
                if let Some(target) = csharp_using_target(&node_text(source, node)) {
                    push_import(imports, &target, node.start_position().row + 1);
                }
            }
            _ => {}
        }

        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Ok(())
}

fn append_csharp_field(source: &str, node: Node<'_>, symbols: &mut Vec<Symbol>) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let mut owner = node.parent();
    while let Some(candidate) = owner {
        let kind = match candidate.kind() {
            "field_declaration" => "field",
            "event_field_declaration" => "event",
            "variable_declaration" => {
                owner = candidate.parent();
                continue;
            }
            _ => return,
        };
        push_structural_symbol(
            source,
            candidate,
            node_text(source, name),
            kind,
            signature_from_node(source, candidate),
            symbols,
        );
        return;
    }
}

fn csharp_terminal_name(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    if matches!(
        node.kind(),
        "generic_name"
            | "member_access_expression"
            | "member_binding_expression"
            | "qualified_name"
            | "alias_qualified_name"
    ) {
        if let Some(name) = node.child_by_field_name("name") {
            return csharp_terminal_name(name);
        }
        let mut cursor = node.walk();
        if let Some(identifier) = node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "identifier")
        {
            return Some(identifier);
        }
    }

    let mut found = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(name) = csharp_terminal_name(child) {
            found = Some(name);
        }
    }
    found
}

fn csharp_using_target(directive: &str) -> Option<String> {
    let mut target = directive.trim().trim_end_matches(';').trim();
    target = target.strip_prefix("global ").unwrap_or(target).trim();
    target = target.strip_prefix("using ").unwrap_or(target).trim();
    target = target.strip_prefix("static ").unwrap_or(target).trim();
    if let Some((_, aliased)) = target.split_once('=') {
        target = aliased.trim();
    }
    (!target.is_empty()).then(|| target.to_string())
}

fn append_css_structure(
    source: &str,
    root: Node<'_>,
    symbols: &mut Vec<Symbol>,
    references: &mut Vec<Reference>,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<()> {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if is_cancelled() {
            return Err(Error::Cancelled);
        }

        match node.kind() {
            "rule_set" => {
                let mut cursor = node.walk();
                if let Some(selectors) = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "selectors")
                {
                    let name = node_text(source, selectors).trim().to_string();
                    push_structural_symbol(
                        source,
                        node,
                        name.clone(),
                        "css_selector",
                        Some(name),
                        symbols,
                    );
                    append_css_selector_references(source, selectors, references, is_cancelled)?;
                }
            }
            "declaration" => {
                let mut cursor = node.walk();
                if let Some(property) = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "property_name")
                {
                    let name = node_text(source, property);
                    if name.starts_with("--") {
                        push_structural_symbol(
                            source,
                            node,
                            name,
                            "css_custom_property",
                            signature_from_node(source, node),
                            symbols,
                        );
                    }
                }
            }
            "media_statement" => {
                push_css_at_rule(source, node, "css_media", symbols);
            }
            "supports_statement" => {
                push_css_at_rule(source, node, "css_supports", symbols);
            }
            "at_rule"
                if node_text(source, node)
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("@container") =>
            {
                push_css_at_rule(source, node, "css_container", symbols);
            }
            "keyframes_statement" => {
                let mut cursor = node.walk();
                if let Some(name) = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "keyframes_name")
                    .map(|child| node_text(source, child))
                {
                    push_structural_symbol(
                        source,
                        node,
                        name,
                        "css_keyframes",
                        Some(css_header(source, node)),
                        symbols,
                    );
                }
            }
            _ => {}
        }

        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Ok(())
}

fn append_css_selector_references(
    source: &str,
    selectors: Node<'_>,
    references: &mut Vec<Reference>,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<()> {
    let mut pending = vec![selectors];
    while let Some(node) = pending.pop() {
        if is_cancelled() {
            return Err(Error::Cancelled);
        }
        if matches!(
            node.kind(),
            "class_selector"
                | "id_selector"
                | "attribute_selector"
                | "pseudo_class_selector"
                | "pseudo_element_selector"
                | "tag_name"
        ) {
            push_structural_reference(
                node_text(source, node),
                "css_selector",
                ReferenceRole::Definition,
                node,
                references,
            );
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Ok(())
}

fn push_css_at_rule(source: &str, node: Node<'_>, kind: &str, symbols: &mut Vec<Symbol>) {
    let header = css_header(source, node);
    push_structural_symbol(source, node, header.clone(), kind, Some(header), symbols);
}

fn css_header(source: &str, node: Node<'_>) -> String {
    node_text(source, node)
        .split_once('{')
        .map_or_else(|| node_text(source, node), |(header, _)| header.to_string())
        .trim()
        .to_string()
}

#[derive(Clone)]
struct HtmlAttribute<'tree> {
    name: String,
    value: String,
    value_node: Node<'tree>,
}

fn append_html_structure(
    source: &str,
    root: Node<'_>,
    symbols: &mut Vec<Symbol>,
    references: &mut Vec<Reference>,
    imports: &mut Vec<Import>,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<()> {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if is_cancelled() {
            return Err(Error::Cancelled);
        }

        if matches!(node.kind(), "element" | "script_element" | "style_element")
            && let Some((tag_node, tag_name)) = html_tag(source, node)
        {
            let attributes = html_attributes(source, tag_node);
            append_html_element(
                source,
                node,
                tag_node,
                &tag_name,
                &attributes,
                symbols,
                references,
                imports,
            );
        }

        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_html_element(
    source: &str,
    owner: Node<'_>,
    tag_node: Node<'_>,
    tag_name: &str,
    attributes: &[HtmlAttribute<'_>],
    symbols: &mut Vec<Symbol>,
    references: &mut Vec<Reference>,
    imports: &mut Vec<Import>,
) {
    let id = html_attribute(attributes, "id");
    let data_attribute = attributes
        .iter()
        .find(|attribute| attribute.name.starts_with("data-") && !attribute.value.is_empty());
    let href = html_attribute(attributes, "href");
    let name = html_attribute(attributes, "name");

    if let Some(id) = id {
        push_structural_symbol(
            source,
            owner,
            format!("#{}", id.value),
            "html_id",
            signature_from_node(source, tag_node),
            symbols,
        );
        push_structural_reference(
            format!("#{}", id.value),
            "html_id",
            ReferenceRole::Definition,
            id.value_node,
            references,
        );
    } else if html_outline_tag(tag_name) {
        let element_name = if let Some(attribute) = data_attribute {
            format!("{tag_name}[{}={}]", attribute.name, attribute.value)
        } else if let Some(attribute) = name {
            format!("{tag_name}[name={}]", attribute.value)
        } else if let Some(attribute) = href {
            format!("{tag_name}[href={}]", attribute.value)
        } else {
            format!("<{tag_name}>")
        };
        let kind = if html_section_tag(tag_name) {
            "html_section"
        } else if matches!(tag_name, "script" | "style" | "link") {
            "html_resource"
        } else {
            "html_element"
        };
        push_structural_symbol(
            source,
            owner,
            element_name,
            kind,
            signature_from_node(source, tag_node),
            symbols,
        );
    }

    for attribute in attributes {
        if attribute.name.starts_with("data-") && !attribute.value.is_empty() {
            push_structural_reference(
                format!("{}={}", attribute.name, attribute.value),
                "html_data_attribute",
                ReferenceRole::Reference,
                attribute.value_node,
                references,
            );
        }
        if attribute.name == "href" && attribute.value.starts_with('#') && attribute.value.len() > 1
        {
            push_structural_reference(
                attribute.value.clone(),
                "html_anchor",
                ReferenceRole::Reference,
                attribute.value_node,
                references,
            );
        }
        if attribute.name == "for" && !attribute.value.is_empty() {
            push_structural_reference(
                format!("#{}", attribute.value),
                "html_id",
                ReferenceRole::Reference,
                attribute.value_node,
                references,
            );
        }
    }

    match tag_name {
        "script" => {
            if let Some(src) = html_attribute(attributes, "src") {
                push_import(imports, &src.value, src.value_node.start_position().row + 1);
            }
        }
        "link" => {
            let rel = html_attribute(attributes, "rel")
                .map(|attribute| attribute.value.to_ascii_lowercase());
            if rel.as_deref().is_some_and(|rel| {
                rel.split_ascii_whitespace()
                    .any(|value| matches!(value, "stylesheet" | "modulepreload" | "preload"))
            }) && let Some(href) = href
            {
                push_import(
                    imports,
                    &href.value,
                    href.value_node.start_position().row + 1,
                );
            }
        }
        _ => {}
    }
}

fn html_tag<'tree>(source: &str, owner: Node<'tree>) -> Option<(Node<'tree>, String)> {
    let mut cursor = owner.walk();
    let tag = owner
        .named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "start_tag" | "self_closing_tag"))?;
    let mut cursor = tag.walk();
    let name = tag
        .named_children(&mut cursor)
        .find(|child| child.kind() == "tag_name")
        .map(|child| node_text(source, child).to_ascii_lowercase())?;
    Some((tag, name))
}

fn html_attributes<'tree>(source: &str, tag: Node<'tree>) -> Vec<HtmlAttribute<'tree>> {
    let mut cursor = tag.walk();
    tag.named_children(&mut cursor)
        .filter(|child| child.kind() == "attribute")
        .filter_map(|attribute| {
            let mut cursor = attribute.walk();
            let mut children = attribute.named_children(&mut cursor);
            let name = children.next()?;
            let value = children.next()?;
            let value_node = if value.kind() == "quoted_attribute_value" {
                let mut cursor = value.walk();
                value
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "attribute_value")
                    .unwrap_or(value)
            } else {
                value
            };
            Some(HtmlAttribute {
                name: node_text(source, name).to_ascii_lowercase(),
                value: node_text(source, value_node)
                    .trim_matches(['\'', '"'])
                    .to_string(),
                value_node,
            })
        })
        .collect()
}

fn html_attribute<'attributes, 'tree>(
    attributes: &'attributes [HtmlAttribute<'tree>],
    name: &str,
) -> Option<&'attributes HtmlAttribute<'tree>> {
    attributes
        .iter()
        .find(|attribute| attribute.name == name && !attribute.value.is_empty())
}

fn html_section_tag(tag: &str) -> bool {
    matches!(
        tag,
        "main" | "nav" | "section" | "article" | "aside" | "header" | "footer"
    )
}

fn html_outline_tag(tag: &str) -> bool {
    html_section_tag(tag)
        || matches!(
            tag,
            "a" | "button"
                | "dialog"
                | "form"
                | "input"
                | "select"
                | "textarea"
                | "script"
                | "style"
                | "link"
        )
}

fn push_structural_symbol(
    source: &str,
    node: Node<'_>,
    name: String,
    kind: &str,
    signature: Option<String>,
    symbols: &mut Vec<Symbol>,
) {
    if name.is_empty() {
        return;
    }
    let (start_line, end_line, start_byte, end_byte) = range_from_node(node);
    symbols.push(Symbol {
        name,
        kind: kind.into(),
        parent: None,
        signature: signature.or_else(|| signature_from_node(source, node)),
        start_line,
        end_line,
        start_byte,
        end_byte,
    });
}

fn push_structural_reference(
    name: String,
    kind: &str,
    role: ReferenceRole,
    node: Node<'_>,
    references: &mut Vec<Reference>,
) {
    if name.is_empty() {
        return;
    }
    let (start_line, end_line, start_byte, end_byte) = range_from_node(node);
    references.push(Reference {
        name,
        kind: kind.into(),
        role,
        enclosing_symbol: None,
        start_line,
        end_line,
        start_byte,
        end_byte,
    });
}

fn canonical_definition(
    language: &str,
    source: &str,
    kind: &str,
    node: Node<'_>,
) -> (String, Option<String>) {
    match (language, node.kind()) {
        ("rust", "function_item") => rust_function_identity(source, node),
        ("go", "method_declaration") => ("method".into(), go_method_owner(source, node)),
        _ => (kind.to_string(), None),
    }
}

fn rust_function_identity(source: &str, node: Node<'_>) -> (String, Option<String>) {
    let Some(declarations) = node
        .parent()
        .filter(|parent| parent.kind() == "declaration_list")
    else {
        return ("function".into(), None);
    };
    match declarations.parent() {
        Some(owner) if owner.kind() == "impl_item" => {
            let owner = owner
                .child_by_field_name("type")
                .and_then(|node| base_type_name(source, node));
            ("method".into(), owner)
        }
        Some(owner) if owner.kind() == "trait_item" => {
            let owner = owner
                .child_by_field_name("name")
                .and_then(|node| base_type_name(source, node));
            ("method".into(), owner)
        }
        _ => ("function".into(), None),
    }
}

fn go_method_owner(source: &str, node: Node<'_>) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    receiver.named_children(&mut cursor).find_map(|parameter| {
        parameter
            .child_by_field_name("type")
            .and_then(|node| base_type_name(source, node))
    })
}

fn base_type_name(source: &str, node: Node<'_>) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "field_identifier" | "primitive_type" => {
            let name = node_text(source, node);
            (!name.is_empty()).then_some(name)
        }
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|node| base_type_name(source, node)),
        "scoped_type_identifier" | "qualified_type" => node
            .child_by_field_name("name")
            .and_then(|node| base_type_name(source, node)),
        "pointer_type" | "reference_type" | "parenthesized_type" | "bracketed_type"
        | "abstract_type" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find_map(|child| base_type_name(source, child))
        }
        _ => node
            .child_by_field_name("type")
            .and_then(|node| base_type_name(source, node)),
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
    let mut python_module = None;
    let mut python_members = Vec::new();
    let mut python_wildcard = false;
    for cap in qm.captures {
        let cap_name = capture_names[cap.index as usize];
        let raw = node_text(source, cap.node);
        let raw = unquote(&raw);
        if raw.is_empty() {
            continue;
        }
        match cap_name {
            "raw" => push_import(imports, raw, cap.node.start_position().row + 1),
            "python_module" => {
                python_module = Some((raw.to_string(), cap.node.start_position().row + 1));
            }
            "python_member" => {
                let member = cap.node.child_by_field_name("name").unwrap_or(cap.node);
                python_members.push((node_text(source, member), member.start_position().row + 1));
            }
            "python_wildcard" => python_wildcard = true,
            _ => {}
        }
    }

    if let Some((module, line)) = python_module {
        if python_wildcard {
            push_import(imports, &module, line);
        } else if module.bytes().all(|byte| byte == b'.') {
            for (member, member_line) in python_members {
                push_import(imports, &format!("{module}{member}"), member_line);
            }
        } else {
            push_import(imports, &module, line);
        }
    }
}

fn push_import(imports: &mut Vec<Import>, raw_target: &str, line: usize) {
    if imports
        .iter()
        .any(|import| import.line == line && import.raw_target == raw_target)
    {
        return;
    }
    imports.push(Import {
        raw_target: raw_target.to_string(),
        resolved_path: None,
        line,
    });
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
            if strictly_contains(&symbols[top], &symbols[i]) {
                break;
            }
            stack.pop();
        }
        if symbols[i].parent.is_none() {
            symbols[i].parent = stack.last().map(|&top| symbols[top].name.clone());
        }
        stack.push(i);
    }

    symbols.sort_by(|a, b| {
        a.start_byte
            .cmp(&b.start_byte)
            .then_with(|| a.end_byte.cmp(&b.end_byte))
    });
}

fn deduplicate_symbols(symbols: &mut Vec<Symbol>) {
    let mut seen = HashSet::with_capacity(symbols.len());
    symbols.retain(|symbol| {
        seen.insert((
            symbol.kind.clone(),
            symbol.name.clone(),
            symbol.start_byte,
            symbol.end_byte,
        ))
    });
}

fn strictly_contains(parent: &Symbol, child: &Symbol) -> bool {
    parent.start_byte <= child.start_byte
        && parent.end_byte >= child.end_byte
        && (parent.start_byte < child.start_byte || parent.end_byte > child.end_byte)
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

    const CSHARP_SRC: &str = r#"
global using System;
using Text = System.Text;

namespace Clinic.Core;

public delegate void ChangedHandler(int value);
public readonly struct Coordinates {
    public int X { get; }
}
public record Dose(string Name);

public interface IFormatter {
    string Format(int value);
}

public partial class Formatter : IFormatter {
    private readonly int offset;
    public event EventHandler? Changed;
    public string Name { get; init; }

    public Formatter(int offset) {
        this.offset = offset;
    }

    public string Format(int value) {
        string Render(string input) => input.Trim();
        var builder = new Text.StringBuilder();
        Changed?.Invoke(this, EventArgs.Empty);
        return Render(Normalize(builder.ToString()));
    }

    private string Normalize(string value) => value.Trim();
    private T Echo<T>(T value) where T : class => value;
    [System.Runtime.InteropServices.DllImport("native")]
    private static extern int NativeCall(int value);
    public int 計算(int value) => NativeCall(value);
    private sealed class Nested {}
    public int this[int index] => index + offset;
    public static Formatter operator +(Formatter left, int right) => new(left.offset + right);
    public static implicit operator int(Formatter value) => value.offset;
}

public enum State {
    Ready,
    Done,
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
            ("src/Service.cs", "csharp"),
            ("include/value.h", "cpp"),
            ("src/value.cpp", "cpp"),
            ("include/value.hpp", "cpp"),
            ("src/Value.java", "java"),
            ("src/value.php", "php"),
            ("lib/value.rb", "ruby"),
            ("src/index.html", "html"),
            ("src/partial.htm", "html"),
            ("src/styles.css", "css"),
            ("README.md", "markdown"),
            ("docs/GUIDE.markdown", "markdown"),
        ] {
            assert_eq!(language_by_path(path).as_deref(), Some(expected), "{path}");
        }
    }

    #[test]
    fn csharp_indexes_tolerant_structure_imports_calls_and_parents() -> Result<()> {
        let output = parse_language("csharp", CSHARP_SRC)?;
        assert!(output.structurally_complete);
        assert_eq!(import_targets(&output), vec!["System", "System.Text"]);

        for (name, kind, parent) in [
            ("Clinic.Core", "module", None),
            ("ChangedHandler", "delegate", Some("Clinic.Core")),
            ("Coordinates", "struct", Some("Clinic.Core")),
            ("Dose", "record", Some("Clinic.Core")),
            ("IFormatter", "interface", Some("Clinic.Core")),
            ("Formatter", "class", Some("Clinic.Core")),
            ("offset", "field", Some("Formatter")),
            ("Changed", "event", Some("Formatter")),
            ("Name", "property", Some("Formatter")),
            ("Format", "method", Some("Formatter")),
            ("Render", "function", Some("Format")),
            ("Normalize", "method", Some("Formatter")),
            ("Echo", "method", Some("Formatter")),
            ("NativeCall", "method", Some("Formatter")),
            ("計算", "method", Some("Formatter")),
            ("Nested", "class", Some("Formatter")),
            ("this[]", "indexer", Some("Formatter")),
            ("operator +", "operator", Some("Formatter")),
            ("implicit operator int", "operator", Some("Formatter")),
            ("State", "enum", Some("Clinic.Core")),
            ("Ready", "enum_member", Some("State")),
        ] {
            assert!(
                output.symbols.iter().any(|symbol| {
                    symbol.name == name && symbol.kind == kind && symbol.parent.as_deref() == parent
                }),
                "missing {kind} {name} with parent {parent:?}: {:?}",
                output.symbols
            );
        }

        let format = output
            .symbols
            .iter()
            .find(|symbol| symbol.name == "Format" && symbol.parent.as_deref() == Some("Formatter"))
            .expect("Format method");
        assert!(
            CSHARP_SRC[format.start_byte..format.end_byte].contains("return Render"),
            "method extent should retain the complete body"
        );
        let echo = output
            .symbols
            .iter()
            .find(|symbol| symbol.name == "Echo")
            .expect("generic method");
        assert!(
            echo.signature
                .as_deref()
                .is_some_and(|signature| signature.contains("<T>") && signature.contains("where T"))
        );

        for name in [
            "IFormatter",
            "StringBuilder",
            "Invoke",
            "ToString",
            "Normalize",
            "Render",
            "NativeCall",
        ] {
            let expected_owner = match name {
                "IFormatter" => None,
                "NativeCall" => Some("計算"),
                _ => Some("Format"),
            };
            assert!(
                output.references.iter().any(|reference| {
                    reference.name == name
                        && expected_owner.is_none_or(|owner| {
                            reference.enclosing_symbol.as_deref() == Some(owner)
                        })
                }),
                "missing call reference {name}: {:?}",
                output.references
            );
        }
        Ok(())
    }

    #[test]
    fn malformed_csharp_retains_recoverable_symbols() -> Result<()> {
        let output = parse_language(
            "csharp",
            "class Worker { int Ready() => 1; int Broken( { return 2; } }",
        )?;
        assert!(!output.structurally_complete);
        assert!(output.symbols.iter().any(|symbol| symbol.name == "Worker"));
        assert!(output.symbols.iter().any(|symbol| symbol.name == "Ready"));
        Ok(())
    }

    #[test]
    fn markdown_headings_define_nested_sections_and_ignore_fenced_code() -> Result<()> {
        let source = "\
# Root
intro
## Repeat
first
### Child
child
## Repeat
second

Setext
------
```markdown
# hidden
```
";
        let output = parse("README.md", source)?;

        assert_eq!(output.language.as_deref(), Some("markdown"));
        assert!(output.structurally_complete);
        assert_eq!(
            output
                .symbols
                .iter()
                .map(|symbol| (
                    symbol.name.as_str(),
                    symbol.kind.as_str(),
                    symbol.parent.as_deref(),
                    symbol.start_line,
                    symbol.end_line,
                    symbol.signature.as_deref(),
                ))
                .collect::<Vec<_>>(),
            vec![
                ("Root", "markdown_heading", None, 1, 14, Some("# Root")),
                (
                    "Repeat",
                    "markdown_heading",
                    Some("Root"),
                    3,
                    6,
                    Some("## Repeat")
                ),
                (
                    "Child",
                    "markdown_heading",
                    Some("Repeat"),
                    5,
                    6,
                    Some("### Child")
                ),
                (
                    "Repeat",
                    "markdown_heading",
                    Some("Root"),
                    7,
                    9,
                    Some("## Repeat")
                ),
                (
                    "Setext",
                    "markdown_heading",
                    Some("Root"),
                    10,
                    14,
                    Some("## Setext")
                ),
            ]
        );
        assert!(!output.symbols.iter().any(|symbol| symbol.name == "hidden"));
        Ok(())
    }

    #[test]
    fn css_indexes_selectors_custom_properties_conditions_and_keyframes() -> Result<()> {
        let source = r#"
:root {
  --clinic-accent: #0b6;
}
.clinic-hero {
  color: var(--clinic-accent);
}
.clinic-card, #clinic-panel > .clinic-title {
  display: grid;
}
@media (max-width: 720px) {
  .clinic-hero { display: block; }
}
@supports (display: grid) {
  .clinic-grid { display: grid; }
}
@container (min-width: 40rem) {
  .clinic-card { grid-template-columns: 1fr 1fr; }
}
@keyframes clinic-pulse {
  from { opacity: 0; }
  to { opacity: 1; }
}
"#;
        let output = parse_language("css", source)?;

        for (name, kind) in [
            (":root", "css_selector"),
            (".clinic-hero", "css_selector"),
            (
                ".clinic-card, #clinic-panel > .clinic-title",
                "css_selector",
            ),
            ("--clinic-accent", "css_custom_property"),
            ("clinic-pulse", "css_keyframes"),
        ] {
            assert!(
                output
                    .symbols
                    .iter()
                    .any(|symbol| symbol.name == name && symbol.kind == kind),
                "missing {kind} {name}: {:?}",
                output.symbols
            );
        }
        for kind in ["css_media", "css_supports", "css_container"] {
            assert!(
                output.symbols.iter().any(|symbol| symbol.kind == kind),
                "missing {kind}: {:?}",
                output.symbols
            );
        }
        for name in [".clinic-card", "#clinic-panel", ".clinic-title"] {
            assert!(
                output
                    .references
                    .iter()
                    .any(|reference| reference.name == name),
                "missing selector reference {name}: {:?}",
                output.references
            );
        }
        let hero = output
            .symbols
            .iter()
            .find(|symbol| symbol.name == ".clinic-hero" && symbol.parent.is_none())
            .expect("top-level hero selector");
        assert!(source[hero.start_byte..hero.end_byte].contains("color: var(--clinic-accent)"));
        let responsive_hero = output
            .symbols
            .iter()
            .find(|symbol| symbol.name == ".clinic-hero" && symbol.parent.is_some())
            .expect("nested hero selector");
        assert!(
            responsive_hero
                .parent
                .as_deref()
                .is_some_and(|parent| parent.starts_with("@media"))
        );
        assert!(output.structurally_complete);
        Ok(())
    }

    #[test]
    fn html_indexes_sections_controls_actions_anchors_and_resources() -> Result<()> {
        let source = r##"
<!doctype html>
<html>
<head>
  <link rel="stylesheet" href="./styles/clinic.css">
</head>
<body>
  <nav id="mobile-nav" data-action="toggle-nav">
    <a href="#clinic">Clinic</a>
  </nav>
  <main>
    <section id="clinic">
      <form id="clinic-form">
        <label for="therapy">Therapy</label>
        <select name="therapy" id="therapy"></select>
        <input name="query">
        <button data-action="book-therapy">Book</button>
      </form>
      <dialog id="clinic-dialog"></dialog>
    </section>
  </main>
  <script type="module" src="./js/clinic.js"></script>
</body>
</html>
"##;
        let output = parse_language("html", source)?;

        for name in [
            "#mobile-nav",
            "#clinic",
            "#clinic-form",
            "#therapy",
            "#clinic-dialog",
            "input[name=query]",
            "button[data-action=book-therapy]",
            "<script>",
            "link[href=./styles/clinic.css]",
        ] {
            assert!(
                output.symbols.iter().any(|symbol| symbol.name == name),
                "missing HTML symbol {name}: {:?}",
                output.symbols
            );
        }
        assert_eq!(
            output
                .symbols
                .iter()
                .find(|symbol| symbol.name == "#clinic-form")
                .and_then(|symbol| symbol.parent.as_deref()),
            Some("#clinic")
        );
        for (name, role) in [
            ("#clinic", ReferenceRole::Reference),
            ("data-action=toggle-nav", ReferenceRole::Reference),
            ("data-action=book-therapy", ReferenceRole::Reference),
            ("#therapy", ReferenceRole::Reference),
        ] {
            assert!(
                output
                    .references
                    .iter()
                    .any(|reference| reference.name == name && reference.role == role),
                "missing HTML reference {name}: {:?}",
                output.references
            );
        }
        assert_eq!(
            import_targets(&output),
            vec!["./styles/clinic.css", "./js/clinic.js"]
        );
        let clinic = output
            .symbols
            .iter()
            .find(|symbol| symbol.name == "#clinic")
            .expect("clinic section");
        assert!(source[clinic.start_byte..clinic.end_byte].contains("clinic-dialog"));
        assert!(output.structurally_complete);
        Ok(())
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
    fn rust_canonicalizes_function_identity_and_method_owners() -> Result<()> {
        let source = r#"
struct Point;
struct Wrapper<T>(T);
mod nested {
    pub struct Scoped<T>(pub T);
}

fn top_level() {}

mod tests {
    fn helper() {}
}

impl Point {
    fn distance(&self) {}

    const VALUE: usize = {
        fn associated_helper() -> usize { 1 }
        associated_helper()
    };
}

impl<T> Wrapper<T> {
    fn generic_owner(&self) {}
}

impl<T> nested::Scoped<T> {
    fn scoped_owner(&self) {}
}

trait Render {
    fn render(&self) {}
}

trait Local {
    fn primitive_owner(&self);
}

impl Local for u32 {
    fn primitive_owner(&self) {}
}
"#;
        let output = parse_language("rust", source)?;

        for (name, kind, parent) in [
            ("top_level", "function", None),
            ("helper", "function", Some("tests")),
            ("distance", "method", Some("Point")),
            ("associated_helper", "function", Some("VALUE")),
            ("generic_owner", "method", Some("Wrapper")),
            ("scoped_owner", "method", Some("Scoped")),
            ("render", "method", Some("Render")),
            ("primitive_owner", "method", Some("u32")),
        ] {
            let matching = output
                .symbols
                .iter()
                .filter(|symbol| symbol.name == name)
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1, "symbols for {name}: {matching:?}");
            assert_eq!(matching[0].kind, kind, "symbol: {:?}", matching[0]);
            assert_eq!(
                matching[0].parent.as_deref(),
                parent,
                "symbol: {:?}",
                matching[0]
            );
        }

        assert!(output.symbols.iter().all(|symbol| {
            symbol.parent.as_deref() != Some(symbol.name.as_str())
                || output.symbols.iter().any(|candidate| {
                    candidate.name == symbol.name
                        && candidate.start_byte < symbol.start_byte
                        && candidate.end_byte > symbol.end_byte
                })
        }));
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
        assert!(!imports.contains(&"defaultdict"), "imports: {imports:?}");

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
    fn python_imports_preserve_module_semantics() -> Result<()> {
        let out = parse_language(
            "python",
            "from pkg.mod import thing, other\nfrom . import helpers, tools, aliased\tas\tlocal\nfrom ..core import api\n",
        )?;
        assert_eq!(
            import_targets(&out),
            vec!["pkg.mod", ".helpers", ".tools", ".aliased", "..core"]
        );
        Ok(())
    }

    #[test]
    fn python_wildcard_imports_preserve_their_module() -> Result<()> {
        let out = parse_language("python", "from pkg.mod import *\nfrom . import *\n")?;
        assert_eq!(import_targets(&out), vec!["pkg.mod", "."]);
        Ok(())
    }

    #[test]
    fn python_relative_import_members_preserve_their_source_lines() -> Result<()> {
        let out = parse_language("python", "from . import (\n    helpers,\n    tools,\n)\n")?;
        assert_eq!(
            out.imports
                .iter()
                .map(|import| (import.raw_target.as_str(), import.line))
                .collect::<Vec<_>>(),
            vec![(".helpers", 2), (".tools", 3)]
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
    fn javascript_indexes_top_level_data_bindings_without_local_noise() -> Result<()> {
        let source = r#"
export const CLINIC_KEYS = { medicines: "clinic:medicines" };
const clinicMedicines = [{ id: "moon-rabbit-saline", labels: ["en", "ja", "zh"] }];
let copy = { en: { title: "Clinic" }, ja: {}, zh: {} };
var legacyRows = [1, 2, 3];
const primary = 1, secondary = { enabled: true };
export const handler = () => true;
function scoped() {
  const localOnly = { hidden: true };
}
class Catalog {
  entries = [{ id: "entry" }];
  static settings = { pageSize: 20 };
}
export default { clinicMedicines, copy };
"#;
        let output = parse_language("javascript", source)?;

        for (name, kind) in [
            ("CLINIC_KEYS", "constant"),
            ("clinicMedicines", "constant"),
            ("copy", "variable"),
            ("legacyRows", "variable"),
            ("primary", "constant"),
            ("secondary", "constant"),
            ("default", "constant"),
            ("entries", "field"),
            ("settings", "field"),
        ] {
            assert!(
                output
                    .symbols
                    .iter()
                    .any(|symbol| symbol.name == name && symbol.kind == kind),
                "missing {kind} {name}: {:?}",
                output.symbols
            );
        }
        assert!(
            !output
                .symbols
                .iter()
                .any(|symbol| symbol.name == "localOnly")
        );
        assert_eq!(
            output
                .symbols
                .iter()
                .filter(|symbol| symbol.name == "handler")
                .count(),
            1
        );
        let medicines = output
            .symbols
            .iter()
            .find(|symbol| symbol.name == "clinicMedicines")
            .expect("medicine data symbol");
        assert!(
            source[medicines.start_byte..medicines.end_byte].starts_with("clinicMedicines = [")
        );
        let entries = output
            .symbols
            .iter()
            .find(|symbol| symbol.name == "entries")
            .expect("class field");
        assert_eq!(entries.parent.as_deref(), Some("Catalog"));

        let array_default = parse_language(
            "javascript",
            "export default [{ id: \"default-array-item\" }];\n",
        )?;
        assert!(
            array_default
                .symbols
                .iter()
                .any(|symbol| symbol.name == "default" && symbol.kind == "constant")
        );
        Ok(())
    }

    #[test]
    fn typescript_indexes_annotated_and_wrapped_data_bindings() -> Result<()> {
        let source = r#"
type Therapy = { id: string };
export const therapies: readonly Therapy[] = [{ id: "boundary-anchor" }] as const;
const copy = { en: "Clinic", ja: "診療所", zh: "診所" } satisfies Record<string, string>;
class Store {
  public entries: Therapy[] = [];
  private settings = { pageSize: 20 };
}
export default ({ therapies, copy } satisfies Record<string, unknown>);
"#;
        let output = parse_language("typescript", source)?;

        for name in ["therapies", "copy", "entries", "settings", "default"] {
            assert!(
                output.symbols.iter().any(|symbol| symbol.name == name),
                "missing {name}: {:?}",
                output.symbols
            );
        }
        let therapies = output
            .symbols
            .iter()
            .find(|symbol| symbol.name == "therapies")
            .expect("therapies data symbol");
        assert_eq!(therapies.kind, "constant");
        assert!(source[therapies.start_byte..therapies.end_byte].contains("as const"));
        for field in ["entries", "settings"] {
            assert_eq!(
                output
                    .symbols
                    .iter()
                    .find(|symbol| symbol.name == field)
                    .and_then(|symbol| symbol.parent.as_deref()),
                Some("Store")
            );
        }
        let tsx = parse_language(
            "tsx",
            "export const labels = { title: <span>Clinic</span> };\n",
        )?;
        assert!(
            tsx.symbols
                .iter()
                .any(|symbol| symbol.name == "labels" && symbol.kind == "constant")
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
    fn go_methods_use_value_pointer_and_generic_receiver_owners() -> Result<()> {
        let source = r#"
package sample

type Point struct{}
func (p Point) Value() {}
func (p *Point) Pointer() {}

type Pair[T any] struct{}
func (p Pair[T]) Generic() {}
"#;
        let output = parse_language("go", source)?;

        for (name, parent) in [
            ("Value", "Point"),
            ("Pointer", "Point"),
            ("Generic", "Pair"),
        ] {
            let matching = output
                .symbols
                .iter()
                .filter(|symbol| symbol.name == name)
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1, "symbols for {name}: {matching:?}");
            assert_eq!(matching[0].kind, "method");
            assert_eq!(matching[0].parent.as_deref(), Some(parent));
        }
        Ok(())
    }

    #[test]
    fn malformed_source_is_marked_incomplete() -> Result<()> {
        let out = parse_language("rust", "fn broken(")?;
        assert!(!out.structurally_complete);
        Ok(())
    }
}
