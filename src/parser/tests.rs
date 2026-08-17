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
fn import_deduplication_preserves_first_occurrence_order() {
    let mut imports = Vec::new();
    for (target, line) in [("alpha", 1), ("beta", 2), ("alpha", 1), ("alpha", 3)] {
        push_import(&mut imports, target, line);
    }
    deduplicate_imports(&mut imports);
    assert_eq!(
        imports
            .iter()
            .map(|import| (import.raw_target.as_str(), import.line))
            .collect::<Vec<_>>(),
        vec![("alpha", 1), ("beta", 2), ("alpha", 3)]
    );
}

#[test]
fn c_family_indexes_named_calls_without_declaration_false_positives() -> Result<()> {
    let source = "\
static int target(int value);\n\
static int target(int value) { return value; }\n\
struct Hooks { int (*target)(int); };\n\
static int caller(struct Hooks *hooks) {\n\
    const char *literal = \"target()\";\n\
    // target(0);\n\
    int direct = target(1);\n\
    int member = hooks->target(2);\n\
    return direct + member + (literal[0] == 't');\n\
}\n";
    for language in ["c", "cpp"] {
        let output = parse_language(language, source)?;

        assert!(output.structurally_complete, "{language}");
        assert_eq!(
            output
                .references
                .iter()
                .map(|reference| (
                    reference.name.as_str(),
                    reference.kind.as_str(),
                    reference.enclosing_symbol.as_deref(),
                    reference.start_line,
                ))
                .collect::<Vec<_>>(),
            vec![
                ("target", "call", Some("caller"), 7),
                ("target", "call", Some("caller"), 8),
            ],
            "{language}"
        );
        assert!(output.references.iter().all(|reference| {
            &source[reference.start_byte..reference.end_byte] == "target"
                && reference.role == ReferenceRole::Reference
        }));
    }

    let macro_prototype = "\
#ifdef __cplusplus\n\
extern \"C\" {\n\
#endif\n\
API(void) declared(int value);\n\
WRAP(target());\n\
#ifdef __cplusplus\n\
}\n\
#endif\n";
    let recovered = parse_language("cpp", macro_prototype)?;
    assert!(!recovered.structurally_complete);
    assert!(recovered.references.is_empty());
    Ok(())
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
                    && expected_owner
                        .is_none_or(|owner| reference.enclosing_symbol.as_deref() == Some(owner))
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
fn latex_single_pass_indexes_structure_references_and_imports() -> Result<()> {
    let source = r#"\section{Introduction}
intro \cite{smith2024, doe2023}.
\subsection[Short]{Method}
method \label{sec:method}
\begin{figure}
\caption[Short]{Pipeline}
\label{fig:pipeline}
\end{figure}
\input{sections/results}
\include{appendix}
\subsubsection{Details}
See \cref{fig:pipeline,sec:method}.
\paragraph{Limits}
text
\section{References}
\begin{thebibliography}{9}
\bibitem{smith2024} Smith.
continued.
\bibitem[Doe]{doe2023} Doe.
\end{thebibliography}
"#;
    let output = parse("paper.tex", source)?;

    assert_eq!(output.language.as_deref(), Some("latex"));
    assert!(output.structurally_complete);
    assert_eq!(
        import_targets(&output),
        vec!["sections/results", "appendix"]
    );

    for (name, kind, start_line, end_line) in [
        ("Introduction", "latex_section", 1, 14),
        ("Method", "latex_subsection", 3, 14),
        ("Details", "latex_subsubsection", 11, 14),
        ("Limits", "latex_paragraph", 13, 14),
        ("References", "latex_section", 15, 20),
        ("figure", "latex_environment", 5, 8),
        ("Pipeline", "latex_caption", 5, 8),
        ("fig:pipeline", "latex_label", 5, 8),
        ("sec:method", "latex_label", 3, 14),
        ("smith2024", "latex_bibitem", 17, 18),
        ("doe2023", "latex_bibitem", 19, 20),
    ] {
        assert!(
            output.symbols.iter().any(|symbol| {
                symbol.name == name
                    && symbol.kind == kind
                    && symbol.start_line == start_line
                    && symbol.end_line == end_line
            }),
            "missing {kind} {name} at {start_line}-{end_line}: {:?}",
            output.symbols
        );
    }

    for (name, kind, role) in [
        ("smith2024", "latex_cite", ReferenceRole::Reference),
        ("doe2023", "latex_cite", ReferenceRole::Reference),
        ("fig:pipeline", "latex_ref", ReferenceRole::Reference),
        ("sec:method", "latex_ref", ReferenceRole::Reference),
        ("fig:pipeline", "latex_label", ReferenceRole::Definition),
        ("sec:method", "latex_label", ReferenceRole::Definition),
        ("smith2024", "latex_bibitem", ReferenceRole::Definition),
        ("doe2023", "latex_bibitem", ReferenceRole::Definition),
    ] {
        assert!(
            output.references.iter().any(|reference| {
                reference.name == name && reference.kind == kind && reference.role == role
            }),
            "missing {role:?} {kind} {name}: {:?}",
            output.references
        );
    }
    Ok(())
}

#[test]
fn latex_recovers_bounded_structure_without_indexing_comments_or_verbatim() -> Result<()> {
    let source = r#"\section{Visible}
% \section{Hidden}
\begin{verbatim}
\section{Also hidden}
\end{verbatim}
\section{Recovered}
\begin{figure}
\label{fig:open}
"#;
    let output = parse("paper.tex", source)?;

    assert!(!output.structurally_complete);
    assert!(output.symbols.iter().any(|symbol| symbol.name == "Visible"));
    assert!(
        output
            .symbols
            .iter()
            .any(|symbol| symbol.name == "Recovered")
    );
    assert!(!output.symbols.iter().any(|symbol| symbol.name == "Hidden"));
    assert!(
        !output
            .symbols
            .iter()
            .any(|symbol| symbol.name == "Also hidden")
    );
    assert!(output.symbols.iter().any(|symbol| {
        symbol.name == "fig:open"
            && symbol.kind == "latex_label"
            && symbol.start_line == 7
            && symbol.end_line == 8
    }));

    let commented = parse(
        "commented.tex",
        "\\section{Visible % a commented closing brace }\nTitle}\n",
    )?;
    assert!(commented.structurally_complete);
    assert!(commented.symbols.iter().any(|symbol| {
        symbol.name == "Visible Title" && symbol.start_line == 1 && symbol.end_line == 2
    }));

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        parse_with_cancellation("cancelled.tex", "\\section{Never}", &cancellation),
        Err(Error::Cancelled)
    ));
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
    assert!(
        imports.contains(&"collections.defaultdict"),
        "imports: {imports:?}"
    );

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
        vec![
            "pkg.mod",
            "pkg.mod.thing",
            "pkg.mod.other",
            ".helpers",
            ".tools",
            ".aliased",
            "..core",
            "..core.api"
        ]
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
    assert!(source[medicines.start_byte..medicines.end_byte].starts_with("clinicMedicines = ["));
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
