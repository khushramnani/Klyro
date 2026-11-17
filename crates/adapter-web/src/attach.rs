//! Attach to a running Chromium-based app (Electron, or Chrome started with
//! --remote-debugging-port) instead of launching a browser.

use crate::cdp::{Cdp, engine_err};
use crate::page::WebPage;
use klyro_core::{Error, Result};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

impl WebPage {
    /// Attaches to the app's first window on 127.0.0.1:`port`. The app keeps running after
    /// `close`, and its window size is left alone.
    pub fn attach(port: u16) -> Result<Self> {
        let version = http_get_json(port, "/json/version")?;
        let ws = version["webSocketDebuggerUrl"]
            .as_str()
            .ok_or_else(|| engine_err(format!("port {port} answered, but is not a DevTools port (no webSocketDebuggerUrl)")))?;
        // The advertised host may be "localhost", which can resolve to IPv6 first; the app
        // listens on 127.0.0.1.
        let path = ws.splitn(4, '/').nth(3).unwrap_or("");
        let mut cdp = Cdp::connect(&format!("ws://127.0.0.1:{port}/{path}"))?;
        let targets = cdp.call("Target.getTargets", json!({}))?;
        let page = targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t["type"] == "page" && !t["url"].as_str().unwrap_or("").starts_with("devtools://"))
            .ok_or_else(|| engine_err(format!("the app on port {port} has no open window")))?;
        let attached = cdp.call("Target.attachToTarget", json!({ "targetId": page["targetId"], "flatten": true }))?;
        cdp.session = attached["sessionId"].as_str().map(String::from);
        cdp.call("Page.enable", json!({}))?;
        let main_frame = cdp.call("Page.getFrameTree", json!({}))?["frameTree"]["frame"]["id"].as_str().unwrap_or("").to_string();
        Ok(WebPage::attached(cdp, main_frame))
    }
}

/// Minimal HTTP/1.1 GET on localhost returning a JSON body (DevTools discovery endpoint).
fn http_get_json(port: u16, path: &str) -> Result<Value> {
    let nothing = || {
        Error::Engine(format!(
            "nothing is listening on port {port}: start the app with --remote-debugging-port={port}"
        ))
    };
    let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|_| nothing())?;
    s.set_read_timeout(Some(Duration::from_secs(5))).map_err(engine_err)?;
    write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").map_err(engine_err)?;
    // Read by Content-Length: some servers keep the connection open despite Connection: close.
    let mut r = BufReader::new(s);
    let (mut line, mut len) = (String::new(), 0usize);
    while r.read_line(&mut line).map_err(engine_err)? > 2 {
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
        line.clear();
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).map_err(engine_err)?;
    serde_json::from_slice(&body).map_err(|_| engine_err(format!("port {port} is not a DevTools port")))
}
