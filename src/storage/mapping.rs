impl Storage {
    pub(crate) fn map_file(row: &Row) -> std::result::Result<FileRecord, rusqlite::Error> {
        Ok(FileRecord {
            id: row.get(0)?,
            path: row.get(1)?,
            language: row.get(2)?,
            size_bytes: i64_to_u64(row.get(3)?)?,
            modified_ns: row.get::<_, Option<i64>>(4)?.map(i64_to_u128).transpose()?,
            content_hash: row.get(5)?,
            generation: i64_to_u64(row.get(6)?)?,
            structurally_complete: row.get(7)?,
        })
    }

    pub(crate) fn map_chunk(row: &Row) -> std::result::Result<ChunkRecord, rusqlite::Error> {
        Ok(ChunkRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            content: row.get(2)?,
            start_line: i64_to_usize(row.get(3)?)?,
            end_line: i64_to_usize(row.get(4)?)?,
            start_byte: i64_to_usize(row.get(5)?)?,
            end_byte: i64_to_usize(row.get(6)?)?,
            token_count: i64_to_usize(row.get(7)?)?,
        })
    }

    pub(crate) fn map_symbol(row: &Row) -> std::result::Result<SymbolRecord, rusqlite::Error> {
        Ok(SymbolRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            kind: row.get(3)?,
            parent: row.get(4)?,
            signature: row.get(5)?,
            start_line: i64_to_usize(row.get(6)?)?,
            end_line: i64_to_usize(row.get(7)?)?,
            start_byte: i64_to_usize(row.get(8)?)?,
            end_byte: i64_to_usize(row.get(9)?)?,
        })
    }

    pub(crate) fn map_reference(
        row: &Row,
    ) -> std::result::Result<ReferenceRecord, rusqlite::Error> {
        Ok(ReferenceRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            kind: row.get(3)?,
            role: role_from_str(&row.get::<_, String>(4)?),
            enclosing_symbol: row.get(5)?,
            start_line: i64_to_usize(row.get(6)?)?,
            end_line: i64_to_usize(row.get(7)?)?,
            start_byte: i64_to_usize(row.get(8)?)?,
            end_byte: i64_to_usize(row.get(9)?)?,
        })
    }

    pub(crate) fn map_import(row: &Row) -> std::result::Result<ImportRecord, rusqlite::Error> {
        Ok(ImportRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            raw_target: row.get(2)?,
            resolved_path: row.get(3)?,
            line: i64_to_usize(row.get(4)?)?,
        })
    }

    pub(crate) fn map_chunk_hit(row: &Row) -> std::result::Result<ChunkHit, rusqlite::Error> {
        Ok(ChunkHit {
            chunk_id: row.get(0)?,
            file_id: row.get(1)?,
            path: row.get(2)?,
            content: row.get(3)?,
            start_line: i64_to_usize(row.get(4)?)?,
            end_line: i64_to_usize(row.get(5)?)?,
            start_byte: i64_to_usize(row.get(6)?)?,
            end_byte: i64_to_usize(row.get(7)?)?,
            token_count: i64_to_usize(row.get(8)?)?,
            generation: i64_to_u64(row.get(9)?)?,
            score: row.get::<_, f64>(10)?,
        })
    }

    pub(crate) fn map_symbol_hit(row: &Row) -> std::result::Result<SymbolHit, rusqlite::Error> {
        Ok(SymbolHit {
            path: row.get(0)?,
            content_hash: row.get(1)?,
            generation: i64_to_u64(row.get(2)?)?,
            symbol: SymbolRecord {
                id: row.get(3)?,
                file_id: row.get(4)?,
                name: row.get(5)?,
                kind: row.get(6)?,
                parent: row.get(7)?,
                signature: row.get(8)?,
                start_line: i64_to_usize(row.get(9)?)?,
                end_line: i64_to_usize(row.get(10)?)?,
                start_byte: i64_to_usize(row.get(11)?)?,
                end_byte: i64_to_usize(row.get(12)?)?,
            },
        })
    }

    pub(crate) fn map_reference_hit(
        row: &Row,
    ) -> std::result::Result<ReferenceHit, rusqlite::Error> {
        Ok(ReferenceHit {
            path: row.get(0)?,
            content_hash: row.get(1)?,
            generation: i64_to_u64(row.get(2)?)?,
            reference: ReferenceRecord {
                id: row.get(3)?,
                file_id: row.get(4)?,
                name: row.get(5)?,
                kind: row.get(6)?,
                role: role_from_str(&row.get::<_, String>(7)?),
                enclosing_symbol: row.get(8)?,
                start_line: i64_to_usize(row.get(9)?)?,
                end_line: i64_to_usize(row.get(10)?)?,
                start_byte: i64_to_usize(row.get(11)?)?,
                end_byte: i64_to_usize(row.get(12)?)?,
            },
        })
    }
}
use super::*;
