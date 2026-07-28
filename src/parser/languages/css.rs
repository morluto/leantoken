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
