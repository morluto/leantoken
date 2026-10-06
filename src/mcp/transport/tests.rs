use std::{
    pin::Pin,
    task::{Context, Poll, Waker},
    time::Duration,
};

use rmcp::{model::JsonRpcMessage, service::RxJsonRpcMessage};
use tokio::io::{AsyncReadExt, BufReader, DuplexStream, Empty};

use super::super::{
    DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY, McpResultMode, RequestAdmission, RoleServer,
};
use super::*;

fn incoming_request(value: serde_json::Value) -> RxJsonRpcMessage<RoleServer> {
    serde_json::from_value(value).expect("valid MCP request")
}

fn buffered_transport(
    input: serde_json::Value,
    writer_capacity: usize,
    dispatch: RequestAdmission,
) -> (BoundedTransport<Empty, DuplexStream>, DuplexStream) {
    let (writer, peer) = tokio::io::duplex(writer_capacity);
    let mut transport = BoundedTransport::with_io(
        tokio::io::empty(),
        writer,
        dispatch,
        McpResultMode::Structured,
    );
    for frame in [
        input,
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
    ] {
        transport
            .read_buffer
            .extend_from_slice(serde_json::to_string(&frame).unwrap().as_bytes());
        transport.read_buffer.extend_from_slice(b"\n");
    }
    (transport, peer)
}

fn assert_pending<F: Future>(future: Pin<&mut F>) {
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(future.poll(&mut context), Poll::Pending));
}

fn tool_request() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "files", "arguments": {}}
    })
}

fn assert_ping(message: Option<RxJsonRpcMessage<RoleServer>>) {
    assert!(
        matches!(message, Some(JsonRpcMessage::Request(request)) if request.id == rmcp::model::NumberOrString::Number(2))
    );
}

#[tokio::test]
async fn cancelled_receive_preserves_rejection_while_waiting_for_the_writer() {
    let dispatch = RequestAdmission::new(1);
    let _permit = dispatch.try_admit().expect("occupy tool capacity");
    let (mut transport, peer) = buffered_transport(tool_request(), 4096, dispatch);
    let writer = Arc::clone(&transport.writer);
    let guard = writer.lock().await;
    let mut receiving = Box::pin(transport.receive());
    assert_pending(receiving.as_mut());
    drop(receiving);
    drop(guard);

    let mut reader = BufReader::new(peer);
    let mut line = String::new();
    let (next, read) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(transport.receive(), reader.read_line(&mut line))
    })
    .await
    .expect("cancelled receive must retain the rejection");
    read.expect("read rejection");
    assert_ping(next);
    let response: serde_json::Value = serde_json::from_str(&line).expect("complete JSON-RPC frame");
    assert_eq!(response["id"], 1);
    assert_eq!(
        response["result"]["structuredContent"]["reason"],
        "retrieval_capacity_exhausted"
    );
}

#[tokio::test]
async fn cancelled_receive_preserves_partial_rejection_frame() {
    let dispatch = RequestAdmission::new(1);
    let _permit = dispatch.try_admit().expect("occupy tool capacity");
    let (mut transport, mut peer) = buffered_transport(tool_request(), 8, dispatch);
    let mut receiving = Box::pin(transport.receive());
    assert_pending(receiving.as_mut());
    drop(receiving);
    let mut prefix = [0; 8];
    peer.read_exact(&mut prefix)
        .await
        .expect("read partial frame");

    let mut resumed = Box::pin(transport.receive());
    assert_pending(resumed.as_mut());
    let mut reader = BufReader::new(peer);
    let mut line = String::from_utf8(prefix.to_vec()).expect("JSON prefix");
    let (next, read) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(resumed, reader.read_line(&mut line))
    })
    .await
    .expect("partial rejection must resume without blocking the next request");
    read.expect("read rejection suffix");
    assert_ping(next);
    let response: serde_json::Value =
        serde_json::from_str(&line).expect("no lost or duplicated frame prefix");
    assert_eq!(response["id"], 1);
    assert_eq!(
        response["result"]["structuredContent"]["reason"],
        "retrieval_capacity_exhausted"
    );
}

#[tokio::test]
async fn close_finishes_a_cancelled_partial_rejection() {
    let dispatch = RequestAdmission::new(1);
    let _permit = dispatch.try_admit().expect("occupy tool capacity");
    let (mut transport, mut peer) = buffered_transport(tool_request(), 8, dispatch);
    let mut receiving = Box::pin(transport.receive());
    assert_pending(receiving.as_mut());
    drop(receiving);

    let mut bytes = Vec::new();
    let (closed, read) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(transport.close(), peer.read_to_end(&mut bytes))
    })
    .await
    .expect("close must drain retained output before shutting down its writer");
    closed.expect("close transport");
    read.expect("read final output");
    let response: serde_json::Value =
        serde_json::from_slice(&bytes).expect("complete final rejection");
    assert_eq!(response["id"], 1);
}

#[tokio::test(start_paused = true)]
async fn close_bounds_a_retained_write_when_the_peer_stops_reading() {
    let dispatch = RequestAdmission::new(1);
    let _permit = dispatch.try_admit().expect("occupy tool capacity");
    let (mut transport, _peer) = buffered_transport(tool_request(), 8, dispatch);
    let writer = Arc::clone(&transport.writer);
    let mut receiving = Box::pin(transport.receive());
    assert_pending(receiving.as_mut());
    drop(receiving);

    let started = tokio::time::Instant::now();
    let error = tokio::time::timeout(Duration::from_secs(3), transport.close())
        .await
        .expect("close must finish within its own shutdown budget")
        .expect_err("an undrained write must time out");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(started.elapsed(), Duration::from_secs(2));
    assert!(transport.pending_write.is_none());
    assert!(
        writer.try_lock().is_ok(),
        "cancelled close must release its writer"
    );
}

#[tokio::test(start_paused = true)]
async fn close_bounds_waiting_for_another_sender() {
    let (mut transport, _peer) = buffered_transport(tool_request(), 8, RequestAdmission::new(1));
    let writer = Arc::clone(&transport.writer);
    let _guard = writer.lock().await;

    let started = tokio::time::Instant::now();
    let error = tokio::time::timeout(Duration::from_secs(3), transport.close())
        .await
        .expect("close must not wait indefinitely for another sender")
        .expect_err("an occupied writer must time out");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(started.elapsed(), Duration::from_secs(2));
}

struct PendingShutdownWriter;

impl AsyncWrite for PendingShutdownWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Ok(buffer.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }
}

#[tokio::test(start_paused = true)]
async fn close_bounds_the_writer_shutdown_and_releases_its_guard() {
    let mut transport = BoundedTransport::with_io(
        tokio::io::empty(),
        PendingShutdownWriter,
        RequestAdmission::new(1),
        McpResultMode::Structured,
    );
    let writer = Arc::clone(&transport.writer);
    let started = tokio::time::Instant::now();
    let error = tokio::time::timeout(Duration::from_secs(3), transport.close())
        .await
        .expect("the shutdown operation must share the close budget")
        .expect_err("a blocked shutdown must time out");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(started.elapsed(), Duration::from_secs(2));
    assert!(
        writer.try_lock().is_ok(),
        "timed-out shutdown must release its writer"
    );
}

#[tokio::test]
async fn cancelled_receive_preserves_invalid_shape_errors() {
    let (mut transport, peer) = buffered_transport(
        serde_json::json!({"foo": "bar"}),
        4096,
        RequestAdmission::new(1),
    );
    let writer = Arc::clone(&transport.writer);
    let guard = writer.lock().await;
    let mut receiving = Box::pin(transport.receive());
    assert_pending(receiving.as_mut());
    drop(receiving);
    drop(guard);

    let mut reader = BufReader::new(peer);
    let mut line = String::new();
    let (next, read) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(transport.receive(), reader.read_line(&mut line))
    })
    .await
    .expect("invalid shape response must survive receive cancellation");
    read.expect("read invalid request error");
    assert_ping(next);
    let response: serde_json::Value = serde_json::from_str(&line).expect("complete error frame");
    assert_eq!(response["error"]["code"], -32600);
    assert_eq!(response["id"], serde_json::Value::Null);
}

#[test]
fn dispatch_has_an_exact_tool_boundary_and_bypasses_control_requests() {
    let dispatch = RequestAdmission::new(DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY);
    let transport = BoundedStdioTransport::new(dispatch.clone(), McpResultMode::Dual);
    let mut admitted = (0..DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY)
        .map(|id| {
            let mut request = incoming_request(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": "files", "arguments": {}}
            }));
            transport
                .admit_message(&mut request)
                .expect("admit tool call");
            request
        })
        .collect::<Vec<_>>();
    assert_eq!(dispatch.available_permits(), 0);

    let mut excess = incoming_request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1000,
        "method": "tools/call",
        "params": {"name": "files", "arguments": {}}
    }));
    assert!(transport.admit_message(&mut excess).is_err());

    for mut control in [
        incoming_request(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1001,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1"}
            }
        })),
        incoming_request(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1002,
            "method": "tools/list",
            "params": {}
        })),
    ] {
        transport
            .admit_message(&mut control)
            .expect("control request bypasses tool dispatch");
    }

    let request_ids = admitted
        .iter()
        .map(|message| match message {
            JsonRpcMessage::Request(request) => request.id.clone(),
            _ => unreachable!("test creates requests"),
        })
        .collect::<Vec<_>>();
    admitted.clear();
    assert_eq!(
        dispatch.available_permits(),
        0,
        "handler completion alone must not release response capacity"
    );
    for id in request_ids {
        BoundedStdioTransport::finish_dispatch(&transport.dispatched_calls, &id);
    }
    assert_eq!(
        dispatch.available_permits(),
        DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY
    );
}

#[test]
fn dispatch_permit_returns_when_a_handler_unwinds() {
    let dispatch = RequestAdmission::new(1);
    let transport = BoundedStdioTransport::new(dispatch.clone(), McpResultMode::Dual);

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut request = incoming_request(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "files", "arguments": {}}
        }));
        transport
            .admit_message(&mut request)
            .expect("admit tool call");
        assert_eq!(dispatch.available_permits(), 0);
        panic!("injected handler panic");
    }));

    assert!(unwind.is_err());
    assert_eq!(dispatch.available_permits(), 1);
}

#[test]
fn dispatch_permit_returns_when_a_request_is_cancelled() {
    let dispatch = RequestAdmission::new(1);
    let transport = BoundedStdioTransport::new(dispatch.clone(), McpResultMode::Dual);
    let mut request = incoming_request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "files", "arguments": {}}
    }));
    transport
        .admit_message(&mut request)
        .expect("admit tool call");
    assert_eq!(dispatch.available_permits(), 0);

    let mut cancellation = incoming_request(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 1, "reason": "no longer needed"}
    }));
    transport
        .admit_message(&mut cancellation)
        .expect("admit cancellation");
    assert_eq!(dispatch.available_permits(), 1);

    drop(request);
    assert_eq!(dispatch.available_permits(), 1);
}

#[test]
fn overload_results_follow_the_negotiated_rmcp_result_shape() {
    let transport = BoundedStdioTransport::new(RequestAdmission::new(1), McpResultMode::Dual);
    let legacy =
        serde_json::to_value(transport.overloaded_response(rmcp::model::NumberOrString::Number(1)))
            .expect("serialize legacy overload response");
    assert!(legacy.pointer("/result/resultType").is_none());

    *transport
        .negotiated_protocol
        .write()
        .expect("protocol lock") = Some(ProtocolVersion::V_2026_07_28);
    let modern =
        serde_json::to_value(transport.overloaded_response(rmcp::model::NumberOrString::Number(2)))
            .expect("serialize modern overload response");
    assert_eq!(
        modern.pointer("/result/resultType"),
        Some(&serde_json::json!("complete"))
    );
}

#[test]
fn control_request_with_a_reserved_id_is_rejected_and_keeps_the_tombstone() {
    let dispatch = RequestAdmission::new(1);
    let transport = BoundedStdioTransport::new(dispatch.clone(), McpResultMode::Dual);
    let mut tool = incoming_request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "files", "arguments": {}}
    }));
    transport.admit_message(&mut tool).expect("admit tool call");

    let mut cancellation = incoming_request(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 1, "reason": "no longer needed"}
    }));
    transport
        .admit_message(&mut cancellation)
        .expect("admit cancellation");

    let mut ping = incoming_request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "ping"
    }));
    let failure = transport
        .admit_message(&mut ping)
        .expect_err("control request with a reserved id must be rejected");
    assert!(
        !failure.tool_call,
        "a control request must not receive a tool-call overload response"
    );

    let dispatched = transport.dispatched_calls.lock().expect("dispatch lock");
    assert!(
        dispatched.contains_key(&rmcp::model::NumberOrString::Number(1)),
        "the tombstone must survive the rejected control request"
    );
}

#[test]
fn retained_tombstones_are_bounded() {
    let dispatch = RequestAdmission::new(1);
    let transport = BoundedStdioTransport::new(dispatch.clone(), McpResultMode::Dual);
    let bound = RETAINED_TOMBSTONE_MULTIPLIER;

    for id in 0..bound {
        let mut tool = incoming_request(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": "files", "arguments": {}}
        }));
        transport.admit_message(&mut tool).expect("admit tool call");
        let mut cancellation = incoming_request(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": id, "reason": "no longer needed"}
        }));
        transport
            .admit_message(&mut cancellation)
            .expect("admit cancellation");
    }

    let mut excess = incoming_request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": bound,
        "method": "tools/call",
        "params": {"name": "files", "arguments": {}}
    }));
    assert!(
        transport.admit_message(&mut excess).is_err(),
        "admission must be rejected once retained entries reach the bound"
    );
    let dispatched = transport.dispatched_calls.lock().expect("dispatch lock");
    assert_eq!(
        dispatched.len(),
        bound,
        "cancelled-but-draining entries must not grow past the bound"
    );
}

#[test]
fn native_rmcp_codec_recovers_after_the_bounded_frame_limit() {
    let mut transport = BoundedStdioTransport::new(RequestAdmission::new(1), McpResultMode::Dual);
    transport
        .read_buffer
        .extend(std::iter::repeat_n(b'x', MAX_MCP_STDIO_FRAME_BYTES + 1));
    transport.read_buffer.extend_from_slice(b"\n");
    let partial = br#"{"jsonrpc""#;
    transport.read_buffer.extend_from_slice(partial);

    assert!(matches!(
        transport.decoder.decode(&mut transport.read_buffer),
        Err(JsonRpcMessageCodecError::MaxLineLengthExceeded)
    ));
    assert!(
        transport
            .decoder
            .decode(&mut transport.read_buffer)
            .expect("discard oversized line")
            .is_none()
    );
    assert_eq!(&transport.read_buffer[..], partial);
    assert!(transport.read_buffer.capacity() > RETAINED_MCP_FRAME_CAPACITY);
    transport.release_oversized_read_buffer();
    assert_eq!(&transport.read_buffer[..], partial);
    assert!(transport.read_buffer.capacity() <= RETAINED_MCP_FRAME_CAPACITY);

    transport.read_buffer.extend_from_slice(
        br#":"2.0","id":1,"method":"ping"}
"#,
    );
    let recovered = transport
        .decoder
        .decode(&mut transport.read_buffer)
        .expect("discard oversized line and decode the next frame")
        .expect("valid frame after oversized line");
    assert!(matches!(recovered, JsonRpcMessage::Request(_)));
    transport.release_oversized_read_buffer();
    assert!(transport.read_buffer.capacity() <= RETAINED_MCP_FRAME_CAPACITY);
}
