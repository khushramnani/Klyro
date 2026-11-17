//! Chromium engine: launches headless Chrome/Chromium/Edge and drives one page.

use crate::ax::{self, Handle};
use crate::cdp::{Cdp, engine_err};
use crate::keys;
use klyro_core::{Action, Adapter, Capabilities, Error, RefMap, Result, Snapshot, Wait};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// `KLYRO_CHROME`, else the usual install locations.
pub fn find_chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KLYRO_CHROME") {
        return Some(p.into());
    }
    let mut candidates: Vec<PathBuf> = vec![];
    for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            let base = PathBuf::from(base);
            candidates.push(base.join(r"Google\Chrome\Application\chrome.exe"));
            candidates.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
        }
    }
    candidates.extend(
        [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ]
        .map(PathBuf::from),
    );
    candidates.into_iter().find(|p| p.exists())
}

/// The browser process and its throwaway profile; both go away on drop.
struct Proc {
    child: Child,
    profile: PathBuf,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

pub struct Chromium {
    cdp: Cdp,
    refs: RefMap<Handle>,
    main_frame: String,
    // Declared last so the connection closes before the process is killed.
    _proc: Proc,
}

impl Chromium {
    /// `headed`: show a normal browser window (for a human watching or debugging); default is headless.
    pub fn launch(headed: bool) -> Result<Self> {
        let exe = find_chrome().ok_or_else(|| engine_err("Chrome, Chromium or Edge not found; set KLYRO_CHROME to its path"))?;
        static N: AtomicU32 = AtomicU32::new(0);
        let profile = std::env::temp_dir().join(format!("klyro-chrome-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&profile);

        let mut cmd = Command::new(&exe);
        if !headed {
            cmd.arg("--headless=new");
        }
        cmd.args(["--remote-debugging-port=0", "--no-first-run", "--no-default-browser-check"])
            .arg(format!("--user-data-dir={}", profile.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Containers and some CI hosts block Chrome's sandbox.
        if std::env::var_os("KLYRO_NO_SANDBOX").is_some() {
            cmd.arg("--no-sandbox");
        }
        let child = cmd.spawn().map_err(|e| engine_err(format!("could not start {}: {e}", exe.display())))?;
        let mut proc = Proc { child, profile };

        let ws = wait_for_port(&mut proc, &exe)?;
        let mut cdp = Cdp::connect(&ws)?;
        let target = cdp.call("Target.createTarget", json!({ "url": "about:blank" }))?;
        let attached = cdp.call("Target.attachToTarget", json!({ "targetId": target["targetId"], "flatten": true }))?;
        cdp.session = attached["sessionId"].as_str().map(String::from);
        cdp.call("Page.enable", json!({}))?;
        let main_frame = cdp.call("Page.getFrameTree", json!({}))?["frameTree"]["frame"]["id"].as_str().unwrap_or("").to_string();
        Ok(Self { cdp, refs: RefMap::new(), main_frame, _proc: proc })
    }
}

/// Chrome writes "<port>\n<browser ws path>" to DevToolsActivePort once it listens.
fn wait_for_port(proc: &mut Proc, exe: &Path) -> Result<String> {
    let file = proc.profile.join("DevToolsActivePort");
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if let Ok(s) = std::fs::read_to_string(&file) {
            let mut lines = s.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                return Ok(format!("ws://127.0.0.1:{port}{path}"));
            }
        }
        if let Ok(Some(status)) = proc.child.try_wait() {
            return Err(engine_err(format!("{} exited early ({status}); try KLYRO_NO_SANDBOX=1", exe.display())));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(engine_err(format!("{} did not open a debugging port", exe.display())))
}

impl Chromium {
    fn eval(&mut self, expr: &str) -> Result<Value> {
        let r = self.cdp.call("Runtime.evaluate", json!({ "expression": expr, "returnByValue": true }))?;
        if let Some(ex) = r.get("exceptionDetails") {
            return Err(Error::Engine(format!("page script error: {}", ex["text"].as_str().unwrap_or("?"))));
        }
        Ok(r["result"]["value"].clone())
    }

    fn loader(&mut self) -> Result<String> {
        let ft = self.cdp.call("Page.getFrameTree", json!({}))?;
        Ok(ft["frameTree"]["frame"]["loaderId"].as_str().unwrap_or("").to_string())
    }

    /// backendNodeId for a ref, or StaleRef if the page navigated since the snapshot.
    fn node(&mut self, r: &str) -> Result<i64> {
        let (loader, backend) = self.refs.resolve(r)?;
        if self.loader()? != loader {
            return Err(Error::StaleRef(r.trim_start_matches('@').into()));
        }
        Ok(backend)
    }

    /// Runs `function(){...}` with `this` = the element; returns its value.
    fn call_on(&mut self, r: &str, backend: i64, func: &str) -> Result<Value> {
        let obj = self
            .cdp
            .call("DOM.resolveNode", json!({ "backendNodeId": backend }))
            .map_err(|_| Error::StaleRef(r.trim_start_matches('@').into()))?;
        let res = self.cdp.call(
            "Runtime.callFunctionOn",
            json!({ "objectId": obj["object"]["objectId"], "functionDeclaration": func, "returnByValue": true }),
        )?;
        Ok(res["result"]["value"].clone())
    }

    /// A real mouse click at the element's centre, like a user.
    fn mouse_click(&mut self, backend: i64) -> Result<()> {
        self.cdp.call("DOM.scrollIntoViewIfNeeded", json!({ "backendNodeId": backend }))?;
        let bm = self.cdp.call("DOM.getBoxModel", json!({ "backendNodeId": backend }))?;
        let q: Vec<f64> = bm["model"]["content"].as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
        if q.len() < 8 {
            return Err(engine_err("element has no box"));
        }
        let (x, y) = ((q[0] + q[2] + q[4] + q[6]) / 4.0, (q[1] + q[3] + q[5] + q[7]) / 4.0);
        for t in ["mouseMoved", "mousePressed", "mouseReleased"] {
            self.cdp.call("Input.dispatchMouseEvent", json!({ "type": t, "x": x, "y": y, "button": "left", "clickCount": 1 }))?;
        }
        Ok(())
    }

    fn press(&mut self, spec: &str) -> Result<()> {
        let k = keys::parse(spec).map_err(Error::Engine)?;
        let down = if k.text.is_some() { "keyDown" } else { "rawKeyDown" };
        for t in [down, "keyUp"] {
            let mut p = json!({ "type": t, "key": k.key, "code": k.code, "windowsVirtualKeyCode": k.vk,
                                "nativeVirtualKeyCode": k.vk, "modifiers": k.modifiers });
            if let (Some(text), true) = (&k.text, t != "keyUp") {
                p["text"] = json!(text);
                p["unmodifiedText"] = json!(text);
            }
            self.cdp.call("Input.dispatchKeyEvent", p)?;
        }
        Ok(())
    }

    /// If the action started a main-frame navigation, wait until the new page has loaded.
    fn settle(&mut self) -> Result<()> {
        let main = self.main_frame.clone();
        let started = self.cdp.wait_for(
            |e| {
                matches!(e["method"].as_str(), Some("Page.frameStartedLoading" | "Page.frameRequestedNavigation"))
                    && e["params"]["frameId"] == main.as_str()
            },
            Duration::from_millis(500),
        )?;
        if started.is_none() {
            return Ok(()); // ponytail: navigations that start later than 500 ms need an explicit `wait`.
        }
        self.cdp.wait_for(
            |e| e["method"] == "Page.frameNavigated" && e["params"]["frame"]["parentId"].is_null(),
            LOAD_TIMEOUT,
        )?;
        self.poll(LOAD_TIMEOUT, "the page to load", |c| Ok(c.eval("document.readyState").is_ok_and(|v| v == "complete")))
    }

    fn poll(&mut self, timeout: Duration, what: &str, mut done: impl FnMut(&mut Self) -> Result<bool>) -> Result<()> {
        let end = Instant::now() + timeout;
        loop {
            if done(self)? {
                return Ok(());
            }
            if Instant::now() >= end {
                return Err(Error::Engine(format!("timed out after {}s waiting for {what}", timeout.as_secs_f32())));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn need_ref<'a>(target: Option<&'a str>, what: &str) -> Result<&'a str> {
    target.ok_or_else(|| Error::Engine(format!("{what} needs a ref, e.g. {what} e3")))
}

const FILL_JS: &str = "function() {
    const editable = 'value' in this && !['button','submit','checkbox','radio','file','reset'].includes(this.type);
    if (!editable && !this.isContentEditable) return false;
    this.scrollIntoView({ block: 'center' });
    this.focus();
    if (editable) this.value = ''; else this.textContent = '';
    return true;
}";

impl Adapter for Chromium {
    fn capabilities(&self) -> Capabilities {
        Capabilities { pixels: true, structure: true }
    }

    fn open(&mut self, url: &str) -> Result<()> {
        self.cdp.clear_events();
        let r = self.cdp.call("Page.navigate", json!({ "url": url }))?;
        if let Some(e) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
            return Err(Error::Engine(format!("could not open {url}: {e}")));
        }
        // ponytail: a page that never fires load is snapshotted as-is after the timeout.
        self.cdp.wait_for(|e| e["method"] == "Page.loadEventFired", LOAD_TIMEOUT)?;
        Ok(())
    }

    fn snapshot(&mut self) -> Result<Snapshot> {
        let ft = self.cdp.call("Page.getFrameTree", json!({}))?;
        let frame = &ft["frameTree"]["frame"];
        let loader = frame["loaderId"].as_str().unwrap_or("").to_string();
        let url = format!("{}{}", frame["url"].as_str().unwrap_or(""), frame["urlFragment"].as_str().unwrap_or(""));
        let tree = self.cdp.call("Accessibility.getFullAXTree", json!({}))?;
        let nodes = tree["nodes"].as_array().map(Vec::as_slice).unwrap_or_default();
        let (title, nodes) = ax::build(nodes, &loader, &mut self.refs);
        Ok(Snapshot { url, title, nodes })
    }

    fn act(&mut self, target: Option<&str>, action: &Action) -> Result<()> {
        self.cdp.clear_events();
        match action {
            Action::Click => {
                let r = need_ref(target, "click")?;
                let b = self.node(r)?;
                // Hidden or zero-size elements have no box; fall back to a DOM click.
                if self.mouse_click(b).is_err() {
                    self.call_on(r, b, "function(){ this.click() }")?;
                }
            }
            Action::Fill(text) => {
                let r = need_ref(target, "fill")?;
                let b = self.node(r)?;
                if self.call_on(r, b, FILL_JS)? != json!(true) {
                    return Err(Error::Engine(format!("@{} is not a text field", r.trim_start_matches('@'))));
                }
                // insertText fires real input events, so frameworks (React, Vue) see the change.
                self.cdp.call("Input.insertText", json!({ "text": text }))?;
            }
            Action::Press(key) => {
                if let Some(r) = target {
                    let b = self.node(r)?;
                    self.call_on(r, b, "function(){ this.focus() }")?;
                }
                self.press(key)?;
            }
        }
        self.settle()
    }

    fn text(&mut self) -> Result<String> {
        let v = self.eval("document.body ? document.body.innerText : ''")?;
        Ok(v.as_str().unwrap_or("").trim().to_string())
    }

    fn wait(&mut self, cond: &Wait, timeout: Duration) -> Result<()> {
        match cond {
            Wait::Ms(ms) => {
                std::thread::sleep(Duration::from_millis(*ms));
                Ok(())
            }
            Wait::Text(t) => self.poll(timeout, &format!("text {t:?}"), |c| Ok(c.text()?.contains(t.as_str()))),
            Wait::Url(u) => self.poll(timeout, &format!("URL containing {u:?}"), |c| {
                Ok(c.eval("location.href")?.as_str().is_some_and(|h| h.contains(u.as_str())))
            }),
        }
    }

    fn close(&mut self) -> Result<()> {
        // The process is killed when the adapter is dropped; ask politely first.
        self.cdp.session = None;
        let _ = self.cdp.call("Browser.close", json!({}));
        Ok(())
    }
}
