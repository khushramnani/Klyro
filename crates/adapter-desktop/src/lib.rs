//! Klyro adapter for native desktop apps. Windows only for now (UI Automation, via the
//! `uiautomation` crate); elsewhere every call returns `Unsupported`.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::DesktopApp;

mod keys;
pub use keys::to_send_keys;

#[cfg(not(windows))]
mod other {
    use klyro_core::{Action, Adapter, Capabilities, Error, Result, Snapshot, Wait};
    use std::time::Duration;

    const NOT_YET: &str = "native desktop apps (Windows only for now)";

    /// Placeholder so callers compile everywhere.
    pub struct DesktopApp;

    impl DesktopApp {
        pub fn attach(_title: &str) -> Result<Self> {
            Err(Error::Unsupported(NOT_YET))
        }
        pub fn launch(_command: &[String]) -> Result<Self> {
            Err(Error::Unsupported(NOT_YET))
        }
        pub fn title(&self) -> &str {
            ""
        }
        pub fn launched(&self) -> bool {
            false
        }
        pub fn last_snapshot(&self) -> Option<&Snapshot> {
            None
        }
    }

    impl Adapter for DesktopApp {
        fn capabilities(&self) -> Capabilities {
            Capabilities { pixels: false, structure: false }
        }
        fn open(&mut self, _: &str) -> Result<()> {
            Err(Error::Unsupported(NOT_YET))
        }
        fn snapshot(&mut self) -> Result<Snapshot> {
            Err(Error::Unsupported(NOT_YET))
        }
        fn act(&mut self, _: Option<&str>, _: &Action) -> Result<()> {
            Err(Error::Unsupported(NOT_YET))
        }
        fn text(&mut self) -> Result<String> {
            Err(Error::Unsupported(NOT_YET))
        }
        fn wait(&mut self, _: &Wait, _: Duration) -> Result<()> {
            Err(Error::Unsupported(NOT_YET))
        }
        fn close(&mut self) -> Result<()> {
            Ok(())
        }
    }
}
#[cfg(not(windows))]
pub use other::DesktopApp;
