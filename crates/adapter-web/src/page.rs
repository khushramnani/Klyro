//! One browser page driven over CDP. The engine only changes how the browser starts
//! and a few behaviours (see `Engine`).

use crate::ax::{self, Handle};
use crate::cdp::{Cdp, engine_err};
use crate::engine::{Engine, Proc};
use crate::keys;
use klyro_core::{Action, Adapter, Capabilities, Error, RefMap, Result, ScreenshotOptions, Snapshot, Wait};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

pub struct WebPage {
    engine: Engine,
    cdp: Cdp,
    refs: RefMap<Handle>,
    main_frame: String,
    // Declared last so the connection closes before the process is stopped.
    _proc: Proc,
}

impl WebPage {
    /// Starts the engine and opens one blank page on it.
    pub fn launch(engine: Engine, headed: bool) -> Result<Self> {
        let (proc, ws) = engine.start(headed)?;
        let mut cdp = Cdp::connect(&ws)?;
        let target = cdp.call("Target.createTarget", json!({ "url": "about:blank" }))?;
        let attached = cdp.call("Target.attachToTarget", json!({ "targetId": target["targetId"], "flatten": true }))?;
        cdp.session = attached["sessionId"].as_str().map(String::from);
        cdp.call("Page.enable", json!({}))?;
        if engine.has_layout() {
            cdp.call("Page.bringToFront", json!({}))?;
        }
        if engine.has_layout() && !headed {
            // Exact desktop viewport; the headless default is cramped. Headed keeps the user's window.
            cdp.call("Emulation.setDeviceMetricsOverride", json!({ "width": 1280, "height": 800, "deviceScaleFactor": 1, "mobile": false }))?;
        }
        let main_frame = cdp.call("Page.getFrameTree", json!({}))?["frameTree"]["frame"]["id"].as_str().unwrap_or("").to_string();
        Ok(Self { engine, cdp, refs: RefMap::new(), main_frame, _proc: proc })
    }

    pub fn engine(&self) -> Engine {
        self.engine
    }

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

/// Standard base64 (CDP binary payloads). Ignores whitespace; None on bad input.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let bytes: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let data = bytes.strip_suffix(b"==").or_else(|| bytes.strip_suffix(b"=")).unwrap_or(&bytes);
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in data {
        acc = (acc << 6) | val(c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
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

impl Adapter for WebPage {
    fn capabilities(&self) -> Capabilities {
        Capabilities { pixels: self.engine.has_layout(), structure: true }
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
                // Engines without layout report fake boxes, so they always use a DOM click.
                // Hidden or zero-size elements have no box either.
                if !self.engine.has_layout() || self.mouse_click(b).is_err() {
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

    fn screenshot(&mut self, opts: &ScreenshotOptions) -> Result<Vec<u8>> {
        if !self.engine.has_layout() {
            return Err(Error::Unsupported("screenshot on the lightpanda engine (no real pixels); use --engine chromium"));
        }
        if opts.page.is_some() {
            return Err(Error::Unsupported("screenshot --page on a web page (it is for documents)"));
        }
        let mut params = json!({ "format": "png" });
        if let Some(r) = &opts.target {
            let b = self.node(r)?;
            self.cdp.call("DOM.scrollIntoViewIfNeeded", json!({ "backendNodeId": b }))?;
            let bm = self
                .cdp
                .call("DOM.getBoxModel", json!({ "backendNodeId": b }))
                .map_err(|_| Error::Engine(format!("@{} is not visible, so it has no pixels", r.trim_start_matches('@'))))?;
            let q: Vec<f64> = bm["model"]["border"].as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
            let (xs, ys) = (q.iter().step_by(2), q.iter().skip(1).step_by(2));
            let (x0, x1) = (xs.clone().fold(f64::MAX, |a, &b| a.min(b)), xs.fold(f64::MIN, |a, &b| a.max(b)));
            let (y0, y1) = (ys.clone().fold(f64::MAX, |a, &b| a.min(b)), ys.fold(f64::MIN, |a, &b| a.max(b)));
            if q.len() < 8 || x1 - x0 < 1.0 || y1 - y0 < 1.0 {
                return Err(Error::Engine(format!("@{} has no size, so it has no pixels", r.trim_start_matches('@'))));
            }
            // Box quads are viewport-relative; the clip is page-relative.
            let vp = &self.cdp.call("Page.getLayoutMetrics", json!({}))?["cssLayoutViewport"];
            let (px, py) = (vp["pageX"].as_f64().unwrap_or(0.0), vp["pageY"].as_f64().unwrap_or(0.0));
            params["clip"] = json!({ "x": x0 + px, "y": y0 + py, "width": x1 - x0, "height": y1 - y0, "scale": 1 });
            params["captureBeyondViewport"] = json!(true);
        } else if opts.full_page {
            let size = &self.cdp.call("Page.getLayoutMetrics", json!({}))?["cssContentSize"];
            let (w, h) = (size["width"].as_f64().unwrap_or(1280.0), size["height"].as_f64().unwrap_or(800.0));
            params["clip"] = json!({ "x": 0, "y": 0, "width": w, "height": h, "scale": 1 });
            params["captureBeyondViewport"] = json!(true);
        }
        let shot = self.cdp.call("Page.captureScreenshot", params)?;
        base64_decode(shot["data"].as_str().unwrap_or("")).ok_or_else(|| engine_err("screenshot data was not base64"))
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

#[cfg(test)]
mod tests {
    use super::base64_decode;

    #[test]
    fn base64() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert_eq!(base64_decode("YWJj").unwrap(), b"abc");
        assert_eq!(base64_decode("iVBO\nRw0K").unwrap(), [0x89, b'P', b'N', b'G', 0x0d, 0x0a]);
        assert!(base64_decode("a!b=").is_none());
    }
}
