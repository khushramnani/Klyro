//! `klyro mcp`: the same commands as MCP tools, over stdio (newline-delimited JSON-RPC 2.0).
//! Tool results are the CLI's compact text; screenshots also come back as an image.

mod tools;

use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

/// Used when the client does not say which protocol version it speaks.
const PROTOCOL_VERSION: &str = "2025-06-18";

const INSTRUCTIONS: &str = "Klyro drives a browser, Electron apps and local documents. Loop: `open` a URL or \
file, `snapshot` with interactive=true to get refs like e4, act with `click`/`fill`/`press` on refs, and read \
the action's result: it lists only what changed (+ added, - removed, ~ changed) or the new page when one loaded. \
On `stale ref`/`unknown ref`, take a new snapshot. Use `screenshot` only when pixels matter. Close sessions when done.";

/// Serves until the input ends.
pub fn serve(input: impl BufRead, mut out: impl Write) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg),
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// One JSON-RPC message in, at most one out (notifications get no reply).
pub fn handle(msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    let params = &msg["params"];
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "klyro", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools::list() }),
        "tools/call" => tools::call(params["name"].as_str().unwrap_or(""), &params["arguments"]),
        _ if id.is_none() => return None, // a notification, e.g. notifications/initialized
        _ => return Some(error(id.unwrap_or(Value::Null), -32601, &format!("method not found: {method}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id?, "result": result }))
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(method: &str, params: Value) -> Value {
        handle(&json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params })).unwrap()
    }

    #[test]
    fn initialize_echoes_version_and_offers_tools() {
        let r = call("initialize", json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "t" } }));
        assert_eq!(r["id"], 7);
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert_eq!(r["result"]["serverInfo"]["name"], "klyro");
    }

    #[test]
    fn notifications_get_no_reply_and_unknown_methods_an_error() {
        assert!(handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
        assert_eq!(call("nope", json!({}))["error"]["code"], -32601);
        assert_eq!(call("ping", json!({}))["result"], json!({}));
    }

    #[test]
    fn tools_list_has_every_command_with_a_schema() {
        let r = call("tools/list", json!({}));
        let tools = r["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for n in ["open", "connect", "snapshot", "click", "fill", "press", "text", "wait", "screenshot", "status", "close"] {
            assert!(names.contains(&n), "missing tool {n}");
        }
        assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object" && t["description"].is_string()));
    }

    #[test]
    fn bad_arguments_are_tool_errors_not_protocol_errors() {
        let r = call("tools/call", json!({ "name": "click", "arguments": {} }));
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("ref"));
        let r = call("tools/call", json!({ "name": "fly", "arguments": {} }));
        assert_eq!(r["result"]["isError"], true);
    }

    #[test]
    fn serve_reads_lines_and_answers_in_order() {
        let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\nnot json\n";
        let mut out = vec![];
        serve(input.as_bytes(), &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[1]["error"]["code"], -32700);
    }
}
