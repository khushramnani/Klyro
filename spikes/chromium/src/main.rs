//! Spike (Task 1): drive headless Chromium over raw CDP, print an
//! agent-browser style snapshot with refs, optionally click a ref.
//!
//! cargo run -p spike-chromium -- <url> [click <ref>]
// ponytail: throwaway spike, sync + unwrap-heavy on purpose; the real client lands in Task 3/4.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

type R<T> = Result<T, String>;

const INTERACTIVE: &[&str] = &[
    "link", "button", "textbox", "searchbox", "checkbox", "radio", "combobox", "listbox",
    "option", "menuitem", "tab", "switch", "slider", "spinbutton",
];
const SKIP: &[&str] = &["InlineTextBox", "LineBreak"];
const FLATTEN: &[&str] = &["generic", "none", "RootWebArea", "WebArea"];

struct Cdp {
    ws: WebSocket<TcpStream>,
    next: u64,
    session: Option<String>,
    events: Vec<Value>,
}

impl Cdp {
    fn connect(url: &str) -> R<Self> {
        let addr = url.trim_start_matches("ws://").split('/').next().unwrap().to_string();
        let tcp = TcpStream::connect(&addr).map_err(|e| e.to_string())?;
        tcp.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let (ws, _) = tungstenite::client(url, tcp).map_err(|e| e.to_string())?;
        Ok(Self { ws, next: 0, session: None, events: vec![] })
    }

    /// One frame, or None on read timeout.
    fn read(&mut self) -> R<Option<Value>> {
        match self.ws.read() {
            Ok(Message::Text(t)) => serde_json::from_str(t.as_str()).map(Some).map_err(|e| e.to_string()),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn call(&mut self, method: &str, params: Value) -> R<Value> {
        self.next += 1;
        let id = self.next;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = &self.session {
            msg["sessionId"] = json!(s);
        }
        eprintln!("[cdp] {method}");
        self.ws.send(Message::text(msg.to_string())).map_err(|e| e.to_string())?;
        let end = Instant::now() + Duration::from_secs(30);
        while Instant::now() < end {
            let Some(v) = self.read()? else { continue };
            if v["id"] == id {
                return match v.get("error") {
                    Some(e) => Err(format!("{method}: {e}")),
                    None => Ok(v["result"].clone()),
                };
            }
            if v.get("method").is_some() {
                self.events.push(v);
            }
        }
        Err(format!("{method}: timed out"))
    }

    fn wait_event(&mut self, name: &str, timeout: Duration) -> R<bool> {
        if let Some(i) = self.events.iter().position(|e| e["method"] == name) {
            self.events.remove(i);
            return Ok(true);
        }
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            match self.read()? {
                Some(v) if v["method"] == name => return Ok(true),
                Some(v) if v.get("method").is_some() => self.events.push(v),
                _ => {}
            }
        }
        Ok(false)
    }

    fn href(&mut self) -> R<String> {
        let r = self.call("Runtime.evaluate", json!({ "expression": "location.href", "returnByValue": true }))?;
        Ok(r["result"]["value"].as_str().unwrap_or("").to_string())
    }
}

/// Kills Chromium and removes its profile dir on drop, so no headless Chromes leak.
struct Chrome(Child, PathBuf);
impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

fn chrome_path() -> R<String> {
    if let Ok(p) = std::env::var("KLYRO_CHROME") {
        return Ok(p);
    }
    let candidates = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
    ];
    candidates
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|p| p.to_string())
        .ok_or_else(|| "no Chrome found; set KLYRO_CHROME".into())
}

fn launch() -> R<(Chrome, String)> {
    let dir = std::env::temp_dir().join(format!("klyro-spike-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let child = Command::new(chrome_path()?)
        .args(["--headless=new", "--remote-debugging-port=0", "--no-first-run", "--no-default-browser-check"])
        .arg(format!("--user-data-dir={}", dir.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let chrome = Chrome(child, dir.clone());
    // Chrome writes "<port>\n<browser ws path>" here once the debugger is listening.
    let file = dir.join("DevToolsActivePort");
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if let Ok(s) = std::fs::read_to_string(&file) {
            let mut lines = s.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                return Ok((chrome, format!("ws://127.0.0.1:{port}{path}")));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("Chrome did not open a debugging port".into())
}

/// Prints the AX tree as `- role "name" [ref=eN]`, returns ref -> backendDOMNodeId.
fn snapshot(cdp: &mut Cdp) -> R<HashMap<String, i64>> {
    let tree = cdp.call("Accessibility.getFullAXTree", json!({}))?;
    let nodes = tree["nodes"].as_array().cloned().unwrap_or_default();
    let by_id: HashMap<&str, &Value> = nodes.iter().map(|n| (n["nodeId"].as_str().unwrap_or(""), n)).collect();
    let mut refs = HashMap::new();
    let mut out = String::new();

    fn walk(n: &Value, depth: usize, parent_name: &str, by_id: &HashMap<&str, &Value>, refs: &mut HashMap<String, i64>, out: &mut String) {
        let role = n["role"]["value"].as_str().unwrap_or("");
        let name = n["name"]["value"].as_str().unwrap_or("").trim();
        let ignored = n["ignored"].as_bool().unwrap_or(false);
        let mut child_depth = depth;
        let mut child_parent = parent_name;
        if !ignored && !SKIP.contains(&role) && !FLATTEN.contains(&role) {
            let pad = "  ".repeat(depth);
            if role == "StaticText" {
                if !name.is_empty() && name != parent_name {
                    out.push_str(&format!("{pad}- text: {name}\n"));
                }
                return;
            }
            let mut line = format!("{pad}- {role}");
            if !name.is_empty() {
                line.push_str(&format!(" \"{name}\""));
            }
            if let Some(b) = n["backendDOMNodeId"].as_i64().filter(|_| !name.is_empty() || INTERACTIVE.contains(&role)) {
                let r = format!("e{}", refs.len() + 1);
                line.push_str(&format!(" [ref={r}]"));
                refs.insert(r, b);
            }
            if let Some(level) = n["properties"].as_array().and_then(|ps| ps.iter().find(|p| p["name"] == "level")) {
                line.push_str(&format!(" [level={}]", level["value"]["value"]));
            }
            out.push_str(&line);
            out.push('\n');
            child_depth = depth + 1;
            child_parent = name;
        }
        for c in n["childIds"].as_array().into_iter().flatten() {
            if let Some(child) = c.as_str().and_then(|id| by_id.get(id)) {
                walk(child, child_depth, child_parent, by_id, refs, out);
            }
        }
    }

    if let Some(root) = nodes.first() {
        walk(root, 0, "", &by_id, &mut refs, &mut out);
    }
    print!("{out}");
    Ok(refs)
}

/// Real mouse click at the element's centre. Needs layout, so Lightpanda will want a JS `this.click()` variant.
fn click(cdp: &mut Cdp, backend: i64) -> R<()> {
    cdp.call("DOM.getDocument", json!({ "depth": 0 }))?;
    cdp.call("DOM.scrollIntoViewIfNeeded", json!({ "backendNodeId": backend }))?;
    let bm = cdp.call("DOM.getBoxModel", json!({ "backendNodeId": backend }))?;
    let q: Vec<f64> = bm["model"]["content"].as_array().ok_or("no box model")?.iter().filter_map(|v| v.as_f64()).collect();
    let (x, y) = ((q[0] + q[2] + q[4] + q[6]) / 4.0, (q[1] + q[3] + q[5] + q[7]) / 4.0);
    for t in ["mouseMoved", "mousePressed", "mouseReleased"] {
        cdp.call("Input.dispatchMouseEvent", json!({ "type": t, "x": x, "y": y, "button": "left", "clickCount": 1 }))?;
    }
    Ok(())
}

fn run() -> R<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let url = args.first().ok_or("usage: spike-chromium <url> [click <ref>]")?;
    let (_chrome, ws) = launch()?;
    let mut cdp = Cdp::connect(&ws)?;

    let target = cdp.call("Target.createTarget", json!({ "url": "about:blank" }))?;
    let attached = cdp.call("Target.attachToTarget", json!({ "targetId": target["targetId"], "flatten": true }))?;
    cdp.session = attached["sessionId"].as_str().map(String::from);

    cdp.call("Page.enable", json!({}))?;
    cdp.call("Page.navigate", json!({ "url": url }))?;
    if !cdp.wait_event("Page.loadEventFired", Duration::from_secs(30))? {
        eprintln!("warning: no load event within 30s");
    }
    let refs = snapshot(&mut cdp)?;

    if let (Some("click"), Some(r)) = (args.get(1).map(String::as_str), args.get(2)) {
        let r = r.trim_start_matches('@');
        let backend = *refs.get(r).ok_or_else(|| format!("unknown ref {r}"))?;
        println!("url before: {}", cdp.href()?);
        cdp.events.clear();
        click(&mut cdp, backend)?;
        if !cdp.wait_event("Page.loadEventFired", Duration::from_secs(10))? {
            eprintln!("note: click did not load a new page");
        }
        println!("url after:  {}", cdp.href()?);
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
