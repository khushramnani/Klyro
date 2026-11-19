//! Picks a session's lane and moves it to the visual lane when a step needs it.
//!
//! Sessions start on the fast lane (Lightpanda) when it is available. A command that is
//! unsupported there, or fails there, moves the session to Chromium (cookies and URL carried
//! over) and runs again. `--engine` pins the lane and turns this off. The MVP never moves back.

use klyro_adapter_web::{Engine, WebPage, find_lightpanda};
use klyro_core::{Action, Adapter, Capabilities, Error, Node, Result, ScreenshotOptions, Snapshot, Wait};
use std::time::Duration;

pub struct Router {
    page: WebPage,
    auto: bool,
    headed: bool,
    switches: Vec<String>,
    /// Switches not yet shown to the user.
    unreported: Vec<String>,
    last_snapshot: Option<Snapshot>,
    /// The last snapshot from the lane before the switch, to translate its refs.
    before_switch: Option<Snapshot>,
    /// First ref number issued on the current lane; lower ones belong to an earlier lane.
    lane_ref_start: u32,
}

impl Router {
    /// `engine: None` = automatic lanes. `visual_first`: the first step is known to need the
    /// visual lane (a document), so skip starting the fast lane just to leave it.
    pub fn start(engine: Option<Engine>, headed: bool, visual_first: bool) -> Result<Self> {
        let auto = engine.is_none();
        let fast = !headed && !visual_first && find_lightpanda().is_some();
        let first = engine.unwrap_or(if fast { Engine::Lightpanda } else { Engine::Chromium });
        let mut switches = vec![];
        let page = match WebPage::launch(first, headed) {
            Ok(p) => p,
            Err(e) if auto && first == Engine::Lightpanda => {
                switches.push(format!("lightpanda -> chromium at start: {e}"));
                WebPage::launch(Engine::Chromium, headed)?
            }
            Err(e) => return Err(e),
        };
        Ok(Router { page, auto, headed, unreported: switches.clone(), switches, last_snapshot: None, before_switch: None, lane_ref_start: 1 })
    }

    /// A session on an app that is already running (e.g. Electron). Pinned: no lane switching.
    pub fn attach(port: u16) -> Result<Self> {
        let page = WebPage::attach(port)?;
        Ok(Router { page, auto: false, headed: true, switches: vec![], unreported: vec![], last_snapshot: None, before_switch: None, lane_ref_start: 1 })
    }

    pub fn lane(&self) -> Engine {
        self.page.engine()
    }

    pub fn switches(&self) -> &[String] {
        &self.switches
    }

    /// Switches since the last call, for the command's output.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.unreported)
    }

    /// Live-view frame from the current lane (none on the fast lane: it has no pixels).
    pub fn frame(&mut self) -> Option<Vec<u8>> {
        self.page.frame()
    }

    /// The last snapshot taken in this session, if any.
    pub fn last_snapshot(&self) -> Option<&Snapshot> {
        self.last_snapshot.as_ref()
    }

    pub fn status_json(&self) -> serde_json::Value {
        serde_json::json!({ "lane": self.lane().to_string(), "auto": self.auto, "switches": self.switches })
    }

    pub fn status(&self) -> String {
        let mode = if self.page.is_attached() {
            "attached to a running app"
        } else if self.auto {
            "auto"
        } else {
            "pinned by --engine"
        };
        let mut s = format!("lane: {} ({mode})", self.lane());
        if self.switches.is_empty() {
            s.push_str("\nswitches: none");
        } else {
            s.push_str("\nswitches:");
            for w in &self.switches {
                s.push_str(&format!("\n  {w}"));
            }
        }
        s
    }

    /// Moves to the visual lane, for checks the fast lane can't answer fully (layout; its
    /// accessibility tree leaves out images without alt text).
    pub fn need_chromium(&mut self, what: &'static str) -> Result<()> {
        if self.lane().has_layout() {
            return Ok(());
        }
        if !self.can_switch() {
            return Err(Error::Unsupported(what));
        }
        self.switch(&format!("{what} needs it"))
    }

    /// Layout pack: (viewport width, problems), on the visual lane.
    pub fn layout(&mut self, width: Option<u32>) -> Result<(u32, Vec<String>)> {
        self.need_chromium("the layout check")?;
        self.page.layout_problems(width)
    }

    fn can_switch(&self) -> bool {
        self.auto && self.lane() == Engine::Lightpanda
    }

    fn switch(&mut self, reason: &str) -> Result<()> {
        // A crashed fast lane can't tell its URL any more: fall back to the last one we saw.
        let url = self
            .page
            .url()
            .ok()
            .filter(|u| !u.is_empty())
            .or_else(|| self.last_snapshot.as_ref().map(|s| s.url.clone()))
            .unwrap_or_default();
        let cookies = self.page.cookies().unwrap_or_default();
        let form = self.page.form_values().unwrap_or_default();
        let mut next = WebPage::launch(Engine::Chromium, self.headed)?;
        self.lane_ref_start = self.page.next_ref();
        next.continue_refs_from(self.lane_ref_start);
        next.set_cookies(&cookies)?;
        let mut fields = 0;
        if !url.is_empty() && url != "about:blank" {
            next.open(&url)?;
            // Typed values would otherwise vanish with the reload (the screenshot would lie).
            fields = next.restore_form_values(&form).unwrap_or(0);
        }
        let line = format!(
            "{} -> chromium: {reason} (carried {} cookies, {fields} form fields and {url})",
            self.lane(),
            cookies.len()
        );
        eprintln!("router: {line}");
        self.switches.push(line.clone());
        self.unreported.push(line);
        self.page = next; // drops the fast-lane browser
        // Only refs from a snapshot of this same page can be mapped across.
        self.before_switch = self.last_snapshot.take().filter(|s| s.url == url);
        Ok(())
    }

    /// Runs `f`. On the fast lane, an unsupported operation or an engine failure moves the
    /// session to Chromium and runs `f` once more. Timeouts and ref errors are answers, not faults.
    fn routed<T>(&mut self, what: &str, mut f: impl FnMut(&mut Self) -> Result<T>) -> Result<T> {
        match f(self) {
            Err(e @ (Error::Unsupported(_) | Error::Engine(_))) if self.can_switch() => {
                let reason = match &e {
                    Error::Unsupported(_) => format!("{what} needs it"),
                    _ => format!("{what} failed on lightpanda ({e})"),
                };
                self.switch(&reason)?;
                f(self)
            }
            r => r,
        }
    }

    /// Runs `f` with `target`; a ref from before the switch is mapped to the same element here.
    fn with_ref<T>(&mut self, target: Option<&str>, f: impl Fn(&mut WebPage, Option<&str>) -> Result<T>) -> Result<T> {
        match f(&mut self.page, target) {
            Err(Error::UnknownRef(r)) => match self.translate(&r)? {
                Some(now) => f(&mut self.page, Some(&now)),
                // Issued on an earlier lane, but its element is not on this page any more.
                None if r[1..].parse::<u32>().is_ok_and(|n| n < self.lane_ref_start) => Err(Error::StaleRef(r)),
                None => Err(Error::UnknownRef(r)),
            },
            res => res,
        }
    }

    /// Old-lane ref -> this lane's ref for the element with the same role, name and position.
    fn translate(&mut self, r: &str) -> Result<Option<String>> {
        let Some(old) = &self.before_switch else { return Ok(None) };
        let old = flatten(&old.nodes);
        let Some(pos) = old.iter().position(|(rf, ..)| rf == r) else { return Ok(None) };
        let (_, role, name) = old[pos].clone();
        let nth = old[..pos].iter().filter(|(_, ro, n)| *ro == role && *n == name).count();
        let snap = self.page.snapshot()?;
        let now = flatten(&snap.nodes).into_iter().filter(|(_, ro, n)| *ro == role && *n == name).nth(nth).map(|(rf, ..)| rf);
        self.last_snapshot = Some(snap);
        Ok(now)
    }
}

/// (ref, role, name) for every node with a ref, in document order.
fn flatten(nodes: &[Node]) -> Vec<(String, String, String)> {
    let mut out = vec![];
    fn walk(nodes: &[Node], out: &mut Vec<(String, String, String)>) {
        for n in nodes {
            if let Some(r) = &n.r#ref {
                out.push((r.clone(), n.role.clone(), n.name.clone()));
            }
            walk(&n.children, out);
        }
    }
    walk(nodes, &mut out);
    out
}

impl Adapter for Router {
    fn capabilities(&self) -> Capabilities {
        Capabilities { pixels: self.lane().has_layout() || self.can_switch(), structure: true }
    }

    fn open(&mut self, url: &str) -> Result<()> {
        self.before_switch = None;
        self.routed("open", |r| r.page.open(url))
    }

    fn snapshot(&mut self) -> Result<Snapshot> {
        let s = self.routed("snapshot", |r| r.page.snapshot())?;
        self.last_snapshot = Some(s.clone());
        Ok(s)
    }

    fn act(&mut self, target: Option<&str>, action: &Action) -> Result<()> {
        let what = match action {
            Action::Click => "click",
            Action::Fill(_) => "fill",
            Action::Press(_) => "press",
        };
        self.routed(what, |r| r.with_ref(target, |p, t| p.act(t, action)))
    }

    fn text(&mut self) -> Result<String> {
        self.routed("text", |r| r.page.text())
    }

    fn wait(&mut self, cond: &Wait, timeout: Duration) -> Result<()> {
        self.routed("wait", |r| r.page.wait(cond, timeout))
    }

    fn screenshot(&mut self, opts: &ScreenshotOptions) -> Result<Vec<u8>> {
        self.routed("screenshot", |r| {
            r.with_ref(opts.target.as_deref(), |p, t| p.screenshot(&ScreenshotOptions { target: t.map(String::from), ..opts.clone() }))
        })
    }

    fn close(&mut self) -> Result<()> {
        self.page.close()
    }
}

#[cfg(test)]
mod tests {
    use super::flatten;
    use klyro_core::Node;

    #[test]
    fn flatten_keeps_ref_nodes_in_order() {
        let mut b = Node::new("button", "Go");
        b.r#ref = Some("e2".into());
        let mut h = Node::new("heading", "Hi");
        h.r#ref = Some("e1".into());
        let mut form = Node::new("form", "");
        form.children = vec![Node::new("text", "x"), b];
        let got = flatten(&[h, form]);
        assert_eq!(got, [("e1".into(), "heading".into(), "Hi".into()), ("e2".into(), "button".into(), "Go".into())]);
    }
}
