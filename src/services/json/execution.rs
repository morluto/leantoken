//! Cursor versions, key ordering, and execution options shared by the JSON
//! service entry points and the legacy/MCP adapters.

pub(crate) const MAX_JSON_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JsonCursorVersion {
    V1,
    V2,
}

impl JsonCursorVersion {
    pub(super) fn prefix(self) -> &'static str {
        match self {
            Self::V1 => "j1",
            Self::V2 => "j2",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JsonKeyOrder {
    Pointer,
    DepthThenPointer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JsonExecutionOptions {
    depth: Option<usize>,
    key_order: JsonKeyOrder,
    cursor_version: JsonCursorVersion,
}

impl JsonExecutionOptions {
    pub(super) fn legacy() -> Self {
        Self {
            depth: None,
            key_order: JsonKeyOrder::Pointer,
            cursor_version: JsonCursorVersion::V1,
        }
    }

    pub(crate) fn mcp(depth: Option<usize>) -> Self {
        Self {
            depth,
            key_order: JsonKeyOrder::DepthThenPointer,
            cursor_version: JsonCursorVersion::V2,
        }
    }

    pub(super) fn depth(self) -> Option<usize> {
        self.depth
    }

    pub(super) fn key_order(self) -> JsonKeyOrder {
        self.key_order
    }

    pub(super) fn cursor_version(self) -> JsonCursorVersion {
        self.cursor_version
    }
}
