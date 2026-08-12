//! Service-side naming for the storage-owned index snapshot capability.

pub(super) use crate::storage::IndexSnapshot as IndexReadSnapshot;
pub(super) use crate::storage::{
    ChunkHit, ChunkRecord, FilePathRecord, FileRecord, ReferenceHit, SymbolHit, SymbolRecord,
};
