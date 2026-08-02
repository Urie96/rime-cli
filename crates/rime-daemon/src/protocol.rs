//! Newline-delimited JSON-RPC 2.0 framing for the daemon.
//!
//! One JSON object per line. Requests carry an optional numeric `id`;
//! responses echo it. Notifications (no id) are not used by the current
//! client but tolerated.

use serde_json::{json, Value};

/// Parse one request line into `(id, method, params)`.
pub fn parse_request(line: &str) -> Option<(Option<u64>, String, Value)> {
    let value: Value = serde_json::from_str(line).ok()?;
    let method = value.get("method")?.as_str()?.to_string();
    let id = value.get("id").and_then(Value::as_u64);
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    Some((id, method, params))
}

/// Build a response line (with trailing newline).
pub fn response(id: Option<u64>, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string() + "\n"
}

/// Build an error response line.
pub fn error(id: Option<u64>, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
        + "\n"
}
