//! Chromium engine: launches headless Chrome/Chromium/Edge and drives one page.

use crate::ax::{self, Handle};
use crate::cdp::{Cdp, engine_err};
use klyro_core::{Action, Adapter, Capabilities, Error, RefMap, Result, Snapshot};
use serde_json::json;
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
    // Declared last so the connection closes before the process is killed.
    _proc: Proc,
}

impl Chromium {
    pub fn launch() -> Result<Self> {
        let exe = find_chrome().ok_or_else(|| engine_err("Chrome, Chromium or Edge not found; set KLYRO_CHROME to its path"))?;
        static N: AtomicU32 = AtomicU32::new(0);
        let profile = std::env::temp_dir().join(format!("klyro-chrome-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&profile);

        let mut cmd = Command::new(&exe);
        cmd.args(["--headless=new", "--remote-debugging-port=0", "--no-first-run", "--no-default-browser-check"])
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
        Ok(Self { cdp, refs: RefMap::new(), _proc: proc })
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
        self.cdp.wait_event("Page.loadEventFired", LOAD_TIMEOUT)?;
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

    fn act(&mut self, _target: Option<&str>, _action: &Action) -> Result<()> {
        Err(Error::Unsupported("act")) // Task 5
    }

    fn text(&mut self) -> Result<String> {
        Err(Error::Unsupported("text")) // Task 5
    }

    fn close(&mut self) -> Result<()> {
        // The process is killed when the adapter is dropped; ask politely first.
        self.cdp.session = None;
        let _ = self.cdp.call("Browser.close", json!({}));
        Ok(())
    }
}
