use super::*;

pub(super) fn append_javascript_bindings(
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

pub(super) fn append_javascript_declaration(
    source: &str,
    declaration: Node<'_>,
    symbols: &mut Vec<Symbol>,
) {
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

pub(super) fn javascript_export_is_default(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == "default")
}

pub(super) fn javascript_is_data_expression(node: Node<'_>) -> bool {
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

pub(super) fn push_javascript_symbol(
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
