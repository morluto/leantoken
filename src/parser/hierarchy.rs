fn push_structural_symbol(
    source: &str,
    node: Node<'_>,
    name: String,
    kind: &str,
    signature: Option<String>,
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
        signature: signature.or_else(|| signature_from_node(source, node)),
        start_line,
        end_line,
        start_byte,
        end_byte,
    });
}

fn push_structural_reference(
    name: String,
    kind: &str,
    role: ReferenceRole,
    node: Node<'_>,
    references: &mut Vec<Reference>,
) {
    if name.is_empty() {
        return;
    }
    let (start_line, end_line, start_byte, end_byte) = range_from_node(node);
    references.push(Reference {
        name,
        kind: kind.into(),
        role,
        enclosing_symbol: None,
        start_line,
        end_line,
        start_byte,
        end_byte,
    });
}

fn canonical_definition(
    language: &str,
    source: &str,
    kind: &str,
    node: Node<'_>,
) -> (String, Option<String>) {
    match (language, node.kind()) {
        ("rust", "function_item") => rust_function_identity(source, node),
        ("go", "method_declaration") => ("method".into(), go_method_owner(source, node)),
        _ => (kind.to_string(), None),
    }
}

fn rust_function_identity(source: &str, node: Node<'_>) -> (String, Option<String>) {
    let Some(declarations) = node
        .parent()
        .filter(|parent| parent.kind() == "declaration_list")
    else {
        return ("function".into(), None);
    };
    match declarations.parent() {
        Some(owner) if owner.kind() == "impl_item" => {
            let owner = owner
                .child_by_field_name("type")
                .and_then(|node| base_type_name(source, node));
            ("method".into(), owner)
        }
        Some(owner) if owner.kind() == "trait_item" => {
            let owner = owner
                .child_by_field_name("name")
                .and_then(|node| base_type_name(source, node));
            ("method".into(), owner)
        }
        _ => ("function".into(), None),
    }
}

fn go_method_owner(source: &str, node: Node<'_>) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    receiver.named_children(&mut cursor).find_map(|parameter| {
        parameter
            .child_by_field_name("type")
            .and_then(|node| base_type_name(source, node))
    })
}

fn base_type_name(source: &str, node: Node<'_>) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "field_identifier" | "primitive_type" => {
            let name = node_text(source, node);
            (!name.is_empty()).then_some(name)
        }
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|node| base_type_name(source, node)),
        "scoped_type_identifier" | "qualified_type" => node
            .child_by_field_name("name")
            .and_then(|node| base_type_name(source, node)),
        "pointer_type" | "reference_type" | "parenthesized_type" | "bracketed_type"
        | "abstract_type" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find_map(|child| base_type_name(source, child))
        }
        _ => node
            .child_by_field_name("type")
            .and_then(|node| base_type_name(source, node)),
    }
}

fn definition_extent(node: Node<'_>) -> Node<'_> {
    if node.kind() != "function_declarator" {
        return node;
    }
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "function_definition" {
            return parent;
        }
        if matches!(parent.kind(), "declaration" | "translation_unit") {
            break;
        }
        current = parent;
    }
    node
}

fn signature_from_node(source: &str, node: Node) -> Option<String> {
    const MAX_SIGNATURE_CHARS: usize = 512;

    let end = first_body_start(node).unwrap_or_else(|| node.end_byte());
    let raw = source.get(node.start_byte()..end)?.trim();
    if raw.is_empty() {
        return None;
    }

    let mut compact = String::with_capacity(raw.len().min(MAX_SIGNATURE_CHARS));
    for part in raw.split_whitespace() {
        if !compact.is_empty() {
            compact.push(' ');
        }
        let remaining = MAX_SIGNATURE_CHARS.saturating_sub(compact.chars().count());
        if remaining == 0 {
            break;
        }
        compact.extend(part.chars().take(remaining));
    }
    (!compact.is_empty()).then_some(compact)
}

fn first_body_start(node: Node<'_>) -> Option<usize> {
    let mut pending = vec![node];
    let mut earliest = None;
    while let Some(current) = pending.pop() {
        if let Some(body) = current.child_by_field_name("body") {
            earliest = Some(earliest.map_or(body.start_byte(), |start: usize| {
                start.min(body.start_byte())
            }));
            continue;
        }
        let mut cursor = current.walk();
        pending.extend(current.named_children(&mut cursor));
    }
    earliest
}

fn compute_symbol_parents(symbols: &mut [Symbol]) {
    if symbols.is_empty() {
        return;
    }

    let mut indices: Vec<usize> = (0..symbols.len()).collect();
    indices.sort_by(|&a, &b| {
        symbols[a]
            .start_byte
            .cmp(&symbols[b].start_byte)
            .then_with(|| symbols[b].end_byte.cmp(&symbols[a].end_byte))
    });

    let mut stack: Vec<usize> = Vec::new();
    for i in indices {
        while let Some(&top) = stack.last() {
            if strictly_contains(&symbols[top], &symbols[i]) {
                break;
            }
            stack.pop();
        }
        if symbols[i].parent.is_none() {
            symbols[i].parent = stack.last().map(|&top| symbols[top].name.clone());
        }
        stack.push(i);
    }

    symbols.sort_by(|a, b| {
        a.start_byte
            .cmp(&b.start_byte)
            .then_with(|| a.end_byte.cmp(&b.end_byte))
    });
}

fn deduplicate_symbols(symbols: &mut Vec<Symbol>) {
    let mut seen = HashSet::with_capacity(symbols.len());
    symbols.retain(|symbol| {
        seen.insert((
            symbol.kind.clone(),
            symbol.name.clone(),
            symbol.start_byte,
            symbol.end_byte,
        ))
    });
}

fn strictly_contains(parent: &Symbol, child: &Symbol) -> bool {
    parent.start_byte <= child.start_byte
        && parent.end_byte >= child.end_byte
        && (parent.start_byte < child.start_byte || parent.end_byte > child.end_byte)
}

fn compute_reference_enclosing(symbols: &[Symbol], references: &mut [Reference]) {
    if symbols.is_empty() || references.is_empty() {
        references.sort_by(|a, b| {
            a.start_byte
                .cmp(&b.start_byte)
                .then_with(|| a.end_byte.cmp(&b.end_byte))
        });
        return;
    }

    // LaTeX labels and captions intentionally reuse their owner's full range
    // so exact symbol reads return useful evidence. They are navigation aliases,
    // not lexical owners for references inside that range.
    let mut sym_indices: Vec<usize> = (0..symbols.len())
        .filter(|index| {
            !matches!(
                symbols[*index].kind.as_str(),
                "latex_label" | "latex_caption"
            )
        })
        .collect();
    sym_indices.sort_by(|&a, &b| {
        symbols[a]
            .start_byte
            .cmp(&symbols[b].start_byte)
            .then_with(|| symbols[b].end_byte.cmp(&symbols[a].end_byte))
    });

    let mut ref_indices: Vec<usize> = (0..references.len()).collect();
    ref_indices.sort_by_key(|&i| references[i].start_byte);

    let mut sym_idx = 0;
    let mut stack: Vec<usize> = Vec::new();

    for ri in ref_indices {
        let ref_start = references[ri].start_byte;
        let ref_end = references[ri].end_byte;

        while let Some(&top) = stack.last() {
            if symbols[top].end_byte <= ref_start {
                stack.pop();
            } else {
                break;
            }
        }

        while sym_idx < sym_indices.len() {
            let si = sym_indices[sym_idx];
            if symbols[si].start_byte > ref_start {
                break;
            }
            if symbols[si].end_byte > ref_start {
                stack.push(si);
            }
            sym_idx += 1;
        }

        while let Some(&top) = stack.last() {
            if symbols[top].end_byte < ref_end {
                stack.pop();
            } else {
                break;
            }
        }

        if let Some(&top) = stack.last() {
            references[ri].enclosing_symbol = Some(symbols[top].name.clone());
        }
    }

    references.sort_by(|a, b| {
        a.start_byte
            .cmp(&b.start_byte)
            .then_with(|| a.end_byte.cmp(&b.end_byte))
    });
}

fn range_from_node(node: Node) -> (usize, usize, usize, usize) {
    let start = node.start_position();
    let end = node.end_position();
    (
        start.row + 1,
        end.row + 1,
        node.start_byte(),
        node.end_byte(),
    )
}

fn node_text(source: &str, node: Node) -> String {
    let bytes = source.as_bytes();
    let start = node.start_byte();
    let end = node.end_byte();
    if start <= end && end <= bytes.len() {
        String::from_utf8_lossy(&bytes[start..end]).into_owned()
    } else {
        String::new()
    }
}

fn unquote(s: &str) -> &str {
    let mut chars = s.chars();
    if let (Some(first), Some(last)) = (chars.next(), chars.next_back())
        && first == last
        && (first == '"' || first == '\'' || first == '`')
    {
        return chars.as_str();
    }
    s
}
