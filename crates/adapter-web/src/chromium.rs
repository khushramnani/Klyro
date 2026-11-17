//! Chromium engine: finds and starts Chrome, Chromium or Edge.

use crate::cdp::engine_err;
use crate::engine::Proc;
use klyro_core::Result;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// `KLYRO_CHROME`, else the usual install locations.
pub fn find_chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KLYRO_CHROME") {
        return Some(p.into());
    }
    let mut candidates: Vec<PathBuf> = vec![];
    for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            let base = PathBuf::from(base);
            candidates.push(base.join(r"Google\Chrome\Application\chrome.exe"));
            candidates.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
        }
    }
    candidates.extend(
        [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ]
        .map(PathBuf::from),
    );
    candidates.into_iter().find(|p| p.exists())
}

/// Starts the browser; returns it and its browser-level CDP WebSocket URL.
/// `headed` shows a normal window (to watch or debug); default is headless.
pub(crate) fn start(headed: bool) -> Result<(Proc, String)> {
    let exe = find_chrome().ok_or_else(|| engine_err("Chrome, Chromium or Edge not found; set KLYRO_CHROME to its path"))?;
    static N: AtomicU32 = AtomicU32::new(0);
    let profile = std::env::temp_dir().join(format!("klyro-chrome-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&profile);
    let profile_dir = profile.clone();

    let mut cmd = Command::new(&exe);
    if !headed {
        cmd.arg("--headless=new");
    }
    cmd.args(["--remote-debugging-port=0", "--no-first-run", "--no-default-browser-check"])
        // Chrome throttles pages it thinks are in the background (screenshots then hang under
        // load); an automated page is never in the background. Same defaults as Puppeteer.
        .args(["--disable-background-timer-throttling", "--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding"])
        // A throwaway profile needs no component downloads, sync or extensions (each fresh
        // profile otherwise pulls ~150 MB of models and lists). Same defaults as Puppeteer.
        .args(["--disable-background-networking", "--disable-component-update", "--disable-default-apps",
               "--disable-extensions", "--disable-sync", "--no-pings", "--metrics-recording-only"])
        .arg(format!("--user-data-dir={}", profile.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Containers and some CI hosts block Chrome's sandbox.
    if std::env::var_os("KLYRO_NO_SANDBOX").is_some() {
        cmd.arg("--no-sandbox");
    }
    let child = cmd.spawn().map_err(|e| engine_err(format!("could not start {}: {e}", exe.display())))?;
    let mut proc = Proc::new(child, move || {
        // Windows keeps files locked briefly after Chrome exits; retry instead of leaking the profile.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&profile_dir).is_ok() || !profile_dir.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("could not remove {}", profile_dir.display());
    });
    let ws = wait_for_port(&mut proc, &profile, &exe)?;
    Ok((proc, ws))
}


/// Chrome writes "<port>\n<browser ws path>" to DevToolsActivePort once it listens.
fn wait_for_port(proc: &mut Proc, profile: &Path, exe: &Path) -> Result<String> {
    let file = profile.join("DevToolsActivePort");
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if let Ok(s) = std::fs::read_to_string(&file) {
            let mut lines = s.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                return Ok(format!("ws://127.0.0.1:{port}{path}"));
            }
        }
        if let Ok(Some(status)) = proc.child.try_wait() {
            return Err(engine_err(format!("{} exited early ({status}); try KLYRO_NO_SANDBOX=1", exe.display())));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(engine_err(format!("{} did not open a debugging port", exe.display())))
}

