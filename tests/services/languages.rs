use super::*;

#[tokio::test]
async fn multilingual_structural_indexing_returns_new_language_symbol_bodies() {
    let root = tempfile::tempdir().expect("root");
    for (path, source) in [
        (
            "target.c",
            "int c_target(int value) {\n    return value + 11;\n}\n",
        ),
        (
            "CSharpTarget.cs",
            "class CSharpTarget {\n    int CsharpTarget() {\n        return 66;\n    }\n}\n",
        ),
        (
            "target.cpp",
            "class CppTarget {\npublic:\n    int cpp_target() { return 22; }\n};\n",
        ),
        (
            "JavaTarget.java",
            "class JavaTarget {\n    int javaTarget() {\n        return 33;\n    }\n}\n",
        ),
        (
            "target.php",
            "<?php\nfunction phpTarget() {\n    return 44;\n}\n",
        ),
        (
            "target.rb",
            "def ruby_target\n  55\nend\n",
        ),
    ] {
        std::fs::write(root.path().join(path), source).expect("source");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    for (path, symbol, marker) in [
        ("target.c", "c_target", "return value + 11"),
        ("CSharpTarget.cs", "CsharpTarget", "return 66"),
        ("target.cpp", "cpp_target", "return 22"),
        ("JavaTarget.java", "javaTarget", "return 33"),
        ("target.php", "phpTarget", "return 44"),
        ("target.rb", "ruby_target", "55"),
    ] {
        let outline = services
            .outline(OutlineRequest {
                paths: vec![path.into()],
                symbol_name: Some(symbol.into()),
                symbol_kind: None,
                max_results: Some(10),
                max_tokens: Some(200),
                receipt_id: None,
                cursor: None,
            })
            .await
            .expect("outline");
        assert!(
            outline.files[0]
                .symbols
                .iter()
                .any(|item| item.name == symbol && item.end_line >= item.start_line),
            "missing {symbol} in {path}: {:?}",
            outline.files[0].symbols
        );

        let context = services
            .context(ContextRequest {
                task: format!("Fix {symbol}"),
                token_budget: 300,
                include_paths: Vec::new(),
                must_include_paths: Vec::new(),
                must_include_symbols: Vec::new(),
                required_evidence: Vec::new(),
                max_fragments: None,
                plan_only: false,
                focus_paths: Vec::new(),
                strict_focus_paths: false,
                minimum_fragments_per_focus_path: None,
                focus_symbols: Vec::new(),
                exclude_paths: Vec::new(),
                known_hashes: Vec::new(),
                receipt_id: None,
                prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
            strict_changed_paths: false,
            explain_diagnostics: false,
            })
            .await
            .expect("context");
        assert!(
            context
                .fragments
                .iter()
                .any(|fragment| fragment.path == path && fragment.content.contains(marker)),
            "missing body for {symbol}: {:?}",
            context.fragments
        );
    }
}

#[tokio::test]
async fn csharp_structure_supports_outline_search_reference_read_and_context() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("Worker.cs"),
        r#"using System.Text;

namespace Clinic.Core;

public sealed class Worker {
    public string Run(int value) {
        var builder = new StringBuilder();
        return Normalize(builder.Append(value).ToString());
    }

    private string Normalize(string value) => value.Trim();
}
"#,
    )
    .expect("C# source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["Worker.cs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(20),
            max_tokens: Some(2_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("C# outline");
    assert_eq!(outline.files[0].language.as_deref(), Some("csharp"));
    assert!(
        outline.files[0].symbols.iter().any(|symbol| {
            symbol.name == "Run"
                && symbol.kind == "method"
                && symbol.parent.as_deref() == Some("Worker")
        }),
        "{:?}",
        outline.files[0].symbols
    );

    let symbol_search = services
        .search(SearchRequest {
            query: "Run".into(),
            mode: SearchMode::Symbol,
            include_paths: vec!["Worker.cs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(2_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("C# symbol search");
    assert!(
        symbol_search
            .hits
            .iter()
            .any(|hit| hit.symbol.as_deref() == Some("Run"))
    );

    let reference_search = services
        .search(SearchRequest {
            query: "Normalize".into(),
            mode: SearchMode::Reference,
            include_paths: vec!["Worker.cs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(2_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("C# reference search");
    assert!(reference_search.hits.iter().any(|hit| {
        hit.symbol.as_deref() == Some("Normalize")
            && hit.enclosing_symbol.as_deref() == Some("Run")
    }));

    let read = services
        .read(ReadRequest {
            path: "Worker.cs".into(),
            start_line: None,
            end_line: None,
            symbol: Some("Worker.Run".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(2_000),
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("qualified C# symbol read");
    assert!(
        read.content
            .as_deref()
            .is_some_and(|content| content.contains("return Normalize"))
    );

    let context = services
        .context(ContextRequest {
            task: "Fix the Run method".into(),
            token_budget: 500,
            include_paths: vec!["Worker.cs".into()],
            must_include_paths: Vec::new(),
            must_include_symbols: vec!["Run".into()],
            required_evidence: Vec::new(),
            max_fragments: None,
            plan_only: false,
            focus_paths: Vec::new(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: vec!["Run".into()],
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            receipt_id: None,
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
            strict_changed_paths: false,
            explain_diagnostics: false,
        })
        .await
        .expect("C# context");
    assert!(
        context.fragments.iter().any(|fragment| {
            fragment.path == "Worker.cs" && fragment.content.contains("return Normalize")
        }),
        "{:?}",
        context.fragments
    );
}

#[tokio::test]
async fn javascript_and_typescript_data_bindings_support_outline_search_and_read() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("clinic.js"),
        r#"export const clinicMedicines = [
  { id: "moon-rabbit-saline", labels: { en: "Saline", ja: "生理食塩水" } },
  { id: "boundary-anchor-patch", labels: { en: "Patch", zh: "貼片" } }
];
function helper() {
  const localOnly = { hidden: true };
  return localOnly;
}
"#,
    )
    .expect("JavaScript data");
    std::fs::write(
        root.path().join("copy.ts"),
        r#"export const copy: Record<string, string> = {
  en: "Campus clinic",
  ja: "キャンパス診療所",
  zh: "校園診所"
} satisfies Record<string, string>;
"#,
    )
    .expect("TypeScript data");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    for (path, symbol, marker) in [
        ("clinic.js", "clinicMedicines", "boundary-anchor-patch"),
        ("copy.ts", "copy", "校園診所"),
    ] {
        let outline = services
            .outline(OutlineRequest {
                paths: vec![path.into()],
                symbol_name: None,
                symbol_kind: None,
                max_results: Some(20),
                max_tokens: Some(2_000),
                receipt_id: None,
                cursor: None,
            })
            .await
            .expect("outline data file");
        assert!(
            outline.files[0]
                .symbols
                .iter()
                .any(|item| item.name == symbol && item.kind == "constant"),
            "missing {symbol}: {:?}",
            outline.files[0].symbols
        );
        assert!(
            !outline.files[0]
                .symbols
                .iter()
                .any(|item| item.name == "localOnly")
        );

        let search = services
            .search(SearchRequest {
                query: symbol.into(),
                mode: SearchMode::Symbol,
                include_paths: vec![path.into()],
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: Some(2_000),
                context_lines: Some(0),
                case_sensitive: true,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                query_receipt: None,
                cursor: None,
            })
            .await
            .expect("symbol search");
        assert!(
            search
                .hits
                .iter()
                .any(|hit| hit.symbol.as_deref() == Some(symbol))
        );

        let read = services
            .read(ReadRequest {
                path: path.into(),
                start_line: None,
                end_line: None,
                symbol: Some(symbol.into()),
                heading: None,
                heading_occurrence: None,
                continuation_cursor: None,
                max_tokens: Some(2_000),
                expected_hash: None,
                delta: false,
                receipt_id: None,
                policy: leantoken::ReadPolicy::default(),
            })
            .await
            .expect("symbol read");
        assert!(
            read.content
                .as_deref()
                .is_some_and(|content| content.contains(marker)),
            "missing {marker} in {symbol} read: {:?}",
            read.content
        );
    }
}

#[tokio::test]
async fn html_and_css_structure_support_outline_search_reference_and_read() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("styles")).expect("styles directory");
    std::fs::create_dir(root.path().join("js")).expect("JavaScript directory");
    std::fs::write(
        root.path().join("styles/clinic.css"),
        r#":root {
  --clinic-accent: #087;
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
"#,
    )
    .expect("CSS source");
    std::fs::write(
        root.path().join("index.html"),
        r##"<!doctype html>
<html>
<head>
  <link rel="stylesheet" href="./styles/clinic.css">
</head>
<body>
  <nav id="mobile-nav" data-action="toggle-nav">
    <a href="#clinic">Clinic</a>
  </nav>
  <section id="clinic">
    <form id="clinic-form">
      <button data-action="book-therapy">Book</button>
    </form>
  </section>
  <script type="module" src="./js/clinic.js"></script>
</body>
</html>
"##,
    )
    .expect("HTML source");
    std::fs::write(root.path().join("js/clinic.js"), "export const clinic = {};\n")
        .expect("JavaScript source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["styles/clinic.css".into(), "index.html".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(4_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("frontend outlines");
    let css = outline
        .files
        .iter()
        .find(|file| file.path == "styles/clinic.css")
        .expect("CSS outline");
    assert!(css.parse_complete);
    assert!(
        css.symbols
            .iter()
            .any(|symbol| symbol.name == ".clinic-hero" && symbol.kind == "css_selector")
    );
    let top_level_hero = css
        .symbols
        .iter()
        .find(|symbol| symbol.name == ".clinic-hero" && symbol.parent.is_none())
        .expect("top-level hero selector");
    assert!(
        css.symbols.iter().any(
            |symbol| symbol.name == "--clinic-accent" && symbol.kind == "css_custom_property"
        )
    );
    let html = outline
        .files
        .iter()
        .find(|file| file.path == "index.html")
        .expect("HTML outline");
    assert!(html.parse_complete);
    assert!(
        html.symbols
            .iter()
            .any(|symbol| symbol.name == "#clinic" && symbol.kind == "html_id")
    );
    assert_eq!(
        html.imports
            .iter()
            .map(|import| (
                import.raw_target.as_str(),
                import.resolved_path.as_deref()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("./styles/clinic.css", Some("styles/clinic.css")),
            ("./js/clinic.js", Some("js/clinic.js"))
        ]
    );

    for (query, mode, path) in [
        (".clinic-hero", SearchMode::Symbol, "styles/clinic.css"),
        (".clinic-title", SearchMode::Reference, "styles/clinic.css"),
        ("#clinic", SearchMode::Reference, "index.html"),
        (
            "data-action=book-therapy",
            SearchMode::Reference,
            "index.html",
        ),
    ] {
        let search = services
            .search(SearchRequest {
                query: query.into(),
                mode,
                include_paths: vec![path.into()],
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: Some(2_000),
                context_lines: Some(0),
                case_sensitive: true,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                query_receipt: None,
                cursor: None,
            })
            .await
            .expect("structural search");
        assert!(!search.hits.is_empty(), "missing {mode:?} search for {query}");
    }

    let ambiguous_hero = services
        .read(ReadRequest {
            path: "styles/clinic.css".into(),
            start_line: None,
            end_line: None,
            symbol: Some(".clinic-hero".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(2_000),
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect_err("repeated selector requires an exact outline range");
    assert!(matches!(
        ambiguous_hero,
        Error::AmbiguousSymbol { path, symbol }
            if path == "styles/clinic.css" && symbol == ".clinic-hero"
    ));
    let hero = services
        .read(ReadRequest {
            path: "styles/clinic.css".into(),
            start_line: Some(top_level_hero.start_line),
            end_line: Some(top_level_hero.end_line),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(2_000),
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("exact top-level selector range");
    assert!(
        hero.content
            .as_deref()
            .is_some_and(|content| content.contains("color: var(--clinic-accent)"))
    );

    for (path, symbol, marker) in [("index.html", "#clinic", "data-action=\"book-therapy\"")] {
        let read = services
            .read(ReadRequest {
                path: path.into(),
                start_line: None,
                end_line: None,
                symbol: Some(symbol.into()),
                heading: None,
                heading_occurrence: None,
                continuation_cursor: None,
                max_tokens: Some(2_000),
                expected_hash: None,
                delta: false,
                receipt_id: None,
                policy: leantoken::ReadPolicy::default(),
            })
            .await
            .expect("structural symbol read");
        assert!(
            read.content
                .as_deref()
                .is_some_and(|content| content.contains(marker)),
            "missing {marker} from {symbol} read: {:?}",
            read.content
        );
    }
}

#[tokio::test]
async fn markdown_outline_and_heading_read_preserve_section_structure_and_occurrences() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("README.md"),
        "\
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
",
    )
    .expect("Markdown source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["README.md".into()],
            symbol_name: None,
            symbol_kind: Some("markdown_heading".into()),
            max_results: Some(20),
            max_tokens: Some(2_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("Markdown outline");
    assert!(outline.parse_complete);
    assert!(outline.result_complete);
    assert_eq!(outline.total_symbols, 5);
    assert_eq!(
        outline.symbol_counts_by_kind.get("markdown_heading"),
        Some(&5)
    );
    let markdown = &outline.files[0];
    assert_eq!(markdown.language.as_deref(), Some("markdown"));
    assert!(markdown.parse_complete);
    assert_eq!(
        markdown
            .symbols
            .iter()
            .map(|symbol| (
                symbol.name.as_str(),
                symbol.parent.as_deref(),
                symbol.start_line,
                symbol.end_line,
            ))
            .collect::<Vec<_>>(),
        vec![
            ("Root", None, 1, 14),
            ("Repeat", Some("Root"), 3, 6),
            ("Child", Some("Repeat"), 5, 6),
            ("Repeat", Some("Root"), 7, 9),
            ("Setext", Some("Root"), 10, 14),
        ]
    );
    assert!(!markdown.symbols.iter().any(|symbol| symbol.name == "hidden"));

    for (occurrence, expected_range, expected_content) in [
        (
            None,
            (3, 6),
            "## Repeat\nfirst\n### Child\nchild",
        ),
        (Some(2), (7, 9), "## Repeat\nsecond"),
    ] {
        let read = services
            .read(ReadRequest {
                path: "README.md".into(),
                start_line: None,
                end_line: None,
                symbol: None,
                heading: Some(if occurrence == Some(2) {
                    "## Repeat".into()
                } else {
                    "Repeat".into()
                }),
                heading_occurrence: occurrence,
                continuation_cursor: None,
                max_tokens: Some(2_000),
                expected_hash: None,
                delta: false,
                receipt_id: None,
                policy: leantoken::ReadPolicy::default(),
            })
            .await
            .expect("Markdown heading read");
        assert_eq!(
            (read.target_start_line, read.target_end_line),
            expected_range
        );
        assert_eq!(read.content.as_deref().map(str::trim_end), Some(expected_content));
    }

    let error = services
        .read(ReadRequest {
            path: "README.md".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: Some("Repeat".into()),
            heading_occurrence: Some(3),
            continuation_cursor: None,
            max_tokens: Some(2_000),
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect_err("missing duplicate occurrence");
    assert!(matches!(
        error,
        Error::HeadingNotFound {
            path,
            heading,
            occurrence: 3
        } if path == "README.md" && heading == "Repeat"
    ));

    let error = services
        .read(ReadRequest {
            path: "README.md".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: Some("Repeat".into()),
            heading_occurrence: Some(0),
            continuation_cursor: None,
            max_tokens: Some(2_000),
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect_err("zero heading occurrence");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "heading occurrence",
            reason: "must be one-based"
        }
    ));
}

#[tokio::test]
async fn latex_outline_and_read_share_exact_section_label_and_bibliography_structure() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("sections")).expect("sections");
    std::fs::write(
        root.path().join("paper.tex"),
        "\\section{Overview}\nintro\n\\subsection{Method}\nmethod \\cite{alpha}\n\\label{sec:method}\n\\input{sections/results}\n\\section{References}\n\\begin{thebibliography}{9}\n\\bibitem{alpha} Source.\n\\end{thebibliography}\n",
    )
    .expect("LaTeX source");
    std::fs::write(root.path().join("sections/results.tex"), "result\n").expect("input");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["paper.tex".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(50),
            max_tokens: Some(4_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("LaTeX outline");
    assert!(outline.parse_complete);
    assert!(outline.result_complete);
    let latex = &outline.files[0];
    assert_eq!(latex.language.as_deref(), Some("latex"));
    assert!(latex.parse_complete);
    assert!(latex.symbols.iter().any(|symbol| {
        symbol.name == "Method"
            && symbol.kind == "latex_subsection"
            && symbol.start_line == 3
            && symbol.end_line == 6
    }));
    assert_eq!(
        latex
            .imports
            .iter()
            .map(|import| (
                import.raw_target.as_str(),
                import.resolved_path.as_deref(),
                import.line
            ))
            .collect::<Vec<_>>(),
        vec![("sections/results", Some("sections/results.tex"), 6)]
    );

    let references = services
        .search(SearchRequest {
            query: "alpha".into(),
            mode: SearchMode::Reference,
            include_paths: vec!["paper.tex".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(2_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("LaTeX citation audit");
    assert!(references.hits.iter().any(|hit| {
        hit.symbol.as_deref() == Some("alpha")
            && hit.role == Some(ReferenceRole::Reference)
            && hit.start_line == 4
            && hit.enclosing_symbol.as_deref() == Some("Method")
    }));
    assert!(references.hits.iter().any(|hit| {
        hit.symbol.as_deref() == Some("alpha")
            && hit.role == Some(ReferenceRole::Definition)
            && hit.start_line == 9
    }));

    for (heading, symbol, expected_range, expected_content) in [
        (
            Some("Method"),
            None,
            (3, 6),
            "\\subsection{Method}\nmethod \\cite{alpha}\n\\label{sec:method}\n\\input{sections/results}",
        ),
        (
            Some("\\subsection{Method}"),
            None,
            (3, 6),
            "\\subsection{Method}\nmethod \\cite{alpha}\n\\label{sec:method}\n\\input{sections/results}",
        ),
        (
            None,
            Some("sec:method"),
            (3, 6),
            "\\subsection{Method}\nmethod \\cite{alpha}\n\\label{sec:method}\n\\input{sections/results}",
        ),
        (
            None,
            Some("alpha"),
            (9, 10),
            "\\bibitem{alpha} Source.\n\\end{thebibliography}",
        ),
    ] {
        let read = services
            .read(ReadRequest {
                path: "paper.tex".into(),
                start_line: None,
                end_line: None,
                symbol: symbol.map(str::to_owned),
                heading: heading.map(str::to_owned),
                heading_occurrence: None,
                continuation_cursor: None,
                max_tokens: Some(2_000),
                expected_hash: None,
                delta: false,
                receipt_id: None,
                policy: leantoken::ReadPolicy::default(),
            })
            .await
            .expect("LaTeX structured read");
        assert_eq!(
            (read.target_start_line, read.target_end_line),
            expected_range
        );
        assert_eq!(
            read.content.as_deref().map(str::trim_end),
            Some(expected_content)
        );
    }
}
