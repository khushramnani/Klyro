//! Klyro web adapter: drives browsers over the Chrome DevTools Protocol.

mod ax;
mod cdp;
mod chromium;

pub use chromium::{Chromium, find_chrome};
