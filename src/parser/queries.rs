pub(super) const RUST_DEFS_QUERY: &str = r#"
(const_item
  name: (identifier) @name) @definition.constant

(static_item
  name: (identifier) @name) @definition.static
"#;

pub(super) const GO_DEFS_QUERY: &str = r#"
(package_clause "package" (package_identifier) @name) @definition.module

(var_declaration (var_spec name: (identifier) @name)) @definition.variable

(const_declaration (const_spec name: (identifier) @name)) @definition.constant
"#;

pub(super) const C_FAMILY_REFS_QUERY: &str = r#"
(call_expression
  function: (identifier) @name) @reference.call

(call_expression
  function: (field_expression
    field: (field_identifier) @name)) @reference.call
"#;

pub(super) const PHP_REFS_QUERY: &str = r#"
(function_call_expression
  function: (name) @name) @reference.call
"#;

pub(super) const RUST_IMPORT_QUERY: &str = r#"
(use_declaration
  argument: (_) @raw) @import
"#;

pub(super) const PYTHON_IMPORT_QUERY: &str = r#"
(import_statement
  name: (_) @raw) @import

(import_from_statement
  module_name: (_) @python_module
  name: (_) @python_member) @import

(import_from_statement
  module_name: (_) @python_module
  (wildcard_import) @python_wildcard) @import
"#;

pub(super) const JS_IMPORT_QUERY: &str = r#"
(import_statement
  source: (string) @raw) @import

(export_statement
  source: (string) @raw) @import

(call_expression
  function: (identifier) @fn
  arguments: (arguments (string) @raw)
  (#eq? @fn "require")) @import
"#;

pub(super) const GO_IMPORT_QUERY: &str = r#"
(import_spec
  path: (interpreted_string_literal) @raw) @import

(import_spec
  path: (raw_string_literal) @raw) @import
"#;
