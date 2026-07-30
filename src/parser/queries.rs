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

// Pinned verbatim to fwcd/tree-sitter-kotlin
// c10ad83a66c76855e006496db3bdb002afc49203/queries/tags.scm for the
// research-only Kotlin 0.4.0 evaluation.
const KOTLIN_TAGS_QUERY: &str = r#"
; Classes
(class_declaration
  (type_identifier) @name) @definition.class

; Objects
(object_declaration
  (type_identifier) @name) @definition.class

; Functions (top-level and member)
(function_declaration
  (simple_identifier) @name) @definition.function

; Properties
(property_declaration
  (variable_declaration
    (simple_identifier) @name)) @definition.constant

; Enum entries
(enum_entry
  (simple_identifier) @name) @definition.constant

; Type aliases
(type_alias
  (type_identifier) @name) @definition.type

; Companion objects (only named ones)
(companion_object
  (type_identifier) @name) @definition.class

; Function calls
(call_expression
  (simple_identifier) @name) @reference.call

; Method calls via navigation
(call_expression
  (navigation_expression
    (navigation_suffix
      (simple_identifier) @name))) @reference.call

; Constructor invocations (class references)
(constructor_invocation
  (user_type
    (type_identifier) @name)) @reference.class
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
