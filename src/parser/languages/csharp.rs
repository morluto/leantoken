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
