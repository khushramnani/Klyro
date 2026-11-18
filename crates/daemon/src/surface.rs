//! What a session drives: a web page (with automatic lanes) or a native desktop app.

use crate::router::Router;
use klyro_adapter_desktop::DesktopApp;
use klyro_core::{Action, Adapter, Capabilities, Result, ScreenshotOptions, Snapshot, Wait};
use serde_json::{Value, json};
use std::time::Duration;

// One per session, already on the heap (Arc<Mutex<_>>): the size difference costs nothing.
#[allow(clippy::large_enum_variant)]
pub enum Surface {
    Web(Router),
    App(DesktopApp),
}

impl Surface {
    fn adapter(&mut self) -> &mut dyn Adapter {
        match self {
            Surface::Web(r) => r,
            Surface::App(a) => a,
        }
    }

    pub fn last_snapshot(&self) -> Option<&Snapshot> {
        match self {
            Surface::Web(r) => r.last_snapshot(),
            Surface::App(a) => a.last_snapshot(),
        }
    }

    /// Lane switches since the last call (web only).
    pub fn take_notes(&mut self) -> Vec<String> {
        match self {
            Surface::Web(r) => r.take_notes(),
            Surface::App(_) => vec![],
        }
    }

    pub fn status(&self) -> String {
        match self {
            Surface::Web(r) => r.status(),
            Surface::App(a) => format!(
                "app: {} ({})",
                a.title(),
                if a.launched() { "started by klyro; close stops it" } else { "attached; close leaves it running" }
            ),
        }
    }

    pub fn status_json(&self) -> Value {
        match self {
            Surface::Web(r) => r.status_json(),
            Surface::App(a) => json!({ "app": a.title(), "launched": a.launched() }),
        }
    }
}

impl Adapter for Surface {
    fn capabilities(&self) -> Capabilities {
        match self {
            Surface::Web(r) => r.capabilities(),
            Surface::App(a) => a.capabilities(),
        }
    }
    fn open(&mut self, target: &str) -> Result<()> {
        self.adapter().open(target)
    }
    fn snapshot(&mut self) -> Result<Snapshot> {
        self.adapter().snapshot()
    }
    fn act(&mut self, target: Option<&str>, action: &Action) -> Result<()> {
        self.adapter().act(target, action)
    }
    fn text(&mut self) -> Result<String> {
        self.adapter().text()
    }
    fn wait(&mut self, cond: &Wait, timeout: Duration) -> Result<()> {
        self.adapter().wait(cond, timeout)
    }
    fn screenshot(&mut self, opts: &ScreenshotOptions) -> Result<Vec<u8>> {
        self.adapter().screenshot(opts)
    }
    fn close(&mut self) -> Result<()> {
        self.adapter().close()
    }
}
