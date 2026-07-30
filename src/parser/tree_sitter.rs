use super::*;

pub(super) fn empty_parse() -> ParseOutput {
    ParseOutput {
        language: None,
        structurally_complete: false,
        symbols: Vec::new(),
        references: Vec::new(),
        imports: Vec::new(),
    }
}

pub(super) fn parse_tree(
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
        let mut progress = |_: &::tree_sitter::ParseState| {
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
        None => Err(Error::OperationFailure("parser returned None".into())),
    }
}

pub(super) fn language_object(name: &str) -> Option<Language> {
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

pub(super) fn build_tags_query(language: &str, lang: &Language) -> Result<Option<Query>> {
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

pub(super) fn build_import_query(language: &str, lang: &Language) -> Result<Option<Query>> {
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

pub(super) fn run_query<F>(
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
        let mut progress = |_: &::tree_sitter::QueryCursorState| {
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

pub(super) fn process_tags_match(
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
