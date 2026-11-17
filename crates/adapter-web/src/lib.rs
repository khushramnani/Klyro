//! Klyro web adapter: drives browsers over the Chrome DevTools Protocol.

mod ax;
mod cdp;
mod chromium;
mod keys;

pub use chromium::{Chromium, find_chrome};
