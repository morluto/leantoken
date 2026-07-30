use super::*;

#[derive(Debug)]
pub(super) struct LatexSection {
    level: usize,
    command: String,
    title: String,
    start_byte: usize,
}

#[derive(Debug)]
pub(super) struct LatexCommand {
    name: String,
    argument_start: usize,
    argument_end: usize,
    command_start: usize,
    command_end: usize,
    owner_section: Option<usize>,
    owner_environment_start: Option<usize>,
}

#[derive(Debug)]
pub(super) struct LatexEnvironment {
    name: String,
    start_byte: usize,
    end_byte: usize,
}

#[derive(Debug)]
pub(super) struct OpenLatexEnvironment {
    name: String,
    start_byte: usize,
}

// The command dispatcher stays flat so every recognized LaTeX construct shares
// one source pass and one structural-completeness state.
#[allow(clippy::cognitive_complexity)]
pub(super) fn parse_latex(
    source: &str,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<ParseOutput> {
    let bytes = source.as_bytes();
    let starts = line_starts(source);
    let mut sections = Vec::<LatexSection>::new();
    let mut section_stack = Vec::<usize>::new();
    let mut labels = Vec::new();
    let mut captions = Vec::new();
    let mut bibitems = Vec::new();
    let mut references = Vec::new();
    let mut imports = Vec::new();
    let mut environments = Vec::new();
    let mut environment_stack = Vec::<OpenLatexEnvironment>::new();
    let mut structurally_complete = true;
    let mut offset = 0;

    while offset < bytes.len() {
        if is_cancelled() {
            return Err(Error::Cancelled);
        }
        match bytes[offset] {
            b'%' => {
                offset = bytes[offset..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |relative| offset + relative + 1);
            }
            b'\\' => {
                let command_start = offset;
                let Some((command, after_command)) = latex_command_name(source, offset) else {
                    offset = (offset + 2).min(bytes.len());
                    continue;
                };
                offset = after_command;

                if command == "verb" {
                    offset = skip_latex_verb(source, offset);
                    continue;
                }

                let mut argument_offset = offset;
                if latex_command_allows_optional_arguments(command) {
                    match skip_latex_optional_arguments(source, argument_offset) {
                        Some(next) => argument_offset = next,
                        None => {
                            structurally_complete = false;
                            offset = after_command;
                            continue;
                        }
                    }
                }
                let Some((argument_start, argument_end, command_end)) =
                    latex_braced_argument(source, argument_offset)
                else {
                    if latex_command_requires_argument(command) {
                        structurally_complete = false;
                    }
                    continue;
                };
                let argument = latex_argument_value(source, argument_start, argument_end);

                match command {
                    "section" | "subsection" | "subsubsection" | "paragraph"
                        if !argument.is_empty() =>
                    {
                        let level = latex_section_level(command);
                        while section_stack
                            .last()
                            .is_some_and(|index| sections[*index].level >= level)
                        {
                            section_stack.pop();
                        }
                        sections.push(LatexSection {
                            level,
                            command: command.to_owned(),
                            title: argument.clone(),
                            start_byte: command_start,
                        });
                        section_stack.push(sections.len() - 1);
                    }
                    "begin" => {
                        if argument.is_empty() {
                            structurally_complete = false;
                        } else if latex_verbatim_environment(&argument) {
                            let closing = format!("\\end{{{argument}}}");
                            if let Some(relative) = source[command_end..].find(&closing) {
                                let end_byte = command_end + relative + closing.len();
                                environments.push(LatexEnvironment {
                                    name: argument.clone(),
                                    start_byte: command_start,
                                    end_byte,
                                });
                                offset = end_byte;
                                continue;
                            }
                            structurally_complete = false;
                            environments.push(LatexEnvironment {
                                name: argument.clone(),
                                start_byte: command_start,
                                end_byte: source.len(),
                            });
                            break;
                        } else {
                            environment_stack.push(OpenLatexEnvironment {
                                name: argument.clone(),
                                start_byte: command_start,
                            });
                        }
                    }
                    "end" => {
                        if environment_stack
                            .last()
                            .is_some_and(|environment| environment.name == argument)
                        {
                            let environment = environment_stack.pop().expect("checked non-empty");
                            environments.push(LatexEnvironment {
                                name: environment.name,
                                start_byte: environment.start_byte,
                                end_byte: command_end,
                            });
                        } else {
                            structurally_complete = false;
                        }
                    }
                    "label" if !argument.is_empty() => labels.push(LatexCommand {
                        name: argument.clone(),
                        argument_start,
                        argument_end,
                        command_start,
                        command_end,
                        owner_section: section_stack.last().copied(),
                        owner_environment_start: environment_stack
                            .last()
                            .map(|environment| environment.start_byte),
                    }),
                    "caption" if !argument.is_empty() => captions.push(LatexCommand {
                        name: argument.clone(),
                        argument_start,
                        argument_end,
                        command_start,
                        command_end,
                        owner_section: section_stack.last().copied(),
                        owner_environment_start: environment_stack
                            .last()
                            .map(|environment| environment.start_byte),
                    }),
                    "bibitem" if !argument.is_empty() => bibitems.push(LatexCommand {
                        name: argument.clone(),
                        argument_start,
                        argument_end,
                        command_start,
                        command_end,
                        owner_section: section_stack.last().copied(),
                        owner_environment_start: environment_stack
                            .last()
                            .map(|environment| environment.start_byte),
                    }),
                    "input" | "include" if !argument.is_empty() => imports.push(Import {
                        raw_target: argument,
                        resolved_path: None,
                        line: byte_to_line(&starts, source.len(), command_start),
                    }),
                    name if latex_citation_command(name) => append_latex_references(
                        source,
                        &starts,
                        argument_start,
                        argument_end,
                        "latex_cite",
                        ReferenceRole::Reference,
                        &mut references,
                    ),
                    name if latex_reference_command(name) => append_latex_references(
                        source,
                        &starts,
                        argument_start,
                        argument_end,
                        "latex_ref",
                        ReferenceRole::Reference,
                        &mut references,
                    ),
                    _ => {}
                }
                offset = command_end;
            }
            _ => offset += 1,
        }
    }

    if !environment_stack.is_empty() {
        structurally_complete = false;
        environments.extend(
            environment_stack
                .into_iter()
                .map(|environment| LatexEnvironment {
                    name: environment.name,
                    start_byte: environment.start_byte,
                    end_byte: source.len(),
                }),
        );
    }

    let mut section_ranges = vec![(0usize, source.len()); sections.len()];
    let mut open_sections = Vec::<usize>::new();
    for (index, section) in sections.iter().enumerate() {
        while open_sections
            .last()
            .is_some_and(|open| sections[*open].level >= section.level)
        {
            let closed = open_sections.pop().expect("checked non-empty");
            section_ranges[closed].1 = section.start_byte;
        }
        section_ranges[index].0 = section.start_byte;
        open_sections.push(index);
    }
    let environment_ranges = environments
        .iter()
        .map(|environment| {
            (
                environment.start_byte,
                (environment.name.as_str(), environment.end_byte),
            )
        })
        .collect::<HashMap<_, _>>();

    let mut symbols = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        let (start_byte, end_byte) = section_ranges[index];
        symbols.push(latex_symbol(
            source,
            &starts,
            section.title.clone(),
            format!("latex_{}", section.command),
            Some(format!("\\{}{{{}}}", section.command, section.title)),
            start_byte,
            end_byte,
        ));
    }
    for environment in &environments {
        symbols.push(latex_symbol(
            source,
            &starts,
            environment.name.clone(),
            "latex_environment".into(),
            Some(format!("\\begin{{{}}}", environment.name)),
            environment.start_byte,
            environment.end_byte,
        ));
    }
    for label in &labels {
        let (start_byte, end_byte) = latex_owner_range(
            label.command_start,
            label.command_end,
            label.owner_section,
            label.owner_environment_start,
            &section_ranges,
            &environment_ranges,
        );
        symbols.push(latex_symbol(
            source,
            &starts,
            label.name.clone(),
            "latex_label".into(),
            Some(format!("\\label{{{}}}", label.name)),
            start_byte,
            end_byte,
        ));
        append_latex_references(
            source,
            &starts,
            label.argument_start,
            label.argument_end,
            "latex_label",
            ReferenceRole::Definition,
            &mut references,
        );
    }
    for caption in &captions {
        let (start_byte, end_byte) = latex_owner_range(
            caption.command_start,
            caption.command_end,
            caption.owner_section,
            caption.owner_environment_start,
            &section_ranges,
            &environment_ranges,
        );
        symbols.push(latex_symbol(
            source,
            &starts,
            caption.name.clone(),
            "latex_caption".into(),
            Some(format!("\\caption{{{}}}", caption.name)),
            start_byte,
            end_byte,
        ));
    }
    for (index, bibitem) in bibitems.iter().enumerate() {
        let enclosing_end = bibitem
            .owner_environment_start
            .and_then(|start| environment_ranges.get(&start))
            .filter(|(name, _)| *name == "thebibliography")
            .map_or(source.len(), |(_, end)| *end);
        let end_byte = bibitems
            .get(index + 1)
            .map_or(enclosing_end, |next| next.command_start.min(enclosing_end));
        symbols.push(latex_symbol(
            source,
            &starts,
            bibitem.name.clone(),
            "latex_bibitem".into(),
            Some(format!("\\bibitem{{{}}}", bibitem.name)),
            bibitem.command_start,
            end_byte,
        ));
        append_latex_references(
            source,
            &starts,
            bibitem.argument_start,
            bibitem.argument_end,
            "latex_bibitem",
            ReferenceRole::Definition,
            &mut references,
        );
    }

    deduplicate_symbols(&mut symbols);
    compute_symbol_parents(&mut symbols);
    compute_reference_enclosing(&symbols, &mut references);
    imports.sort_by(|left, right| {
        left.line
            .cmp(&right.line)
            .then_with(|| left.raw_target.cmp(&right.raw_target))
    });
    imports.dedup_by(|left, right| left.line == right.line && left.raw_target == right.raw_target);

    Ok(ParseOutput {
        language: Some("latex".into()),
        structurally_complete,
        symbols,
        references,
        imports,
    })
}

fn latex_command_name(source: &str, offset: usize) -> Option<(&str, usize)> {
    let bytes = source.as_bytes();
    let mut end = offset.checked_add(1)?;
    if end >= bytes.len() || !matches!(bytes[end], b'a'..=b'z' | b'A'..=b'Z' | b'@') {
        return None;
    }
    while end < bytes.len() && matches!(bytes[end], b'a'..=b'z' | b'A'..=b'Z' | b'@') {
        end += 1;
    }
    let name = &source[offset + 1..end];
    if bytes.get(end) == Some(&b'*') {
        end += 1;
    }
    Some((name, end))
}

fn latex_braced_argument(source: &str, offset: usize) -> Option<(usize, usize, usize)> {
    let bytes = source.as_bytes();
    let mut open = offset;
    while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
        open += 1;
    }
    if bytes.get(open) != Some(&b'{') {
        return None;
    }
    let mut depth = 1usize;
    let mut cursor = open + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            b'%' => {
                cursor = bytes[cursor..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |relative| cursor + relative + 1);
            }
            b'{' => {
                depth += 1;
                cursor += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open + 1, cursor, cursor + 1));
                }
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    None
}

fn skip_latex_optional_arguments(source: &str, offset: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut cursor = offset;
    loop {
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'[') {
            return Some(cursor);
        }
        let mut depth = 1usize;
        cursor += 1;
        while cursor < bytes.len() && depth > 0 {
            match bytes[cursor] {
                b'\\' => cursor = (cursor + 2).min(bytes.len()),
                b'%' => {
                    cursor = bytes[cursor..]
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .map_or(bytes.len(), |relative| cursor + relative + 1);
                }
                b'[' => {
                    depth += 1;
                    cursor += 1;
                }
                b']' => {
                    depth -= 1;
                    cursor += 1;
                }
                _ => cursor += 1,
            }
        }
        if depth > 0 {
            return None;
        }
    }
}

fn latex_argument_value(source: &str, start: usize, end: usize) -> String {
    let bytes = source.as_bytes();
    let mut value = Vec::with_capacity(end.saturating_sub(start));
    let mut cursor = start;
    while cursor < end {
        match bytes[cursor] {
            b'\\' if cursor + 1 < end => {
                value.extend_from_slice(&bytes[cursor..cursor + 2]);
                cursor += 2;
            }
            b'%' => {
                cursor = bytes[cursor..end]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(end, |relative| cursor + relative + 1);
            }
            byte => {
                value.push(byte);
                cursor += 1;
            }
        }
    }
    String::from_utf8(value)
        .expect("subsequence of UTF-8 source remains UTF-8")
        .trim()
        .to_owned()
}

fn skip_latex_verb(source: &str, offset: usize) -> usize {
    let bytes = source.as_bytes();
    let Some(&delimiter) = bytes.get(offset) else {
        return offset;
    };
    if delimiter.is_ascii_whitespace() || delimiter == b'*' {
        return offset + 1;
    }
    bytes[offset + 1..]
        .iter()
        .position(|byte| *byte == delimiter)
        .map_or(bytes.len(), |relative| offset + relative + 2)
}

fn latex_command_allows_optional_arguments(command: &str) -> bool {
    matches!(
        command,
        "section"
            | "subsection"
            | "subsubsection"
            | "paragraph"
            | "caption"
            | "bibitem"
            | "cite"
            | "citep"
            | "citet"
            | "citeauthor"
            | "citeyear"
            | "parencite"
            | "textcite"
            | "autocite"
            | "footcite"
    )
}

fn latex_command_requires_argument(command: &str) -> bool {
    matches!(
        command,
        "section"
            | "subsection"
            | "subsubsection"
            | "paragraph"
            | "label"
            | "caption"
            | "bibitem"
            | "cite"
            | "citep"
            | "citet"
            | "citeauthor"
            | "citeyear"
            | "parencite"
            | "textcite"
            | "autocite"
            | "footcite"
            | "nocite"
            | "ref"
            | "eqref"
            | "pageref"
            | "autoref"
            | "cref"
            | "Cref"
            | "nameref"
            | "vref"
            | "input"
            | "include"
            | "begin"
            | "end"
    )
}

fn latex_section_level(command: &str) -> usize {
    match command {
        "section" => 1,
        "subsection" => 2,
        "subsubsection" => 3,
        "paragraph" => 4,
        _ => unreachable!("only section commands are passed"),
    }
}

fn latex_verbatim_environment(name: &str) -> bool {
    matches!(name, "verbatim" | "Verbatim" | "lstlisting" | "minted")
}

fn latex_citation_command(name: &str) -> bool {
    matches!(
        name,
        "cite"
            | "citep"
            | "citet"
            | "citeauthor"
            | "citeyear"
            | "parencite"
            | "textcite"
            | "autocite"
            | "footcite"
            | "nocite"
    )
}

fn latex_reference_command(name: &str) -> bool {
    matches!(
        name,
        "ref" | "eqref" | "pageref" | "autoref" | "cref" | "Cref" | "nameref" | "vref"
    )
}

fn latex_owner_range(
    command_start: usize,
    command_end: usize,
    owner_section: Option<usize>,
    owner_environment_start: Option<usize>,
    section_ranges: &[(usize, usize)],
    environment_ranges: &HashMap<usize, (&str, usize)>,
) -> (usize, usize) {
    owner_section
        .and_then(|index| section_ranges.get(index).copied())
        .into_iter()
        .chain(
            owner_environment_start
                .and_then(|start| environment_ranges.get(&start).map(|(_, end)| (start, *end)))
                .filter(|(start, end)| *start <= command_start && *end >= command_end),
        )
        .min_by_key(|(start, end)| end.saturating_sub(*start))
        .unwrap_or((command_start, command_end))
}

fn latex_symbol(
    source: &str,
    starts: &[usize],
    name: String,
    kind: String,
    signature: Option<String>,
    start_byte: usize,
    end_byte: usize,
) -> Symbol {
    Symbol {
        name,
        kind,
        parent: None,
        signature,
        start_line: byte_to_line(starts, source.len(), start_byte),
        end_line: byte_to_line(starts, source.len(), end_byte.saturating_sub(1)),
        start_byte,
        end_byte,
    }
}

fn append_latex_references(
    source: &str,
    starts: &[usize],
    argument_start: usize,
    argument_end: usize,
    kind: &str,
    role: ReferenceRole,
    references: &mut Vec<Reference>,
) {
    let argument = &source[argument_start..argument_end];
    let mut part_start = 0usize;
    for part in argument.split(',') {
        let leading = part.len() - part.trim_start().len();
        let name = part.trim();
        if !name.is_empty() {
            let start_byte = argument_start + part_start + leading;
            let end_byte = start_byte + name.len();
            references.push(Reference {
                name: name.to_owned(),
                kind: kind.into(),
                role,
                enclosing_symbol: None,
                start_line: byte_to_line(starts, source.len(), start_byte),
                end_line: byte_to_line(starts, source.len(), end_byte.saturating_sub(1)),
                start_byte,
                end_byte,
            });
        }
        part_start += part.len() + 1;
    }
}
