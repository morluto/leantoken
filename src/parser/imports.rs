use super::*;

pub(super) fn process_imports_match(
    source: &str,
    query: &Query,
    qm: &QueryMatch,
    imports: &mut Vec<Import>,
) {
    let capture_names = query.capture_names();
    let mut python_module = None;
    let mut python_members = Vec::new();
    let mut python_wildcard = false;
    for cap in qm.captures {
        let cap_name = capture_names[cap.index as usize];
        let raw = node_text(source, cap.node);
        let raw = unquote(&raw);
        if raw.is_empty() {
            continue;
        }
        match cap_name {
            "raw" => push_import(imports, raw, cap.node.start_position().row + 1),
            "python_module" => {
                python_module = Some((raw.to_string(), cap.node.start_position().row + 1));
            }
            "python_member" => {
                let member = cap.node.child_by_field_name("name").unwrap_or(cap.node);
                python_members.push((node_text(source, member), member.start_position().row + 1));
            }
            "python_wildcard" => python_wildcard = true,
            _ => {}
        }
    }

    if let Some((module, line)) = python_module {
        if python_wildcard {
            push_import(imports, &module, line);
        } else if module.bytes().all(|byte| byte == b'.') {
            for (member, member_line) in python_members {
                push_import(imports, &format!("{module}{member}"), member_line);
            }
        } else {
            push_import(imports, &module, line);
            // `from package import member` may load either an attribute owned
            // by the package or the repository submodule `package.member`.
            // Persist both bounded syntax-derived alternatives instead of
            // erasing the member and manufacturing certainty downstream.
            for (member, member_line) in python_members {
                push_import(imports, &format!("{module}.{member}"), member_line);
            }
        }
    }
}

pub(super) fn push_import(imports: &mut Vec<Import>, raw_target: &str, line: usize) {
    imports.push(Import {
        raw_target: raw_target.to_string(),
        resolved_path: None,
        line,
    });
}

pub(super) fn deduplicate_imports(imports: &mut Vec<Import>) {
    let mut seen = HashSet::with_capacity(imports.len());
    // Decide before mutating so the set can borrow targets instead of cloning
    // every string solely for deduplication.
    let keep = imports
        .iter()
        .map(|import| seen.insert((import.line, import.raw_target.as_str())))
        .collect::<Vec<_>>();
    drop(seen);
    let mut keep = keep.into_iter();
    imports.retain(|_| keep.next().expect("one decision per import"));
}
