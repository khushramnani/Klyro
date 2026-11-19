//! `klyro view`: frames sized for a terminal, and a human's mouse and keys at page coordinates.

use crate::cdp::engine_err;
use crate::page::WebPage;
use klyro_core::{Error, Result};
use serde_json::json;
use std::time::Duration;

/// A human input event from the view, in CSS pixels of the viewport.
#[derive(Debug, Clone, PartialEq)]
pub enum Pointer {
    Click { x: f64, y: f64 },
    Move { x: f64, y: f64 },
    Wheel { x: f64, y: f64, dy: f64 },
}

impl WebPage {
    /// (PNG, CSS viewport width, height): the visible viewport, scaled down in the browser so
    /// the image is at most `max_width` pixels wide. Never switches lanes, gives up after 3 s.
    pub fn view_frame(&mut self, max_width: u32) -> Result<(Vec<u8>, u32, u32)> {
        if !self.engine().has_layout() {
            return Err(Error::Unsupported("the live view"));
        }
        let quick = Duration::from_secs(3);
        let vp = self.cdp.call_timeout("Page.getLayoutMetrics", json!({}), quick)?["cssLayoutViewport"].clone();
        let (w, h) = (vp["clientWidth"].as_f64().unwrap_or(1280.0), vp["clientHeight"].as_f64().unwrap_or(800.0));
        let scale = (max_width as f64 / w).min(1.0);
        let clip = json!({ "x": vp["pageX"], "y": vp["pageY"], "width": w, "height": h, "scale": scale });
        let shot = self.cdp.call_timeout("Page.captureScreenshot", json!({ "format": "png", "clip": clip }), quick)?;
        let png = crate::page::base64_decode(shot["data"].as_str().unwrap_or("")).ok_or_else(|| engine_err("frame was not base64"))?;
        Ok((png, w as u32, h as u32))
    }

    pub fn view_pointer(&mut self, p: &Pointer) -> Result<()> {
        if !self.engine().has_layout() {
            return Err(Error::Unsupported("input in the live view"));
        }
        let mouse = |t: &str, x: f64, y: f64| json!({ "type": t, "x": x, "y": y, "button": "left", "clickCount": 1 });
        let events = match *p {
            Pointer::Click { x, y } => vec![mouse("mouseMoved", x, y), mouse("mousePressed", x, y), mouse("mouseReleased", x, y)],
            Pointer::Move { x, y } => vec![json!({ "type": "mouseMoved", "x": x, "y": y })],
            Pointer::Wheel { x, y, dy } => vec![json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": 0, "deltaY": dy })],
        };
        for e in events {
            self.cdp.call("Input.dispatchMouseEvent", e)?;
        }
        Ok(())
    }

    /// A key such as "a", "Enter" or "Control+a", to whatever has focus.
    pub fn view_key(&mut self, spec: &str) -> Result<()> {
        if !self.engine().has_layout() {
            return Err(Error::Unsupported("input in the live view"));
        }
        self.press(spec)
    }
}
