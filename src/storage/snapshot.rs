//! Application-facing reads from one pinned index snapshot.

use super::{
    ChunkHit, ChunkRecord, FilePathRecord, FileRecord, ImportRecord, ImportSymbolTarget,
    MetaRecord, ParserCoverageRows, PathRecord, ReadSession, ReferenceHit, Storage, StorageCounts,
    SymbolHit, SymbolRecord,
};
use crate::Result;
use crate::query_receipt::{QueryPartition, StoredQueryReceipt};

/// One atomically published repository generation pinned by a SQLite read
/// transaction for its complete lifetime.
///
/// Every retrieval projection must be read through this capability so source,
/// structure, paths, and metadata cannot come from different publications.
pub(crate) struct RepositoryGeneration {
    session: ReadSession,
    generation: u64,
    semantics_fingerprint: String,
    repository_identity: String,
}

impl RepositoryGeneration {
    pub(crate) fn open(storage: &Storage) -> Result<Self> {
        let session = storage.begin_read()?;
        let meta = session.meta()?;
        Ok(Self {
            session,
            generation: meta.repository_generation,
            semantics_fingerprint: meta.config_hash,
            repository_identity: meta.repository_identity,
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn semantics_fingerprint(&self) -> &str {
        &self.semantics_fingerprint
    }

    pub(crate) fn repository_identity(&self) -> &str {
        &self.repository_identity
    }

    pub(crate) fn meta(&self) -> Result<MetaRecord> {
        self.session.meta()
    }

    pub(crate) fn counts(&self) -> Result<StorageCounts> {
        self.session.counts()
    }

    pub(crate) fn parser_coverage_rows(
        &self,
        classify_extension: impl FnMut(&str) -> String,
    ) -> Result<ParserCoverageRows> {
        self.session.parser_coverage_rows(classify_extension)
    }

    pub(crate) fn whole_file_source_tokens(
        &self,
        paths: &[String],
        tokenizer: &str,
    ) -> Result<Option<usize>> {
        self.session.whole_file_source_tokens(paths, tokenizer)
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

    pub(crate) fn list_file_paths(
        &self,
        max_results: usize,
        cursor: Option<i64>,
    ) -> Result<Vec<FilePathRecord>> {
        self.session.list_file_paths(max_results, cursor)
    }

    pub(crate) fn list_glob_paths(
        &self,
        primary: &str,
        alternate: Option<&str>,
        after: Option<&str>,
        max_results: usize,
    ) -> Result<Vec<PathRecord>> {
        self.session
            .list_glob_paths(primary, alternate, after, max_results)
    }

    pub(crate) fn list_tree_paths(
        &self,
        root: &str,
        max_depth: usize,
        after: Option<&str>,
        max_results: usize,
    ) -> Result<Vec<PathRecord>> {
        self.session
            .list_tree_paths(root, max_depth, after, max_results)
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

    /// Reconstruct exact source from the canonical non-overlapping chunks in
    /// this published generation.
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

    pub(crate) fn receipt_structural_hash_matches(
        &self,
        file_id: i64,
        start_line: usize,
        end_line: usize,
        content_hash: &str,
    ) -> Result<Option<bool>> {
        self.session
            .receipt_structural_hash_matches(file_id, start_line, end_line, content_hash)
    }

    pub(crate) fn load_query_receipt(&self, requested_id: &str) -> Result<StoredQueryReceipt> {
        self.session.load_query_receipt(requested_id)
    }

    pub(crate) fn exact_query_partition(
        &self,
        allows_path: impl FnMut(&str) -> bool,
        check: impl FnMut() -> Result<()>,
    ) -> Result<QueryPartition> {
        self.session.exact_query_partition(allows_path, check)
    }
}
