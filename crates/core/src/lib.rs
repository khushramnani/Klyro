//! Klyro core: what every surface (web, Electron, documents) shares.

mod adapter;
mod refs;
mod snapshot;

pub use adapter::{Action, Adapter, Capabilities, Error, Result, ScreenshotOptions, Wait};
pub use refs::RefMap;
pub use snapshot::{Node, Snapshot};
