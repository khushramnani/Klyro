//! Spike (Task 1): drive headless Chromium over raw CDP, print an
//! agent-browser style snapshot with refs, optionally click a ref.
//!
//! cargo run -p spike-chromium -- <url> [click <ref>]
// ponytail: throwaway spike, sync + unwrap-heavy on purpose; the real client lands in Task 3/4.

mod cdp;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cdp::R;

/// Kills Chromium and removes its profile dir on drop, so no headless Chromes leak.
struct Chrome(Child, PathBuf);
impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

fn chrome_path() -> R<String> {
    if let Ok(p) = std::env::var("KLYRO_CHROME") {
        return Ok(p);
    }
    let candidates = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
    ];
    candidates
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|p| p.to_string())
        .ok_or_else(|| "no Chrome found; set KLYRO_CHROME".into())
}

fn launch() -> R<(Chrome, String)> {
    let dir = std::env::temp_dir().join(format!("klyro-spike-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let child = Command::new(chrome_path()?)
        .args(["--headless=new", "--remote-debugging-port=0", "--no-first-run", "--no-default-browser-check"])
        .arg(format!("--user-data-dir={}", dir.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let chrome = Chrome(child, dir.clone());
    // Chrome writes "<port>\n<browser ws path>" here once the debugger is listening.
    let file = dir.join("DevToolsActivePort");
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if let Ok(s) = std::fs::read_to_string(&file) {
            let mut lines = s.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                return Ok((chrome, format!("ws://127.0.0.1:{port}{path}")));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("Chrome did not open a debugging port".into())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = launch().and_then(|(_chrome, ws)| cdp::run(&ws, &args));
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
