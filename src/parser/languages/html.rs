#[derive(Clone)]
struct HtmlAttribute<'tree> {
    name: String,
    value: String,
    value_node: Node<'tree>,
}

fn append_html_structure(
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

        if matches!(node.kind(), "element" | "script_element" | "style_element")
            && let Some((tag_node, tag_name)) = html_tag(source, node)
        {
            let attributes = html_attributes(source, tag_node);
            append_html_element(
                source,
                node,
                tag_node,
                &tag_name,
                &attributes,
                symbols,
                references,
                imports,
            );
        }

        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_html_element(
    source: &str,
    owner: Node<'_>,
    tag_node: Node<'_>,
    tag_name: &str,
    attributes: &[HtmlAttribute<'_>],
    symbols: &mut Vec<Symbol>,
    references: &mut Vec<Reference>,
    imports: &mut Vec<Import>,
) {
    let id = html_attribute(attributes, "id");
    let data_attribute = attributes
        .iter()
        .find(|attribute| attribute.name.starts_with("data-") && !attribute.value.is_empty());
    let href = html_attribute(attributes, "href");
    let name = html_attribute(attributes, "name");

    if let Some(id) = id {
        push_structural_symbol(
            source,
            owner,
            format!("#{}", id.value),
            "html_id",
            signature_from_node(source, tag_node),
            symbols,
        );
        push_structural_reference(
            format!("#{}", id.value),
            "html_id",
            ReferenceRole::Definition,
            id.value_node,
            references,
        );
    } else if html_outline_tag(tag_name) {
        let element_name = if let Some(attribute) = data_attribute {
            format!("{tag_name}[{}={}]", attribute.name, attribute.value)
        } else if let Some(attribute) = name {
            format!("{tag_name}[name={}]", attribute.value)
        } else if let Some(attribute) = href {
            format!("{tag_name}[href={}]", attribute.value)
        } else {
            format!("<{tag_name}>")
        };
        let kind = if html_section_tag(tag_name) {
            "html_section"
        } else if matches!(tag_name, "script" | "style" | "link") {
            "html_resource"
        } else {
            "html_element"
        };
        push_structural_symbol(
            source,
            owner,
            element_name,
            kind,
            signature_from_node(source, tag_node),
            symbols,
        );
    }

    for attribute in attributes {
        if attribute.name.starts_with("data-") && !attribute.value.is_empty() {
            push_structural_reference(
                format!("{}={}", attribute.name, attribute.value),
                "html_data_attribute",
                ReferenceRole::Reference,
                attribute.value_node,
                references,
            );
        }
        if attribute.name == "href" && attribute.value.starts_with('#') && attribute.value.len() > 1
        {
            push_structural_reference(
                attribute.value.clone(),
                "html_anchor",
                ReferenceRole::Reference,
                attribute.value_node,
                references,
            );
        }
        if attribute.name == "for" && !attribute.value.is_empty() {
            push_structural_reference(
                format!("#{}", attribute.value),
                "html_id",
                ReferenceRole::Reference,
                attribute.value_node,
                references,
            );
        }
    }

    match tag_name {
        "script" => {
            if let Some(src) = html_attribute(attributes, "src") {
                push_import(imports, &src.value, src.value_node.start_position().row + 1);
            }
        }
        "link" => {
            let rel = html_attribute(attributes, "rel")
                .map(|attribute| attribute.value.to_ascii_lowercase());
            if rel.as_deref().is_some_and(|rel| {
                rel.split_ascii_whitespace()
                    .any(|value| matches!(value, "stylesheet" | "modulepreload" | "preload"))
            }) && let Some(href) = href
            {
                push_import(
                    imports,
                    &href.value,
                    href.value_node.start_position().row + 1,
                );
            }
        }
        _ => {}
    }
}

fn html_tag<'tree>(source: &str, owner: Node<'tree>) -> Option<(Node<'tree>, String)> {
    let mut cursor = owner.walk();
    let tag = owner
        .named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "start_tag" | "self_closing_tag"))?;
    let mut cursor = tag.walk();
    let name = tag
        .named_children(&mut cursor)
        .find(|child| child.kind() == "tag_name")
        .map(|child| node_text(source, child).to_ascii_lowercase())?;
    Some((tag, name))
}

fn html_attributes<'tree>(source: &str, tag: Node<'tree>) -> Vec<HtmlAttribute<'tree>> {
    let mut cursor = tag.walk();
    tag.named_children(&mut cursor)
        .filter(|child| child.kind() == "attribute")
        .filter_map(|attribute| {
            let mut cursor = attribute.walk();
            let mut children = attribute.named_children(&mut cursor);
            let name = children.next()?;
            let value = children.next()?;
            let value_node = if value.kind() == "quoted_attribute_value" {
                let mut cursor = value.walk();
                value
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "attribute_value")
                    .unwrap_or(value)
            } else {
                value
            };
            Some(HtmlAttribute {
                name: node_text(source, name).to_ascii_lowercase(),
                value: node_text(source, value_node)
                    .trim_matches(['\'', '"'])
                    .to_string(),
                value_node,
            })
        })
        .collect()
}

fn html_attribute<'attributes, 'tree>(
    attributes: &'attributes [HtmlAttribute<'tree>],
    name: &str,
) -> Option<&'attributes HtmlAttribute<'tree>> {
    attributes
        .iter()
        .find(|attribute| attribute.name == name && !attribute.value.is_empty())
}

fn html_section_tag(tag: &str) -> bool {
    matches!(
        tag,
        "main" | "nav" | "section" | "article" | "aside" | "header" | "footer"
    )
}

fn html_outline_tag(tag: &str) -> bool {
    html_section_tag(tag)
        || matches!(
            tag,
            "a" | "button"
                | "dialog"
                | "form"
                | "input"
                | "select"
                | "textarea"
                | "script"
                | "style"
                | "link"
        )
}
