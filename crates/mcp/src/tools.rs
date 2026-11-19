//! MCP tools: one per CLI command, same daemon requests, same text.

use klyro_daemon::client::send;
use klyro_daemon::ipc::{self, Request};
use serde_json::{Value, json};

fn tool(name: &str, description: &str, props: Value, required: &[&str]) -> Value {
    let mut props = props;
    props["session"] = json!({ "type": "string", "description": "Session name (separate pages and cookies). Default: default" });
    json!({ "name": name, "description": description,
            "inputSchema": { "type": "object", "properties": props, "required": required } })
}

pub fn list() -> Vec<Value> {
    let r = json!({ "type": "string", "description": "Element ref from a snapshot, e.g. e4" });
    vec![
        tool("open", "Open a URL, or a local PDF, Markdown or HTML file. Starts the session if needed.", json!({
            "url": { "type": "string", "description": "URL or file path" },
            "engine": { "type": "string", "enum": ["chromium", "lightpanda"], "description": "Pin the engine for a new session (default: automatic)" },
            "headed": { "type": "boolean", "description": "Show a real browser window (new session only)" },
        }), &["url"]),
        tool("connect", "Attach to a running Electron app started with --remote-debugging-port=<port>.", json!({
            "port": { "type": "integer" },
        }), &["port"]),
        tool("app", "Drive a native Windows app: attach to an open window by `title`, or start `launch` (a program, with `args`).", json!({
            "title": { "type": "string", "description": "Part of an open window's title" },
            "launch": { "type": "string", "description": "Program to start instead (close stops it)" },
            "args": { "type": "array", "items": { "type": "string" } },
        }), &[]),
        tool("snapshot", "The page as a tree with refs, e.g. `- button \"Login\" [ref=e3]`.", json!({
            "interactive": { "type": "boolean", "description": "Interactive elements only, flat (smaller)" },
        }), &[]),
        tool("click", "Click an element. Returns what changed on the page.", json!({ "ref": r }), &["ref"]),
        tool("fill", "Replace a text field's content. Returns what changed.", json!({ "ref": r, "text": { "type": "string" } }), &["ref", "text"]),
        tool("press", "Press a key (Enter, Tab, Escape, ArrowDown, Control+a, ...), on `ref` or the focused element.", json!({
            "key": { "type": "string" }, "ref": r,
        }), &["key"]),
        tool("text", "The page's readable text (PDF: per page).", json!({}), &[]),
        tool("wait", "Wait for a delay, for text to appear, or for the URL to contain something. Give one of ms, text, url.", json!({
            "ms": { "type": "integer" }, "text": { "type": "string" }, "url": { "type": "string" },
            "timeout_ms": { "type": "integer", "description": "Default 10000" },
        }), &[]),
        tool("screenshot", "A PNG of the viewport, the full page, one element, or one PDF page. Returned as an image.", json!({
            "path": { "type": "string", "description": "Also save it here (default: a file in klyro's state dir)" },
            "full": { "type": "boolean" }, "ref": r, "page": { "type": "integer", "description": "PDF page, 1-based" },
        }), &[]),
        tool("status", "The session's engine (lane) and any automatic switches.", json!({}), &[]),
        tool("close", "Close the session (an attached app keeps running).", json!({}), &[]),
    ]
}

/// Runs a tool. Problems come back as `isError` results, so the model can read and react.
pub fn call(name: &str, args: &Value) -> Value {
    match run(name, args) {
        Ok(content) => json!({ "content": content, "isError": false }),
        Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
    }
}

fn run(name: &str, a: &Value) -> Result<Vec<Value>, String> {
    let s = |k: &str| a[k].as_str().map(String::from);
    let need = |k: &str| s(k).ok_or_else(|| format!("{name} needs `{k}`"));
    let mut req = Request {
        token: String::new(),
        session: s("session").unwrap_or_else(|| "default".into()),
        cmd: name.into(),
        args: vec![],
        json: false,
        headed: a["headed"].as_bool().unwrap_or(false),
        engine: s("engine"),
    };
    let mut shot = None;
    match name {
        "open" => {
            let url = need("url")?;
            // Local files: the daemon runs elsewhere, so send an absolute path.
            let p = std::path::Path::new(&url);
            let url = if p.is_file() { std::path::absolute(p).map(|p| p.display().to_string()).unwrap_or(url) } else { url };
            req.args = vec![url];
        }
        "connect" => req.args = vec![a["port"].as_u64().ok_or("connect needs `port`")?.to_string()],
        "app" => {
            req.args = match (s("launch"), s("title")) {
                (Some(p), _) => {
                    let extra = a["args"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from));
                    ["launch".to_string(), p].into_iter().chain(extra).collect()
                }
                (None, Some(t)) => vec!["attach".into(), t],
                _ => return Err("app needs `title` or `launch`".into()),
            }
        }
        "snapshot" if a["interactive"].as_bool() == Some(true) => req.args = vec!["interactive".into()],
        "click" => req.args = vec![need("ref")?],
        "fill" => req.args = vec![need("ref")?, need("text")?],
        "press" => req.args = [Some(need("key")?), s("ref")].into_iter().flatten().collect(),
        "wait" => {
            let (kind, value) = match (a["ms"].as_u64(), s("text"), s("url")) {
                (Some(ms), ..) => ("ms", ms.to_string()),
                (_, Some(t), _) => ("text", t),
                (.., Some(u)) => ("url", u),
                _ => return Err("wait needs one of `ms`, `text`, `url`".into()),
            };
            req.args = vec![kind.into(), value, a["timeout_ms"].as_u64().unwrap_or(10_000).to_string()];
        }
        "screenshot" => {
            let path = match s("path") {
                Some(p) => std::path::absolute(&p).map_err(|e| e.to_string())?,
                None => {
                    let dir = ipc::home().join("screenshots");
                    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                    dir.join(format!("shot-{ms}.png"))
                }
            };
            req.args = vec![path.display().to_string()];
            if a["full"].as_bool() == Some(true) {
                req.args.push("full".into());
            }
            if let Some(r) = s("ref") {
                req.args.push(format!("ref={r}"));
            }
            if let Some(n) = a["page"].as_u64() {
                req.args.push(format!("page={n}"));
            }
            shot = Some(path);
        }
        "snapshot" | "text" | "status" | "close" => {}
        other => return Err(format!("unknown tool {other}")),
    }

    let resp = send(req, matches!(name, "open" | "connect" | "app"))?;
    let mut text = resp.out;
    for n in &resp.notes {
        text.push_str(&format!("\nnote: {n}"));
    }
    if !resp.ok {
        return Err(text);
    }
    let mut content = vec![json!({ "type": "text", "text": text })];
    if let Some(path) = shot {
        let png = std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        content.insert(0, json!({ "type": "image", "data": base64(&png), "mimeType": "image/png" }));
    }
    Ok(content)
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64() {
        assert_eq!(super::base64(b"hello"), "aGVsbG8=");
        assert_eq!(super::base64(b"hi"), "aGk=");
        assert_eq!(super::base64(b"abc"), "YWJj");
        assert_eq!(super::base64(b""), "");
    }
}
