//! Klyro web adapter: drives browsers over the Chrome DevTools Protocol.

mod ax;
mod cdp;
mod chromium;
mod engine;
mod handoff;
mod keys;
mod lightpanda;
mod page;

pub use chromium::find_chrome;
pub use engine::Engine;
pub use lightpanda::find_lightpanda;
pub use page::WebPage;
