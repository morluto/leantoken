#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CandidateDiagnostics {
    Omit,
    Collect,
}

#[derive(Default)]
pub(super) struct ContextPhaseTracker {
    pub(super) enabled: bool,
    pub(super) generation: u64,
    pub(super) counters: ContextPhaseCounters,
    pub(super) timings: ContextPhaseTimings,
    pub(super) started: Option<Instant>,
    pub(super) enclosing_locations: BTreeSet<(i64, usize)>,
    pub(super) adaptive_excerpts: BTreeSet<(i64, usize, usize, usize, usize)>,
    pub(super) stored_excerpts: BTreeSet<(i64, usize, usize, usize, usize, usize)>,
    pub(super) primitive_keys: Vec<RetrievalPrimitiveKey>,
}

#[derive(Clone, Copy)]
pub(super) enum ContextTimedPhase {
    ExactSymbolLookup,
    SymbolSearch,
    ReferenceSearch,
    LexicalSearch,
    LexicalVerify,
    EnclosingLookup,
    AdaptiveExcerpt,
    StoredExcerpt,
}

impl ContextPhaseTracker {
    pub(super) fn new(diagnostics: CandidateDiagnostics, generation: u64) -> Self {
        Self {
            enabled: diagnostics == CandidateDiagnostics::Collect,
            generation,
            started: (diagnostics == CandidateDiagnostics::Collect).then(Instant::now),
            ..Self::default()
        }
    }

    pub(super) fn measure<T>(
        &mut self,
        phase: ContextTimedPhase,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if !self.enabled {
            return operation();
        }
        let started = Instant::now();
        let output = operation()?;
        self.record_elapsed(phase, Some(started));
        Ok(output)
    }

    pub(super) fn record_elapsed(&mut self, phase: ContextTimedPhase, started: Option<Instant>) {
        let Some(started) = started else { return };
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let target = match phase {
            ContextTimedPhase::ExactSymbolLookup => &mut self.timings.exact_symbol_lookup_ms,
            ContextTimedPhase::SymbolSearch => &mut self.timings.symbol_search_ms,
            ContextTimedPhase::ReferenceSearch => &mut self.timings.reference_search_ms,
            ContextTimedPhase::LexicalSearch => &mut self.timings.lexical_search_ms,
            ContextTimedPhase::LexicalVerify => &mut self.timings.lexical_verify_ms,
            ContextTimedPhase::EnclosingLookup => &mut self.timings.enclosing_lookup_ms,
            ContextTimedPhase::AdaptiveExcerpt => &mut self.timings.adaptive_excerpt_ms,
            ContextTimedPhase::StoredExcerpt => &mut self.timings.stored_excerpt_ms,
        };
        *target += elapsed_ms;
    }

    pub(super) fn timer(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
    }

    pub(super) fn record_primitive(
        &mut self,
        kind: &str,
        normalized_inputs: impl FnOnce() -> String,
    ) {
        if self.enabled {
            self.primitive_keys.push(retrieval_primitive_key(
                self.generation,
                kind,
                &normalized_inputs(),
            ));
        }
    }

    pub(super) fn record_enclosing_locations(&mut self, locations: &[(i64, usize)]) {
        if !self.enabled {
            return;
        }
        self.counters.enclosing_location_batches = self
            .counters
            .enclosing_location_batches
            .saturating_add(usize::from(!locations.is_empty()));
        self.counters.enclosing_location_requests = self
            .counters
            .enclosing_location_requests
            .saturating_add(locations.len());
        self.enclosing_locations.extend(locations.iter().copied());
        for (file_id, line) in locations {
            self.record_primitive("enclosing_symbol", || format!("{file_id}:{line}"));
        }
    }

    pub(super) fn record_adaptive_excerpts(&mut self, requests: &[AdaptiveExcerptRequest]) {
        if !self.enabled {
            return;
        }
        self.counters.adaptive_excerpt_batches = self
            .counters
            .adaptive_excerpt_batches
            .saturating_add(usize::from(!requests.is_empty()));
        self.counters.adaptive_excerpt_requests = self
            .counters
            .adaptive_excerpt_requests
            .saturating_add(requests.len());
        self.adaptive_excerpts
            .extend(requests.iter().map(|request| {
                (
                    request.file_id,
                    request.declaration_start,
                    request.declaration_end,
                    request.matched_line,
                    request.token_budget,
                )
            }));
        for request in requests {
            self.record_primitive("adaptive_excerpt", || {
                format!(
                    "{}:{}:{}:{}:{}",
                    request.file_id,
                    request.declaration_start,
                    request.declaration_end,
                    request.matched_line,
                    request.token_budget
                )
            });
        }
    }

    pub(super) fn record_stored_excerpts(&mut self, requests: &[StoredExcerptRequest]) {
        if !self.enabled {
            return;
        }
        self.counters.stored_excerpt_batches = self
            .counters
            .stored_excerpt_batches
            .saturating_add(usize::from(!requests.is_empty()));
        self.counters.stored_excerpt_requests = self
            .counters
            .stored_excerpt_requests
            .saturating_add(requests.len());
        self.stored_excerpts.extend(requests.iter().map(|request| {
            (
                request.file_id,
                request.desired_start_line,
                request.desired_end_line,
                request.required_start_line,
                request.required_end_line,
                request.max_lines,
            )
        }));
        for request in requests {
            self.record_primitive("stored_excerpt", || {
                format!(
                    "{}:{}:{}:{}:{}:{}",
                    request.file_id,
                    request.desired_start_line,
                    request.desired_end_line,
                    request.required_start_line,
                    request.required_end_line,
                    request.max_lines
                )
            });
        }
    }

    pub(super) fn finish(
        mut self,
        generated_candidates: usize,
    ) -> (
        ContextPhaseCounters,
        ContextPhaseTimings,
        Vec<RetrievalPrimitiveKey>,
    ) {
        if !self.enabled {
            return (
                ContextPhaseCounters::default(),
                ContextPhaseTimings::default(),
                Vec::new(),
            );
        }
        self.counters.unique_enclosing_locations = self.enclosing_locations.len();
        self.counters.unique_adaptive_excerpt_requests = self.adaptive_excerpts.len();
        self.counters.unique_stored_excerpt_requests = self.stored_excerpts.len();
        self.counters.generated_candidates = generated_candidates;
        self.timings.total_ms = self
            .started
            .map(|started| started.elapsed().as_secs_f64() * 1_000.0)
            .unwrap_or(0.0);
        (self.counters, self.timings, self.primitive_keys)
    }
}

pub(super) struct LexicalMatchFacts {
    pub(super) search_hit: SearchHit,
    pub(super) matched_line: usize,
    pub(super) occurrences: usize,
}

pub(super) fn analyze_lexical_match(
    hit: &ChunkHit,
    matcher: &regex::Regex,
    context_lines: usize,
) -> Option<LexicalMatchFacts> {
    let mut matches = matcher.find_iter(&hit.content);
    let first = matches.next()?;
    let occurrences = 1usize.saturating_add(
        matches
            .take(LEXICAL_OCCURRENCE_SATURATION.saturating_sub(1))
            .count(),
    );
    let starts = line_starts(&hit.content);
    let search_hit = chunk_search_hit_for_range(
        hit,
        first.start(),
        first.end(),
        context_lines,
        false,
        false,
        &starts,
    );
    let local_start = byte_to_line(&starts, hit.content.len(), first.start());
    Some(LexicalMatchFacts {
        search_hit,
        matched_line: hit.start_line + local_start - 1,
        occurrences,
    })
}
use super::*;
