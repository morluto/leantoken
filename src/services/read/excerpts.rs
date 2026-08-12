pub(super) fn assemble_stored_excerpt(
    request: ResolvedStoredExcerptRequest,
    selected: &[crate::services::index_read::ChunkRecord],
) -> Option<StoredExcerpt> {
    let first_chunk = selected.first()?;
    let base_line = first_chunk.start_line;
    let mut combined = String::new();
    for chunk in selected {
        combined.push_str(&chunk.content);
    }
    let local_start = request.start_line.saturating_sub(base_line) + 1;
    let local_end = request.end_line.saturating_sub(base_line) + 1;
    Some(StoredExcerpt {
        content: crate::text::excerpt(&combined, local_start, local_end),
        start_line: request.start_line,
        end_line: request.end_line,
    })
}

impl Services {
    pub(super) fn stored_excerpt(
        &self,
        session: &IndexReadSnapshot,
        file_id: i64,
        start_line: usize,
        end_line: usize,
        context: usize,
        max_lines: usize,
    ) -> Result<Option<StoredExcerpt>> {
        let request = StoredExcerptRequest {
            file_id,
            desired_start_line: start_line.saturating_sub(context).max(1),
            desired_end_line: end_line.saturating_add(context),
            required_start_line: start_line,
            required_end_line: end_line,
            max_lines,
        };
        Ok(self
            .stored_excerpts(session, &[request])?
            .into_iter()
            .next()
            .flatten())
    }
}

impl Services {
    pub(in crate::services) fn stored_excerpts(
        &self,
        session: &IndexReadSnapshot,
        requests: &[StoredExcerptRequest],
    ) -> Result<Vec<Option<StoredExcerpt>>> {
        let file_ids = requests
            .iter()
            .map(|request| request.file_id)
            .collect::<Vec<_>>();
        let file_end_lines = session.file_end_lines_batch(&file_ids)?;
        let mut unique_indices = HashMap::new();
        let mut unique_requests = Vec::new();
        let mut request_mapping = Vec::new();
        for (index, (request, file_end_line)) in requests.iter().zip(file_end_lines).enumerate() {
            let Some(request) = request.resolve(file_end_line) else {
                continue;
            };
            let unique_index = *unique_indices.entry(request).or_insert_with(|| {
                let unique_index = unique_requests.len();
                unique_requests.push(request);
                unique_index
            });
            request_mapping.push((index, unique_index));
        }
        let ranges = unique_requests
            .iter()
            .map(|request| (request.file_id, request.start_line, request.end_line))
            .collect::<Vec<_>>();
        let chunks = session.get_chunks_overlapping_batch(&ranges)?;
        let hydrated = unique_requests
            .into_iter()
            .zip(chunks)
            .map(|(request, chunks)| assemble_stored_excerpt(request, &chunks))
            .collect::<Vec<_>>();
        let mut excerpts = vec![None; requests.len()];
        for (index, unique_index) in request_mapping {
            excerpts[index] = hydrated[unique_index].clone();
        }
        Ok(excerpts)
    }

    pub(in crate::services) fn fit_context_excerpt(
        &self,
        excerpt: StoredExcerpt,
        matched_line: usize,
        token_budget: usize,
        selection_budget: usize,
    ) -> Option<StoredExcerpt> {
        if matched_line < excerpt.start_line || matched_line > excerpt.end_line {
            return None;
        }
        let full_tokens = self.config.tokenizer.count(&excerpt.content).max(1);
        if full_tokens <= token_budget {
            return Some(excerpt);
        }

        let declaration_lines = excerpt
            .end_line
            .saturating_sub(excerpt.start_line)
            .saturating_add(1);
        let proportional_lines = declaration_lines
            .saturating_mul(token_budget)
            .saturating_div(full_tokens)
            .clamp(MIN_CONTEXT_RANGE_LINES, MAX_CONTEXT_RANGE_LINES)
            .min(declaration_lines);
        let proportional = crop_adaptive_excerpt(&excerpt, matched_line, proportional_lines);
        if self.config.tokenizer.count(&proportional.content) <= token_budget {
            return Some(proportional);
        }

        let required = crop_adaptive_excerpt(&excerpt, matched_line, 1);
        let required_tokens = self.config.tokenizer.count(&required.content);
        if required_tokens > token_budget {
            // Preserve an unselectable candidate so ranking can report a
            // budget omission instead of claiming indexed evidence is absent.
            return (required_tokens > selection_budget).then_some(required);
        }

        let mut lower = 2usize;
        let mut upper = proportional_lines.saturating_sub(1);
        let mut best = Some(required);
        while lower <= upper {
            let line_count = lower + (upper - lower) / 2;
            let candidate = crop_adaptive_excerpt(&excerpt, matched_line, line_count);
            if self.config.tokenizer.count(&candidate.content) <= token_budget {
                best = Some(candidate);
                lower = line_count.saturating_add(1);
            } else {
                upper = line_count.saturating_sub(1);
            }
        }
        best
    }

    pub(in crate::services) fn adaptive_context_excerpts(
        &self,
        session: &IndexReadSnapshot,
        requests: &[AdaptiveExcerptRequest],
    ) -> Result<Vec<Option<StoredExcerpt>>> {
        let full_requests = requests
            .iter()
            .map(|request| StoredExcerptRequest {
                file_id: request.file_id,
                desired_start_line: request.declaration_start,
                desired_end_line: request.declaration_end,
                required_start_line: request.matched_line,
                required_end_line: request.matched_line,
                max_lines: 0,
            })
            .collect::<Vec<_>>();
        Ok(self
            .stored_excerpts(session, &full_requests)?
            .into_iter()
            .zip(requests)
            .map(|(excerpt, request)| {
                excerpt.and_then(|excerpt| {
                    self.fit_context_excerpt(
                        excerpt,
                        request.matched_line,
                        request.token_budget,
                        request.selection_budget,
                    )
                })
            })
            .collect())
    }
}

fn crop_adaptive_excerpt(
    excerpt: &StoredExcerpt,
    matched_line: usize,
    line_count: usize,
) -> StoredExcerpt {
    let (start_line, end_line) = adaptive_excerpt_window(
        excerpt.start_line,
        excerpt.end_line,
        matched_line,
        line_count,
    );
    StoredExcerpt {
        content: crate::text::excerpt(
            &excerpt.content,
            start_line.saturating_sub(excerpt.start_line) + 1,
            end_line.saturating_sub(excerpt.start_line) + 1,
        ),
        start_line,
        end_line,
    }
}

fn adaptive_excerpt_window(
    declaration_start: usize,
    declaration_end: usize,
    matched_line: usize,
    line_count: usize,
) -> (usize, usize) {
    let line_count = line_count.max(1);
    let before = line_count / 3;
    let mut start = matched_line.saturating_sub(before).max(declaration_start);
    let mut end = start
        .saturating_add(line_count.saturating_sub(1))
        .min(declaration_end);
    if end.saturating_sub(start).saturating_add(1) < line_count {
        start = end
            .saturating_add(1)
            .saturating_sub(line_count)
            .max(declaration_start);
    }
    end = start
        .saturating_add(line_count.saturating_sub(1))
        .min(declaration_end);
    (start, end)
}
use super::*;
