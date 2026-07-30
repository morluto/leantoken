use super::*;

pub(super) struct MarkdownHeading {
    level: usize,
    name: String,
    start_byte: usize,
}

pub(super) fn parse_markdown(
    source: &str,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<ParseOutput> {
    let mut headings = Vec::new();
    let mut pending = None::<MarkdownHeading>;
    for (event, range) in MarkdownParser::new(source).into_offset_iter() {
        if is_cancelled() {
            return Err(Error::Cancelled);
        }
        match event {
            MarkdownEvent::Start(Tag::Heading { level, .. }) => {
                pending = Some(MarkdownHeading {
                    level: markdown_heading_level(level),
                    name: String::new(),
                    start_byte: range.start,
                });
            }
            MarkdownEvent::Text(text)
            | MarkdownEvent::Code(text)
            | MarkdownEvent::InlineMath(text) => {
                if let Some(heading) = &mut pending {
                    heading.name.push_str(&text);
                }
            }
            MarkdownEvent::SoftBreak | MarkdownEvent::HardBreak => {
                if let Some(heading) = &mut pending {
                    heading.name.push(' ');
                }
            }
            MarkdownEvent::End(TagEnd::Heading(_)) => {
                if let Some(mut heading) = pending.take() {
                    heading.name = heading.name.trim().to_owned();
                    if !heading.name.is_empty() {
                        headings.push(heading);
                    }
                }
            }
            _ => {}
        }
    }
    if is_cancelled() {
        return Err(Error::Cancelled);
    }

    let mut symbols = Vec::with_capacity(headings.len());
    for (index, heading) in headings.iter().enumerate() {
        let end_byte = headings[index + 1..]
            .iter()
            .find(|next| next.level <= heading.level)
            .map_or(source.len(), |next| next.start_byte);
        let (start_line, end_line) = byte_range_to_line_range(source, heading.start_byte, end_byte);
        symbols.push(Symbol {
            name: heading.name.clone(),
            kind: "markdown_heading".into(),
            parent: None,
            signature: Some(format!("{} {}", "#".repeat(heading.level), heading.name)),
            start_line,
            end_line,
            start_byte: heading.start_byte,
            end_byte,
        });
    }
    compute_symbol_parents(&mut symbols);

    Ok(ParseOutput {
        language: Some("markdown".into()),
        structurally_complete: true,
        symbols,
        references: Vec::new(),
        imports: Vec::new(),
    })
}

pub(super) fn markdown_heading_level(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}
