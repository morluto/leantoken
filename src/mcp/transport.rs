use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};

use rmcp::{
    ErrorData, RoleServer,
    model::{
        ClientNotification, ClientRequest, GetExtensions, JsonRpcMessage, ProtocolVersion,
        ServerResult,
    },
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{
        Transport,
        async_rw::{JsonRpcMessageCodec, JsonRpcMessageCodecError},
    },
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio_util::{
    bytes::BytesMut,
    codec::{Decoder, Encoder},
};

use super::{McpResultMode, RequestAdmission, RetryableToolResponse, retryable_tool_result};

const MAX_MCP_STDIO_FRAME_BYTES: usize = 4 * 1024 * 1024;
const RETAINED_MCP_FRAME_CAPACITY: usize = 64 * 1024;

/// Dispatch entry state: active (holding a permit) or tombstoned (cancelled
/// but handler still draining). Tombstoned entries prevent ID reuse until the
/// handler's response arrives and cleans up the entry.
#[allow(dead_code)]
enum DispatchEntry {
    Active(tokio::sync::OwnedSemaphorePermit),
    Tombstoned,
}

#[derive(Clone)]
struct DispatchedToolCall {
    id: rmcp::model::RequestId,
    dispatched_calls: Arc<Mutex<HashMap<rmcp::model::RequestId, DispatchEntry>>>,
}

impl Drop for DispatchedToolCall {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.dispatched_calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&self.id);
        }
    }
}

pub(super) struct BoundedStdioTransport {
    reader: tokio::io::BufReader<tokio::io::Stdin>,
    writer: Arc<tokio::sync::Mutex<tokio::io::Stdout>>,
    decoder: JsonRpcMessageCodec<RxJsonRpcMessage<RoleServer>>,
    read_buffer: BytesMut,
    request_dispatch: RequestAdmission,
    dispatched_calls: Arc<Mutex<HashMap<rmcp::model::RequestId, DispatchEntry>>>,
    result_mode: McpResultMode,
    negotiated_protocol: Arc<RwLock<Option<ProtocolVersion>>>,
}

impl BoundedStdioTransport {
    pub(super) fn new(request_dispatch: RequestAdmission, result_mode: McpResultMode) -> Self {
        Self {
            reader: tokio::io::BufReader::with_capacity(8 * 1024, tokio::io::stdin()),
            writer: Arc::new(tokio::sync::Mutex::new(tokio::io::stdout())),
            decoder: JsonRpcMessageCodec::new_with_max_length(MAX_MCP_STDIO_FRAME_BYTES),
            read_buffer: BytesMut::new(),
            request_dispatch,
            dispatched_calls: Arc::new(Mutex::new(HashMap::new())),
            result_mode,
            negotiated_protocol: Arc::new(RwLock::new(None)),
        }
    }

    fn release_oversized_read_buffer(&mut self) {
        if self.read_buffer.capacity() <= RETAINED_MCP_FRAME_CAPACITY
            || self.read_buffer.len() > RETAINED_MCP_FRAME_CAPACITY
        {
            return;
        }
        let mut compact = BytesMut::with_capacity(self.read_buffer.len());
        compact.extend_from_slice(&self.read_buffer);
        self.read_buffer = compact;
    }

    async fn write_message(
        writer: Arc<tokio::sync::Mutex<tokio::io::Stdout>>,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> std::io::Result<()> {
        let mut bytes = BytesMut::new();
        JsonRpcMessageCodec::default()
            .encode(item, &mut bytes)
            .map_err(std::io::Error::from)?;
        let mut writer = writer.lock().await;
        writer.write_all(&bytes).await?;
        writer.flush().await
    }

    fn admit_message(
        &self,
        message: &mut RxJsonRpcMessage<RoleServer>,
    ) -> Result<(), rmcp::model::RequestId> {
        let request = match message {
            JsonRpcMessage::Notification(notification) => {
                if let ClientNotification::CancelledNotification(cancelled) =
                    &notification.notification
                    && let Some(id) = &cancelled.params.request_id
                {
                    Self::tombstone_dispatch(&self.dispatched_calls, id);
                }
                return Ok(());
            }
            JsonRpcMessage::Request(request) => request,
            JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_) => return Ok(()),
        };
        if !matches!(&request.request, ClientRequest::CallToolRequest(_)) {
            return Ok(());
        }
        let id = request.id.clone();
        let mut dispatched = self
            .dispatched_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if dispatched.contains_key(&id) {
            return Err(id);
        }
        let permit = match self.request_dispatch.try_admit() {
            Ok(permit) => permit,
            Err(_) => return Err(id),
        };
        dispatched.insert(id.clone(), DispatchEntry::Active(permit));
        drop(dispatched);
        request.request.extensions_mut().insert(DispatchedToolCall {
            id,
            dispatched_calls: Arc::clone(&self.dispatched_calls),
        });
        Ok(())
    }

    fn response_id(item: &TxJsonRpcMessage<RoleServer>) -> Option<rmcp::model::RequestId> {
        match item {
            JsonRpcMessage::Response(response) => Some(response.id.clone()),
            JsonRpcMessage::Error(error) => error.id.clone(),
            JsonRpcMessage::Request(_) | JsonRpcMessage::Notification(_) => None,
        }
    }

    fn finish_dispatch(
        dispatched_calls: &Mutex<HashMap<rmcp::model::RequestId, DispatchEntry>>,
        id: &rmcp::model::RequestId,
    ) {
        dispatched_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
    }

    /// Replace an active dispatch entry with a tombstone, releasing the
    /// semaphore permit but keeping the ID reserved so it cannot be reused
    /// until the handler's response arrives and cleans up the entry.
    fn tombstone_dispatch(
        dispatched_calls: &Mutex<HashMap<rmcp::model::RequestId, DispatchEntry>>,
        id: &rmcp::model::RequestId,
    ) {
        let mut dispatched = dispatched_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = dispatched.get_mut(id) {
            *entry = DispatchEntry::Tombstoned;
        }
    }

    fn overloaded_response(&self, id: rmcp::model::RequestId) -> TxJsonRpcMessage<RoleServer> {
        let result = retryable_tool_result(
            RetryableToolResponse::new(
                "retrieval_capacity_exhausted",
                "repository tool-call capacity is exhausted; retry shortly",
                500,
            ),
            self.result_mode,
        );
        let mut result = ServerResult::CallToolResult(result);
        let modern_protocol = self
            .negotiated_protocol
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(|version| version >= &ProtocolVersion::V_2026_07_28);
        if !modern_protocol {
            result.strip_result_type_for_legacy_peer();
        }
        TxJsonRpcMessage::<RoleServer>::response(result, id)
    }
}

impl Transport<RoleServer> for BoundedStdioTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let writer = Arc::clone(&self.writer);
        let dispatched_calls = Arc::clone(&self.dispatched_calls);
        let response_id = Self::response_id(&item);
        if let JsonRpcMessage::Response(response) = &item
            && let ServerResult::InitializeResult(result) = &response.result
        {
            *self
                .negotiated_protocol
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some(result.protocol_version.clone());
        }
        async move {
            let result = Self::write_message(writer, item).await;
            if let Some(id) = response_id {
                Self::finish_dispatch(&dispatched_calls, &id);
            }
            result
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            let buffered_before = self.read_buffer.len();
            match self.decoder.decode(&mut self.read_buffer) {
                Ok(Some(mut message)) => match self.admit_message(&mut message) {
                    Ok(()) => return Some(message),
                    Err(id) => {
                        let response = self.overloaded_response(id);
                        if Self::write_message(Arc::clone(&self.writer), response)
                            .await
                            .is_err()
                        {
                            return None;
                        }
                    }
                },
                Err(JsonRpcMessageCodecError::MaxLineLengthExceeded) => {
                    tracing::warn!(
                        limit = MAX_MCP_STDIO_FRAME_BYTES,
                        "discarded oversized MCP stdio frame"
                    );
                    continue;
                }
                Err(JsonRpcMessageCodecError::Serde(error)) => match error.classify() {
                    serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
                        tracing::debug!(%error, "ignored unparsable MCP stdio frame");
                        continue;
                    }
                    serde_json::error::Category::Data | serde_json::error::Category::Io => {
                        tracing::debug!(%error, "rejected invalid MCP stdio message shape");
                        let response = TxJsonRpcMessage::<RoleServer>::error(
                            ErrorData::invalid_request("Invalid request", None),
                            None,
                        );
                        if Self::write_message(Arc::clone(&self.writer), response)
                            .await
                            .is_err()
                        {
                            return None;
                        }
                        continue;
                    }
                },
                Err(JsonRpcMessageCodecError::Io(error)) => {
                    tracing::warn!(%error, "MCP stdio decode failed");
                    return None;
                }
                Err(error) => {
                    tracing::warn!(%error, "MCP stdio decode failed");
                    return None;
                }
                Ok(None) if self.read_buffer.len() < buffered_before => {
                    self.release_oversized_read_buffer();
                    continue;
                }
                Ok(None) => {}
            }

            self.release_oversized_read_buffer();
            let available = match self.reader.fill_buf().await {
                Ok([]) => return None,
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "MCP stdio read failed");
                    return None;
                }
            };
            let consumed = available.len();
            self.read_buffer.extend_from_slice(available);
            self.reader.consume(consumed);
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.writer.lock().await.shutdown().await
    }
}

#[cfg(test)]
mod tests;
