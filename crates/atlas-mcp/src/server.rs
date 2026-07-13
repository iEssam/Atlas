//! The MCP server loop: JSON-RPC 2.0 over stdio.
//!
//! Reads newline-delimited requests from stdin, dispatches the MCP methods
//! (`initialize`, `notifications/initialized`, `tools/list`, `tools/call`), and
//! writes responses to stdout. All logging goes to **stderr only** — stdout is
//! reserved for the protocol stream.
//!
//! Tool execution failures (service down, bad args, RPC errors) become MCP
//! `isError` tool results, not protocol errors, so the client model can read the
//! failure text. Unknown methods become JSON-RPC `-32601` errors.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::jsonrpc::{self, codes};
use crate::redact::Redactor;
use crate::tools::{self, Connection};

/// MCP protocol version this server implements. Echoed back on `initialize`.
const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "atlas-mcp";

/// Runs the stdio server loop until stdin closes. `reader`/`writer` are injected
/// so the framing can be driven by fixtures in tests.
pub fn run<R: BufRead, W: Write>(
    reader: R,
    mut writer: W,
    mut conn: Connection,
    red: Redactor,
) -> anyhow::Result<()> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let request = match jsonrpc::parse_line(&line) {
            Ok(r) => r,
            Err(err_response) => {
                write_message(&mut writer, &err_response)?;
                continue;
            }
        };

        // Notifications get no response.
        if request.is_notification() {
            tracing::debug!(method = %request.method, "notification");
            continue;
        }

        let id = request.id.clone().unwrap_or(Value::Null);
        let response = handle_request(&mut conn, &red, &request.method, &request.params, id);
        write_message(&mut writer, &response)?;
    }
    Ok(())
}

/// Writes one JSON-RPC message as a single compact line + flush.
fn write_message<W: Write>(writer: &mut W, message: &Value) -> anyhow::Result<()> {
    let line = serde_json::to_string(message)?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Dispatches a single request method to its handler, returning the response
/// value.
pub fn handle_request(
    conn: &mut Connection,
    red: &Redactor,
    method: &str,
    params: &Value,
    id: Value,
) -> Value {
    match method {
        "initialize" => jsonrpc::result_response(id, initialize_result()),
        "ping" => jsonrpc::result_response(id, json!({})),
        "tools/list" => jsonrpc::result_response(id, tools::tools_list()),
        "tools/call" => tools_call(conn, red, params, id),
        other => jsonrpc::error_response(
            id,
            codes::METHOD_NOT_FOUND,
            &format!("method not found: {other}"),
        ),
    }
}

/// The `initialize` result: protocol version, server identity, and the single
/// declared capability — `tools`.
fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "serverInfo": {
            "name": SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION"),
        },
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "instructions": "Read-only System Atlas evidence provider. Every tool result is redacted (MCP-strict, default-on) and self-describing (see the `grounding` block). Atlas supplies citation-ready evidence; it cannot guarantee the final answer is fully cited."
    })
}

/// Handles `tools/call`: validates the tool name, dispatches to the read-only
/// RPC, and packages the result as MCP content + structuredContent. Runtime
/// failures come back as `isError` results.
fn tools_call(conn: &mut Connection, red: &Redactor, params: &Value, id: Value) -> Value {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return jsonrpc::error_response(
                id,
                codes::INVALID_PARAMS,
                "tools/call requires a string `name`",
            )
        }
    };

    if !tools::CATALOG.iter().any(|t| t.name == name) {
        return jsonrpc::error_response(
            id,
            codes::INVALID_PARAMS,
            &format!("unknown tool: {name}"),
        );
    }

    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match tools::dispatch(conn, red, name, &args) {
        Ok(structured) => {
            let text = serde_json::to_string_pretty(&structured)
                .unwrap_or_else(|_| structured.to_string());
            jsonrpc::result_response(
                id,
                json!({
                    "content": [ { "type": "text", "text": text } ],
                    "structuredContent": structured,
                    "isError": false,
                }),
            )
        }
        Err(e) => {
            tracing::warn!(tool = %name, error = %e, "tool call failed");
            jsonrpc::result_response(
                id,
                json!({
                    "content": [ { "type": "text", "text": format!("tool '{name}' failed: {e}") } ],
                    "isError": true,
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_reports_tools_capability() {
        let r = initialize_result();
        assert_eq!(r["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(r["serverInfo"]["name"], SERVER_NAME);
        assert!(r["capabilities"]["tools"].is_object());
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        // No connection needed — the method is rejected before any RPC.
        let mut conn = Connection::new("unused".into()).unwrap();
        let red = Redactor::new(Default::default());
        let resp = handle_request(&mut conn, &red, "does/not/exist", &json!({}), json!(1));
        assert_eq!(resp["error"]["code"], codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn tools_list_returns_full_catalog() {
        let mut conn = Connection::new("unused".into()).unwrap();
        let red = Redactor::new(Default::default());
        let resp = handle_request(&mut conn, &red, "tools/list", &json!({}), json!(2));
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), tools::CATALOG.len());
    }

    #[test]
    fn tools_call_unknown_tool_is_invalid_params() {
        let mut conn = Connection::new("unused".into()).unwrap();
        let red = Redactor::new(Default::default());
        let resp = handle_request(
            &mut conn,
            &red,
            "tools/call",
            &json!({ "name": "no_such_tool", "arguments": {} }),
            json!(3),
        );
        assert_eq!(resp["error"]["code"], codes::INVALID_PARAMS);
    }

    #[test]
    fn full_framing_smoke_over_pipes() {
        // Drive initialize -> initialized (notification) -> tools/list through
        // the real stdio loop with in-memory buffers. No live service needed:
        // tools/list never touches the pipe.
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
        );
        let mut out: Vec<u8> = Vec::new();
        let conn = Connection::new("unused".into()).unwrap();
        let red = Redactor::new(Default::default());
        run(input.as_bytes(), &mut out, conn, red).unwrap();

        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // initialize response + tools/list response (notification produced none).
        assert_eq!(lines.len(), 2, "expected 2 responses, got: {text}");
        let init: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(init["id"], 1);
        assert_eq!(init["result"]["serverInfo"]["name"], SERVER_NAME);
        let list: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(list["id"], 2);
        assert!(list["result"]["tools"].as_array().unwrap().len() >= 11);
    }
}
