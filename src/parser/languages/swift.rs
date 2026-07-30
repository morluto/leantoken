fn append_swift_structure(
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

        match node.kind() {
            "class_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    let kind = node
                        .child_by_field_name("declaration_kind")
                        .map_or("class", |kind| kind.kind());
                    push_structural_symbol(
                        source,
                        node,
                        node_text(source, name),
                        kind,
                        signature_from_node(source, node),
                        symbols,
                    );
                }
            }
            "protocol_declaration" => {
                push_swift_named_symbol(source, node, "interface", symbols);
            }
            "function_declaration" => {
                let kind = if swift_is_type_member(node) {
                    "method"
                } else {
                    "function"
                };
                push_swift_named_symbol(source, node, kind, symbols);
            }
            "protocol_function_declaration" => {
                push_swift_named_symbol(source, node, "method", symbols);
            }
            "property_declaration" | "protocol_property_declaration" => {
                if let Some(pattern) = node.child_by_field_name("name")
                    && let Some(name) = swift_first_identifier(pattern)
                {
                    push_structural_symbol(
                        source,
                        node,
                        node_text(source, name),
                        "property",
                        signature_from_node(source, node),
                        symbols,
                    );
                }
            }
            "init_declaration" => {
                push_structural_symbol(
                    source,
                    node,
                    "init".into(),
                    "constructor",
                    signature_from_node(source, node),
                    symbols,
                );
            }
            "deinit_declaration" => {
                push_structural_symbol(
                    source,
                    node,
                    "deinit".into(),
                    "destructor",
                    signature_from_node(source, node),
                    symbols,
                );
            }
            "subscript_declaration" => {
                push_structural_symbol(
                    source,
                    node,
                    "subscript".into(),
                    "method",
                    signature_from_node(source, node),
                    symbols,
                );
            }
            "enum_entry" => {
                append_swift_enum_entries(source, node, symbols);
            }
            "call_expression" => {
                if let Some(name) = swift_call_name(node) {
                    push_structural_reference(
                        node_text(source, name),
                        "call",
                        ReferenceRole::Reference,
                        name,
                        references,
                    );
                }
            }
            "import_declaration" => {
                let mut cursor = node.walk();
                if let Some(target) = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "identifier")
                {
                    push_import(
                        imports,
                        &node_text(source, target),
                        node.start_position().row + 1,
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

fn push_swift_named_symbol(
    source: &str,
    node: Node<'_>,
    kind: &str,
    symbols: &mut Vec<Symbol>,
) {
    if let Some(name) = node.child_by_field_name("name") {
        push_structural_symbol(
            source,
            node,
            node_text(source, name),
            kind,
            signature_from_node(source, node),
            symbols,
        );
    }
}

fn swift_is_type_member(node: Node<'_>) -> bool {
    let mut owner = node.parent();
    while let Some(candidate) = owner {
        match candidate.kind() {
            "class_body" | "enum_class_body" | "protocol_body" => return true,
            "function_body" => return false,
            _ => owner = candidate.parent(),
        }
    }
    false
}

fn swift_first_identifier(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(node.kind(), "simple_identifier" | "type_identifier") {
        return Some(node);
    }
    let mut pending = vec![node];
    while let Some(candidate) = pending.pop() {
        let mut cursor = candidate.walk();
        let children = candidate.named_children(&mut cursor).collect::<Vec<_>>();
        for child in &children {
            if matches!(child.kind(), "simple_identifier" | "type_identifier") {
                return Some(*child);
            }
        }
        pending.extend(children.into_iter().rev());
    }
    None
}

fn append_swift_enum_entries(source: &str, node: Node<'_>, symbols: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for name in node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "simple_identifier")
    {
        push_structural_symbol(
            source,
            node,
            node_text(source, name),
            "enum_member",
            signature_from_node(source, node),
            symbols,
        );
    }
}

fn swift_call_name(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let callee = node
        .named_children(&mut cursor)
        .find(|child| child.kind() != "call_suffix")?;
    swift_terminal_call_name(callee)
}

fn swift_terminal_call_name(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(node.kind(), "simple_identifier" | "type_identifier") {
        return Some(node);
    }
    if node.kind() == "navigation_expression" {
        return node
            .child_by_field_name("suffix")
            .and_then(|suffix| suffix.child_by_field_name("suffix"))
            .and_then(swift_terminal_call_name);
    }
    None
}
