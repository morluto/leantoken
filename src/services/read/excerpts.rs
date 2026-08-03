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

#[cfg(test)]
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

    #[cfg(test)]
    pub(in crate::services) fn adaptive_context_excerpt(
        &self,
        session: &IndexReadSnapshot,
        file_id: i64,
        declaration_start: usize,
        declaration_end: usize,
        matched_line: usize,
        token_budget: usize,
    ) -> Result<Option<StoredExcerpt>> {
        let Some(full) =
            self.stored_excerpt(session, file_id, declaration_start, declaration_end, 0, 0)?
        else {
            return Ok(None);
        };
        let full_tokens = self.config.tokenizer.count(&full.content).max(1);
        if full_tokens <= token_budget {
            return Ok(Some(full));
        }

        let declaration_lines = declaration_end
            .saturating_sub(declaration_start)
            .saturating_add(1);
        let proportional_lines = declaration_lines
            .saturating_mul(token_budget)
            .saturating_div(full_tokens)
            .clamp(MIN_CONTEXT_RANGE_LINES, MAX_CONTEXT_RANGE_LINES)
            .min(declaration_lines);
        let before = proportional_lines / 3;
        let mut start = matched_line.saturating_sub(before).max(declaration_start);
        let mut end = start
            .saturating_add(proportional_lines.saturating_sub(1))
            .min(declaration_end);
        if end.saturating_sub(start).saturating_add(1) < proportional_lines {
            start = end
                .saturating_add(1)
                .saturating_sub(proportional_lines)
                .max(declaration_start);
        }
        end = start
            .saturating_add(proportional_lines.saturating_sub(1))
            .min(declaration_end);
        self.stored_excerpt(session, file_id, start, end, 0, 0)
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
        let mut excerpts = self.stored_excerpts(session, &full_requests)?;
        let mut narrowed_indices = Vec::new();
        let mut narrowed_requests = Vec::new();
        for (index, (request, excerpt)) in requests.iter().zip(&excerpts).enumerate() {
            let Some(excerpt) = excerpt else {
                continue;
            };
            let full_tokens = self.config.tokenizer.count(&excerpt.content).max(1);
            if full_tokens <= request.token_budget {
                continue;
            }
            let declaration_lines = request
                .declaration_end
                .saturating_sub(request.declaration_start)
                .saturating_add(1);
            let proportional_lines = declaration_lines
                .saturating_mul(request.token_budget)
                .saturating_div(full_tokens)
                .clamp(MIN_CONTEXT_RANGE_LINES, MAX_CONTEXT_RANGE_LINES)
                .min(declaration_lines);
            let before = proportional_lines / 3;
            let mut start = request
                .matched_line
                .saturating_sub(before)
                .max(request.declaration_start);
            let mut end = start
                .saturating_add(proportional_lines.saturating_sub(1))
                .min(request.declaration_end);
            if end.saturating_sub(start).saturating_add(1) < proportional_lines {
                start = end
                    .saturating_add(1)
                    .saturating_sub(proportional_lines)
                    .max(request.declaration_start);
            }
            end = start
                .saturating_add(proportional_lines.saturating_sub(1))
                .min(request.declaration_end);
            narrowed_indices.push(index);
            narrowed_requests.push(StoredExcerptRequest {
                file_id: request.file_id,
                desired_start_line: start,
                desired_end_line: end,
                required_start_line: request.matched_line,
                required_end_line: request.matched_line,
                max_lines: 0,
            });
        }
        let narrowed = self.stored_excerpts(session, &narrowed_requests)?;
        for (index, excerpt) in narrowed_indices.into_iter().zip(narrowed) {
            excerpts[index] = excerpt;
        }
        Ok(excerpts)
    }
}
use super::*;
