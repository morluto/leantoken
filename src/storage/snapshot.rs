//! Application-facing reads from one pinned index snapshot.

use super::{
    ChunkHit, ChunkRecord, FileRecord, ImportRecord, ImportSymbolTarget, ReadSession, ReferenceHit,
    Storage, SymbolHit, SymbolRecord,
};
use crate::Result;

pub(crate) struct IndexSnapshot {
    session: ReadSession,
    generation: u64,
}

impl IndexSnapshot {
    pub(crate) fn open(storage: &Storage) -> Result<Self> {
        let session = storage.begin_read()?;
        let generation = session.repository_generation()?;
        Ok(Self {
            session,
            generation,
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn file_count(&self) -> Result<usize> {
        self.session.file_count()
    }

    pub(crate) fn list_files(
        &self,
        max_results: usize,
        cursor: Option<i64>,
    ) -> Result<Vec<FileRecord>> {
        self.session.list_files(max_results, cursor)
    }

    pub(crate) fn find_file(&self, path: &str) -> Result<Option<FileRecord>> {
        self.session.find_file(path)
    }

    pub(crate) fn affected_importers(&self, candidate_paths: &[String]) -> Result<Vec<String>> {
        self.session.affected_importers(candidate_paths)
    }

    pub(crate) fn import_symbol_targets(
        &self,
        seed_paths: &[String],
        max_imports_per_seed: usize,
        max_symbols_per_target: usize,
    ) -> Result<Vec<ImportSymbolTarget>> {
        self.session
            .import_symbol_targets(seed_paths, max_imports_per_seed, max_symbols_per_target)
    }

    pub(crate) fn get_chunks_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ChunkRecord>> {
        self.session.get_chunks_for_file(file_id, max_results)
    }

    pub(crate) fn file_content(&self, file_id: i64, expected_size: usize) -> Result<String> {
        self.session.file_content(file_id, expected_size)
    }

    pub(crate) fn get_chunks_overlapping_batch(
        &self,
        ranges: &[(i64, usize, usize)],
    ) -> Result<Vec<Vec<ChunkRecord>>> {
        self.session.get_chunks_overlapping_batch(ranges)
    }

    pub(crate) fn file_end_lines_batch(&self, file_ids: &[i64]) -> Result<Vec<Option<usize>>> {
        self.session.file_end_lines_batch(file_ids)
    }

    pub(crate) fn get_symbols_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<SymbolRecord>> {
        self.session.get_symbols_for_file(file_id, max_results)
    }

    pub(crate) fn get_symbols_for_file_filtered_page(
        &self,
        file_id: i64,
        name: Option<&str>,
        kind: Option<&str>,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<SymbolRecord>> {
        self.session
            .get_symbols_for_file_filtered_page(file_id, name, kind, max_results, offset)
    }

    pub(crate) fn symbol_counts_for_file_filtered(
        &self,
        file_id: i64,
        name: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Vec<(String, usize)>> {
        self.session
            .symbol_counts_for_file_filtered(file_id, name, kind)
    }

    pub(crate) fn find_symbol(
        &self,
        file_id: i64,
        name: &str,
    ) -> Result<crate::symbol_identity::SymbolResolution<SymbolRecord>> {
        self.session.find_symbol(file_id, name)
    }

    pub(crate) fn find_document_heading(
        &self,
        file_id: i64,
        name: &str,
        occurrence: usize,
    ) -> Result<Option<SymbolRecord>> {
        self.session
            .find_document_heading(file_id, name, occurrence)
    }

    pub(crate) fn find_enclosing_symbols_batch(
        &self,
        locations: &[(i64, usize)],
    ) -> Result<Vec<Option<SymbolRecord>>> {
        self.session.find_enclosing_symbols_batch(locations)
    }

    pub(crate) fn get_imports_for_file_page(
        &self,
        file_id: i64,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<ImportRecord>> {
        self.session
            .get_imports_for_file_page(file_id, max_results, offset)
    }

    pub(crate) fn count_imports_for_file(&self, file_id: i64) -> Result<usize> {
        self.session.count_imports_for_file(file_id)
    }

    pub(crate) fn search_word_page(
        &self,
        query: &str,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<ChunkHit>> {
        self.session.search_word_page(query, max_results, offset)
    }

    pub(crate) fn search_trigram_page(
        &self,
        query: &str,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<ChunkHit>> {
        self.session.search_trigram_page(query, max_results, offset)
    }

    pub(crate) fn search_word(&self, query: &str, max_results: usize) -> Result<Vec<ChunkHit>> {
        self.session.search_word(query, max_results)
    }

    pub(crate) fn search_trigram(&self, query: &str, max_results: usize) -> Result<Vec<ChunkHit>> {
        self.session.search_trigram(query, max_results)
    }

    pub(crate) fn search_trigram_expression_page(
        &self,
        expression: &str,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<ChunkHit>> {
        self.session
            .search_trigram_expression_page(expression, max_results, offset)
    }

    pub(crate) fn search_trigram_expression(
        &self,
        expression: &str,
        max_results: usize,
    ) -> Result<Vec<ChunkHit>> {
        self.session
            .search_trigram_expression(expression, max_results)
    }

    pub(crate) fn search_symbols_page(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<SymbolHit>> {
        self.session
            .search_symbols_page(query, case_sensitive, max_results, offset)
    }

    pub(crate) fn search_symbols(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
    ) -> Result<Vec<SymbolHit>> {
        self.session
            .search_symbols(query, case_sensitive, max_results)
    }

    pub(crate) fn search_references_page(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<ReferenceHit>> {
        self.session
            .search_references_page(query, case_sensitive, max_results, offset)
    }

    pub(crate) fn search_references(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
    ) -> Result<Vec<ReferenceHit>> {
        self.session
            .search_references(query, case_sensitive, max_results)
    }

    pub(crate) fn find_symbols_exact_batch(
        &self,
        names: &[String],
        max_results_per_name: usize,
    ) -> Result<Vec<Vec<SymbolHit>>> {
        self.session
            .find_symbols_exact_batch(names, max_results_per_name)
    }

    pub(crate) fn regex_scan_files(&self, max_results: usize) -> Result<Vec<(FileRecord, usize)>> {
        self.session.regex_scan_files(max_results)
    }

    pub(crate) fn search_regex_candidates_page(
        &self,
        query: &str,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<ChunkHit>> {
        self.session
            .search_regex_candidates_page(query, max_results, offset)
    }

    pub(crate) fn select_scoped_regex_candidate_ids(
        &self,
        query: &str,
        max_rows_scanned: usize,
        max_candidates: usize,
        include_paths: &[String],
        exclude_paths: &[String],
        allows_path: impl FnMut(&str) -> bool,
    ) -> Result<Vec<i64>> {
        self.session.select_scoped_regex_candidate_ids(
            query,
            max_rows_scanned,
            max_candidates,
            include_paths,
            exclude_paths,
            allows_path,
        )
    }

    pub(crate) fn regex_candidates_by_ids(&self, chunk_ids: &[i64]) -> Result<Vec<ChunkHit>> {
        self.session.regex_candidates_by_ids(chunk_ids)
    }

    pub(crate) fn regex_candidate_count_up_to(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<usize> {
        self.session.regex_candidate_count_up_to(query, max_results)
    }
}
