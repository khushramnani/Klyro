//! Klyro web adapter: drives browsers over the Chrome DevTools Protocol.

mod attach;
mod ax;
mod cdp;
mod docs;
mod chromium;
mod engine;
mod handoff;
mod keys;
mod layout;
mod lightpanda;
mod page;
mod view;

pub use chromium::find_chrome;
pub use engine::Engine;
pub use lightpanda::find_lightpanda;
pub use page::{WebPage, base64_decode, base64_encode};
pub use view::Pointer;
