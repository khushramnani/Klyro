use crate::Snapshot;
use std::fmt;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The ref was never issued in this session.
    UnknownRef(String),
    /// The ref was issued, but its element is gone. Take a new snapshot.
    StaleRef(String),
    /// This adapter or engine cannot do the operation. The router uses this to switch lanes.
    Unsupported(&'static str),
    /// A wait condition did not happen in time. A normal answer, not an engine fault.
    Timeout(String),
    /// Anything else from the engine (CDP error, crash, lost connection).
    Engine(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::UnknownRef(r) => write!(f, "unknown ref @{r}: take a snapshot to get current refs"),
            Error::StaleRef(r) => write!(f, "stale ref @{r}: the element is gone, take a new snapshot"),
            Error::Unsupported(op) => write!(f, "{op} is not supported here"),
            Error::Timeout(m) | Error::Engine(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

/// What a surface can do. The router reads this to pick a lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Can produce real pixels (screenshots).
    pub pixels: bool,
    /// Can produce a structured tree with refs.
    pub structure: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Click,
    Fill(String),
    /// A key such as "Enter" or "Control+a". With no target it goes to the focused element.
    Press(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Wait {
    /// Fixed delay.
    Ms(u64),
    /// Until the page text contains this.
    Text(String),
    /// Until the URL contains this.
    Url(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScreenshotOptions {
    pub full_page: bool,
    /// Only this element.
    pub target: Option<String>,
    /// Document page, 1-based.
    pub page: Option<u32>,
}

/// One kind of screen. Every surface answers the same operations.
pub trait Adapter {
    fn capabilities(&self) -> Capabilities;
    /// Open a URL, file path, or other surface-specific target.
    fn open(&mut self, target: &str) -> Result<()>;
    fn snapshot(&mut self) -> Result<Snapshot>;
    /// `target` is a ref like "e3" (a leading '@' is allowed); `None` means the focused element.
    fn act(&mut self, target: Option<&str>, action: &Action) -> Result<()>;
    /// Readable text of the current page, not markup.
    fn text(&mut self) -> Result<String>;
    /// Block until the condition holds or `timeout` passes.
    fn wait(&mut self, cond: &Wait, timeout: Duration) -> Result<()>;
    /// PNG bytes.
    fn screenshot(&mut self, _opts: &ScreenshotOptions) -> Result<Vec<u8>> {
        Err(Error::Unsupported("screenshot"))
    }
    fn close(&mut self) -> Result<()>;
}
