use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use rmcp::{
    ErrorData, RoleServer,
    model::{ClientNotification, ClientRequest, GetExtensions, JsonRpcMessage, ServerResult},
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::Transport,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use super::{McpResultModeState, RequestAdmission, RetryableToolResponse, retryable_tool_result};

const MAX_MCP_STDIO_FRAME_BYTES: usize = 4 * 1024 * 1024;
const RETAINED_MCP_FRAME_CAPACITY: usize = 64 * 1024;

#[derive(Clone)]
struct DispatchedToolCall {
    id: rmcp::model::RequestId,
    dispatched_calls:
        Arc<Mutex<HashMap<rmcp::model::RequestId, tokio::sync::OwnedSemaphorePermit>>>,
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
    frame: Vec<u8>,
    discarding_oversized_frame: bool,
    request_dispatch: RequestAdmission,
    dispatched_calls:
        Arc<Mutex<HashMap<rmcp::model::RequestId, tokio::sync::OwnedSemaphorePermit>>>,
    result_mode: McpResultModeState,
}

impl BoundedStdioTransport {
    pub(super) fn new(
        request_dispatch: RequestAdmission,
        result_mode: impl Into<McpResultModeState>,
    ) -> Self {
        Self {
            reader: tokio::io::BufReader::with_capacity(8 * 1024, tokio::io::stdin()),
            writer: Arc::new(tokio::sync::Mutex::new(tokio::io::stdout())),
            frame: Vec::new(),
            discarding_oversized_frame: false,
            request_dispatch,
            dispatched_calls: Arc::new(Mutex::new(HashMap::new())),
            result_mode: result_mode.into(),
        }
    }

    fn release_frame(&mut self) {
        self.frame.clear();
        if self.frame.capacity() > RETAINED_MCP_FRAME_CAPACITY {
            self.frame = Vec::new();
        }
    }

    async fn write_message(
        writer: Arc<tokio::sync::Mutex<tokio::io::Stdout>>,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> std::io::Result<()> {
        let mut bytes = serde_json::to_vec(&item)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        bytes.push(b'\n');
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
                    Self::finish_dispatch(&self.dispatched_calls, id);
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
        dispatched.insert(id.clone(), permit);
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
        dispatched_calls: &Mutex<
            HashMap<rmcp::model::RequestId, tokio::sync::OwnedSemaphorePermit>,
        >,
        id: &rmcp::model::RequestId,
    ) {
        dispatched_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
    }

    fn overloaded_response(&self, id: rmcp::model::RequestId) -> TxJsonRpcMessage<RoleServer> {
        let result = retryable_tool_result(
            RetryableToolResponse::new(
                "retrieval_capacity_exhausted",
                "repository tool-call capacity is exhausted; retry shortly",
                500,
            ),
            self.result_mode.resolved_mode(),
        );
        TxJsonRpcMessage::<RoleServer>::response(ServerResult::CallToolResult(result), id)
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
            let available = match self.reader.fill_buf().await {
                Ok([]) => return None,
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "MCP stdio read failed");
                    return None;
                }
            };
            let newline = available.iter().position(|byte| *byte == b'\n');

            if self.discarding_oversized_frame {
                let consumed = newline.map_or(available.len(), |position| position + 1);
                self.reader.consume(consumed);
                if newline.is_some() {
                    self.discarding_oversized_frame = false;
                }
                continue;
            }

            let payload_bytes = newline.unwrap_or(available.len());
            if self.frame.len().saturating_add(payload_bytes) > MAX_MCP_STDIO_FRAME_BYTES {
                let consumed = newline.map_or(available.len(), |position| position + 1);
                self.reader.consume(consumed);
                self.release_frame();
                self.discarding_oversized_frame = newline.is_none();
                tracing::warn!(
                    limit = MAX_MCP_STDIO_FRAME_BYTES,
                    "discarded oversized MCP stdio frame"
                );
                continue;
            }

            self.frame.extend_from_slice(&available[..payload_bytes]);
            self.reader
                .consume(newline.map_or(payload_bytes, |position| position + 1));
            if newline.is_none() {
                continue;
            }
            if self.frame.last() == Some(&b'\r') {
                self.frame.pop();
            }
            if self.frame.is_empty() {
                continue;
            }

            let parsed = serde_json::from_slice(&self.frame);
            self.release_frame();
            match parsed {
                Ok(mut message) => match self.admit_message(&mut message) {
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
                Err(error) => match error.classify() {
                    serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
                        tracing::debug!("ignored unparsable MCP stdio frame");
                    }
                    serde_json::error::Category::Data | serde_json::error::Category::Io => {
                        tracing::debug!("rejected invalid MCP stdio message shape");
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
                    }
                },
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.writer.lock().await.shutdown().await
    }
}

#[cfg(test)]
mod tests;
