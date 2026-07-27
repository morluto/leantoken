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
