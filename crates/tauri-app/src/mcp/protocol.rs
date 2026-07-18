//! Pure MCP-over-JSON-RPC dispatch — no I/O, no app state. The HTTP layer
//! (server.rs) hands each POSTed JSON-RPC message to [`handle_message`];
//! tool metadata + execution come in through the [`ToolHost`] trait.
//!
//! Transport (streamable HTTP, stateless): one JSON-RPC message per POST.
//! The server never issues an `Mcp-Session-Id`, never streams SSE, and does
//! not accept the batch-array form.

use serde::Deserialize;
use serde_json::{json, Value};

/// Known protocol revisions. A requested version in this list is echoed
/// back; any other request gets the newest supported version.
const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema for the tool's arguments.
    pub input_schema: Value,
}

pub trait ToolHost {
    fn tools(&self) -> Vec<ToolDef>;
    /// Executes a tool. Ok(text) becomes a text content block; Err(msg)
    /// becomes an `isError: true` tool result, not a JSON-RPC error.
    fn call_tool(&self, name: &str, args: &Value) -> Result<String, String>;
}

#[derive(Deserialize)]
struct RpcIn {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// Handles one JSON-RPC message. Returns the response body, or None for
/// notifications (the HTTP layer answers 202 Accepted with no body).
pub fn handle_message(host: &dyn ToolHost, body: &[u8]) -> Option<Value> {
    let msg: RpcIn = match serde_json::from_slice(body) {
        Ok(m) => m,
        Err(e) => {
            return Some(error_response(Value::Null, -32700, &format!("parse error: {e}")))
        }
    };
    let id = match msg.id {
        Some(id) => id,
        None => return None, // notification (e.g. notifications/initialized)
    };
    let result = match msg.method.as_str() {
        "initialize" => initialize_result(&msg.params),
        "ping" => json!({}),
        "tools/list" => json!({
            "tools": host.tools().iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
            })).collect::<Vec<_>>()
        }),
        "tools/call" => return Some(tools_call(host, id, &msg.params)),
        // resources/prompts capabilities are not declared; the list methods
        // answer with empty lists.
        "resources/list" => json!({ "resources": [] }),
        "prompts/list" => json!({ "prompts": [] }),
        other => {
            return Some(error_response(id, -32601, &format!("method not found: {other}")))
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn initialize_result(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let version = if SUPPORTED_VERSIONS.contains(&requested) {
        requested
    } else {
        SUPPORTED_VERSIONS[0]
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "daisy",
            "title": "Daisy meeting library",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

fn tools_call(host: &dyn ToolHost, id: Value, params: &Value) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    if !host.tools().iter().any(|t| t.name == name) {
        return error_response(id, -32602, &format!("unknown tool: {name}"));
    }
    let (text, is_error) = match host.call_tool(name, &args) {
        Ok(t) => (t, false),
        Err(e) => (e, true),
    };
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": text }],
            "isError": is_error,
        }
    })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeHost;
    impl ToolHost for FakeHost {
        fn tools(&self) -> Vec<ToolDef> {
            vec![ToolDef {
                name: "echo",
                description: "echo back",
                input_schema: json!({"type":"object","properties":{"msg":{"type":"string"}}}),
            }]
        }
        fn call_tool(&self, name: &str, args: &Value) -> Result<String, String> {
            assert_eq!(name, "echo");
            match args.get("msg").and_then(|v| v.as_str()) {
                Some(m) => Ok(format!("echo: {m}")),
                None => Err("msg required".into()),
            }
        }
    }

    fn send(body: Value) -> Option<Value> {
        handle_message(&FakeHost, body.to_string().as_bytes())
    }

    #[test]
    fn initialize_echoes_known_version() {
        let r = send(json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2025-03-26","capabilities":{},
                      "clientInfo":{"name":"t","version":"0"}}}))
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert_eq!(r["result"]["serverInfo"]["name"], "daisy");
    }

    #[test]
    fn initialize_unknown_version_offers_newest() {
        let r = send(json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"1999-01-01"}}))
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    }

    #[test]
    fn notification_returns_none() {
        assert!(send(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
    }

    #[test]
    fn ping_pongs() {
        let r = send(json!({"jsonrpc":"2.0","id":7,"method":"ping"})).unwrap();
        assert!(r["result"].is_object());
    }

    #[test]
    fn tools_list_carries_schema() {
        let r = send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        assert_eq!(r["result"]["tools"][0]["name"], "echo");
        assert_eq!(r["result"]["tools"][0]["inputSchema"]["type"], "object");
    }

    #[test]
    fn tools_call_ok_and_error_paths() {
        let ok = send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"echo","arguments":{"msg":"hi"}}}))
        .unwrap();
        assert_eq!(ok["result"]["content"][0]["text"], "echo: hi");
        assert_eq!(ok["result"]["isError"], false);

        let err = send(json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"echo","arguments":{}}}))
        .unwrap();
        assert_eq!(err["result"]["isError"], true);
        assert_eq!(err["result"]["content"][0]["text"], "msg required");
    }

    #[test]
    fn unknown_tool_is_rpc_error() {
        let r = send(json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"nope","arguments":{}}}))
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn unknown_method_is_rpc_error() {
        let r = send(json!({"jsonrpc":"2.0","id":6,"method":"wat"})).unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }

    #[test]
    fn parse_error() {
        let r = handle_message(&FakeHost, b"{not json").unwrap();
        assert_eq!(r["error"]["code"], -32700);
    }
}
