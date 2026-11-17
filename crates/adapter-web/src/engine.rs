//! Which browser engine runs a session, and the guard that stops its process.

use crate::{chromium, lightpanda};
use klyro_core::{Error, Result};
use std::fmt;
use std::process::Child;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// Visual lane: real layout and pixels.
    Chromium,
    /// Fast lane: no layout, no real pixels, much lighter.
    Lightpanda,
}

impl Engine {
    /// Real layout (element boxes) and real screenshots.
    pub fn has_layout(self) -> bool {
        self == Engine::Chromium
    }

    pub(crate) fn start(self, headed: bool) -> Result<(Proc, String)> {
        match self {
            Engine::Chromium => chromium::start(headed),
            Engine::Lightpanda if headed => Err(Error::Unsupported("--headed with the lightpanda engine (it has no window)")),
            Engine::Lightpanda => lightpanda::start(),
        }
    }
}

impl FromStr for Engine {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "chromium" | "chrome" => Ok(Engine::Chromium),
            "lightpanda" => Ok(Engine::Lightpanda),
            _ => Err(format!("unknown engine '{s}': use chromium or lightpanda")),
        }
    }
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Engine::Chromium => "chromium",
            Engine::Lightpanda => "lightpanda",
        })
    }
}

/// A browser process; killed (plus engine-specific cleanup) on drop.
pub(crate) struct Proc {
    pub child: Child,
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}

impl Proc {
    pub fn new(child: Child, cleanup: impl FnOnce() + Send + 'static) -> Self {
        Proc { child, cleanup: Some(Box::new(cleanup)) }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(f) = self.cleanup.take() {
            f();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Engine;

    #[test]
    fn parse_engine() {
        assert_eq!("Chromium".parse::<Engine>(), Ok(Engine::Chromium));
        assert_eq!("lightpanda".parse::<Engine>(), Ok(Engine::Lightpanda));
        assert!("firefox".parse::<Engine>().is_err());
    }
}
