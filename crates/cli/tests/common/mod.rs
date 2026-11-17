//! Shared test helpers: the demo app server and a klyro runner with an isolated daemon.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

/// Browser tests take turns: parallel Chromes on a small machine starve each other and
/// CDP calls time out.
static ONE_BROWSER_AT_A_TIME: Mutex<()> = Mutex::new(());

#[path = "../../../../tests/support/demo_app.rs"]
mod demo;
pub use demo::demo_app;

/// `- textbox "Username" [ref=e2]` -> "e2", for the first line with a ref containing `needle`.
pub fn ref_of(snapshot: &str, needle: &str) -> String {
    let line = snapshot
        .lines()
        .find(|l| l.contains(needle) && l.contains("[ref="))
        .unwrap_or_else(|| panic!("no ref for {needle:?} in:\n{snapshot}"));
    let start = line.find("[ref=").unwrap() + 5;
    line[start..].split(']').next().unwrap().to_string()
}

pub struct Klyro {
    pub home: PathBuf,
    /// Engine for new sessions; "" = automatic lanes; None leaves KLYRO_ENGINE from the environment alone.
    pub engine: Option<&'static str>,
    // Dropped after Drop::drop has closed the sessions.
    _turn: MutexGuard<'static, ()>,
}

impl Klyro {
    pub fn new(name: &str) -> Self {
        let home = std::env::temp_dir().join(format!("klyro-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let turn = ONE_BROWSER_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        Klyro { home, engine: None, _turn: turn }
    }

    pub fn with_engine(name: &str, engine: &'static str) -> Self {
        let mut k = Self::new(&format!("{name}-{engine}"));
        k.engine = Some(engine);
        k
    }

    /// (success, stdout, stderr)
    pub fn run(&self, args: &[&str]) -> (bool, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_klyro"));
        cmd.args(args).env("KLYRO_HOME", &self.home);
        match self.engine {
            Some("") => {
                cmd.env_remove("KLYRO_ENGINE"); // automatic lanes
            }
            Some(e) => {
                cmd.env("KLYRO_ENGINE", e);
            }
            None => {}
        }
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
    }

    pub fn ok(&self, args: &[&str]) -> String {
        let (ok, out, err) = self.run(args);
        assert!(ok, "klyro {args:?} failed: {err}");
        out
    }

    pub fn fails(&self, args: &[&str]) -> String {
        let (ok, out, err) = self.run(args);
        assert!(!ok, "klyro {args:?} should fail, printed: {out}");
        err
    }
}

/// Lightpanda is optional: tests on it skip (loudly) where it is not installed.
pub fn have_lightpanda() -> bool {
    let found = klyro_adapter_web::find_lightpanda().is_some();
    if !found {
        eprintln!("SKIP: Lightpanda not installed");
    }
    found
}

impl Drop for Klyro {
    /// Never leave a daemon or browser behind, even when an assert fails.
    fn drop(&mut self) {
        for s in ["a", "b", "default"] {
            self.run(&["--session", s, "close"]);
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}
