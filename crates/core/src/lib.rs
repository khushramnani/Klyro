//! Klyro core: what every surface (web, Electron, documents) shares.

mod a11y;
mod adapter;
mod diff;
mod refs;
mod snapshot;

pub use a11y::a11y;
pub use adapter::{Action, Adapter, Capabilities, Error, Result, ScreenshotOptions, Wait};
pub use diff::{Diff, diff};
pub use refs::RefMap;
pub use snapshot::{Node, Snapshot};
