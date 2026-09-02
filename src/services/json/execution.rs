//! Cursor versions, key ordering, and execution options shared by the JSON
//! service entry points and the standard JSON/MCP adapters.

pub(crate) const MAX_JSON_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JsonKeyOrder {
    Pointer,
    DepthThenPointer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JsonExecutionOptions {
    depth: Option<usize>,
    key_order: JsonKeyOrder,
}

impl JsonExecutionOptions {
    pub(super) fn standard() -> Self {
        Self {
            depth: None,
            key_order: JsonKeyOrder::Pointer,
        }
    }

    pub(crate) fn mcp(depth: Option<usize>) -> Self {
        Self {
            depth,
            key_order: JsonKeyOrder::DepthThenPointer,
        }
    }

    pub(super) fn depth(self) -> Option<usize> {
        self.depth
    }

    pub(super) fn key_order(self) -> JsonKeyOrder {
        self.key_order
    }
}
