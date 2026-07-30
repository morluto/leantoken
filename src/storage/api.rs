impl Storage {
    /// Return files in increasing row-id order after an optional keyset cursor.
    ///
    /// The returned record's `id` is the cursor for the next page. Callers that
    /// require a consistent multi-page view must use [`ReadSession::list_files`]
    /// on one session because file replacement can assign a new row id.
    pub fn list_files(&self, max_results: usize, cursor: Option<i64>) -> Result<Vec<FileRecord>> {
        self.begin_read()?.list_files(max_results, cursor)
    }

    pub fn find_file(&self, path: &str) -> Result<Option<FileRecord>> {
        self.begin_read()?.find_file(path)
    }

    pub fn get_chunks_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ChunkRecord>> {
        self.begin_read()?.get_chunks_for_file(file_id, max_results)
    }

    pub fn get_symbols_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<SymbolRecord>> {
        self.begin_read()?
            .get_symbols_for_file(file_id, max_results)
    }

    pub fn get_references_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ReferenceRecord>> {
        self.begin_read()?
            .get_references_for_file(file_id, max_results)
    }

    pub fn get_imports_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ImportRecord>> {
        self.begin_read()?
            .get_imports_for_file(file_id, max_results)
    }

    pub(crate) fn affected_importers(&self, candidate_paths: &[String]) -> Result<Vec<String>> {
        self.begin_read()?.affected_importers(candidate_paths)
    }

    pub(crate) fn import_seeds_for_paths(&self, paths: &[String]) -> Result<Vec<ImportSeed>> {
        self.begin_read()?.import_seeds_for_paths(paths)
    }

    pub fn search_word(&self, query: &str, max_results: usize) -> Result<Vec<ChunkHit>> {
        self.begin_read()?.search_word(query, max_results)
    }

    pub fn search_trigram(&self, query: &str, max_results: usize) -> Result<Vec<ChunkHit>> {
        self.begin_read()?.search_trigram(query, max_results)
    }

    pub fn search_symbols(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
    ) -> Result<Vec<SymbolHit>> {
        self.begin_read()?
            .search_symbols(query, case_sensitive, max_results)
    }

    pub fn search_references(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
    ) -> Result<Vec<ReferenceHit>> {
        self.begin_read()?
            .search_references(query, case_sensitive, max_results)
    }

    pub fn counts(&self) -> Result<StorageCounts> {
        self.begin_read()?.counts()
    }
}
use super::*;
