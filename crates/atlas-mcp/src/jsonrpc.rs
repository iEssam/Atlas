//! Hand-rolled JSON-RPC 2.0 framing for MCP-over-stdio.
//!
//! MCP's stdio transport carries JSON-RPC 2.0 messages **newline-delimited**:
//! each message is a single UTF-8 JSON object on its own line, and a message
//! never contains an embedded newline. So framing is just "read a line, parse
//! it; write a compact line, flush". Requests carry an `id`; notifications
//! (e.g. `notifications/initialized`) omit it and get no response.
//!
//! This module owns only the envelope — parsing an incoming line into a
//! [`Request`], and building `result` / `error` response values. It has no
//! knowledge of MCP methods or Atlas RPCs.

use serde_json::{json, Value};

/// Standard JSON-RPC 2.0 error codes (plus MCP's use of them).
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
}

/// A parsed inbound JSON-RPC message.
#[derive(Clone, Debug)]
pub struct Request {
    /// Present for requests, absent for notifications.
    pub id: Option<Value>,
    pub method: String,
    /// `params` object (or null → empty object).
    pub params: Value,
}

impl Request {
    /// True when this is a notification (no `id`) and therefore expects no
    /// response.
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// Parse one line of input into a [`Request`].
///
/// Returns `Err` with a ready-to-send error response value when the line is not
/// a valid JSON-RPC request (bad JSON, missing/invalid `method`). The error
/// carries `id: null` when the id couldn't be recovered.
pub fn parse_line(line: &str) -> Result<Request, Value> {
    let value: Value = serde_json::from_str(line).map_err(|e| {
        error_response(
            Value::Null,
            codes::PARSE_ERROR,
            &format!("parse error: {e}"),
        )
    })?;

    let obj = value.as_object().ok_or_else(|| {
        error_response(
            Value::Null,
            codes::INVALID_REQUEST,
            "request must be a JSON object",
        )
    })?;

    // Recover id early so a malformed request can still be answered by id.
    let id = obj.get("id").cloned();

    let method = match obj.get("method").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return Err(error_response(
                id.unwrap_or(Value::Null),
                codes::INVALID_REQUEST,
                "missing or non-string `method`",
            ))
        }
    };

    let params = obj.get("params").cloned().unwrap_or_else(|| json!({}));

    Ok(Request { id, method, params })
}

/// Build a JSON-RPC 2.0 success response.
pub fn result_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Build a JSON-RPC 2.0 error response.
pub fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_with_params() {
        let req =
            parse_line(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"x"}}"#)
                .expect("valid request");
        assert_eq!(req.method, "tools/call");
        assert_eq!(req.id, Some(json!(1)));
        assert_eq!(req.params, json!({"name":"x"}));
        assert!(!req.is_notification());
    }

    #[test]
    fn parses_notification_without_id() {
        let req = parse_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .expect("valid notification");
        assert!(req.is_notification());
        // Missing params defaults to an empty object.
        assert_eq!(req.params, json!({}));
    }

    #[test]
    fn rejects_bad_json_with_parse_error() {
        let err = parse_line("{not json").expect_err("should fail");
        assert_eq!(err["error"]["code"], codes::PARSE_ERROR);
        assert_eq!(err["id"], Value::Null);
    }

    #[test]
    fn rejects_missing_method_but_keeps_id() {
        let err = parse_line(r#"{"jsonrpc":"2.0","id":7}"#).expect_err("should fail");
        assert_eq!(err["error"]["code"], codes::INVALID_REQUEST);
        assert_eq!(err["id"], json!(7));
    }

    #[test]
    fn result_and_error_shapes() {
        let ok = result_response(json!(1), json!({"a":1}));
        assert_eq!(ok["jsonrpc"], "2.0");
        assert_eq!(ok["result"], json!({"a":1}));

        let err = error_response(json!(2), codes::METHOD_NOT_FOUND, "nope");
        assert_eq!(err["error"]["code"], codes::METHOD_NOT_FOUND);
        assert_eq!(err["error"]["message"], "nope");
    }
}
