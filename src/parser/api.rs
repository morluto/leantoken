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
        "swift" => "swift".to_string(),
        "html" | "htm" => "html".to_string(),
        "css" => "css".to_string(),
        "md" | "markdown" => "markdown".to_string(),
        "tex" | "ltx" => "latex".to_string(),
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
    if language == "latex" {
        return parse_latex(source, &mut is_cancelled);
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
            "swift" => {
                append_swift_structure(
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
        if language == "swift" {
            retain_bounded_swift_calls(&symbols, &mut references);
        }

        Ok(ParseOutput {
            language: Some(language.to_string()),
            structurally_complete,
            symbols,
            references,
            imports,
        })
    })
}
