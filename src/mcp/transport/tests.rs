use rmcp::{model::JsonRpcMessage, service::RxJsonRpcMessage};

use super::super::{
    DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY, McpResultMode, RequestAdmission, RoleServer,
};
use super::*;

fn incoming_request(value: serde_json::Value) -> RxJsonRpcMessage<RoleServer> {
    serde_json::from_value(value).expect("valid MCP request")
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
