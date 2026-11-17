//! Lightpanda engine: a separate, unmodified program (AGPL-3.0) we only talk to over CDP.
//! Linux/macOS: runs natively. Windows: runs inside WSL.

use crate::cdp::engine_err;
use crate::engine::Proc;
use klyro_core::{Error, Result};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NOT_FOUND: &str = "the lightpanda engine: Lightpanda not found (install it, or set KLYRO_LIGHTPANDA to its path)";

/// `KLYRO_LIGHTPANDA`, else `lightpanda` on PATH or in the home dir. On Windows the path is
/// inside WSL.
pub fn find_lightpanda() -> Option<String> {
    if let Ok(p) = std::env::var("KLYRO_LIGHTPANDA") {
        return Some(p);
    }
    if cfg!(windows) {
        let out = Command::new("wsl")
            .args(["-e", "sh", "-c", r#"command -v lightpanda || { test -x "$HOME/lightpanda" && echo "$HOME/lightpanda"; }"#])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return (out.status.success() && !path.is_empty()).then_some(path);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .chain(home.iter().flat_map(|h| [h.join(".local/bin"), h.clone()]))
        .map(|dir| dir.join("lightpanda"))
        .find(|p| p.is_file())
        .map(|p| p.display().to_string())
}

pub(crate) fn start() -> Result<(Proc, String)> {
    let exe = find_lightpanda().ok_or(Error::Unsupported(NOT_FOUND))?;
    // ponytail: free-port probe has a tiny race with other programs; Lightpanda fails loudly if it loses.
    let port = TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map_err(engine_err)?.port().to_string();
    let serve = ["serve", "--host", "127.0.0.1", "--port", &port];

    let mut cmd = if cfg!(windows) {
        // WSL2 forwards the port to Windows. `env` because Windows env vars don't cross into WSL.
        let mut c = Command::new("wsl");
        c.args(["-e", "env", "LIGHTPANDA_DISABLE_TELEMETRY=true", &exe]).args(serve);
        c
    } else {
        let mut c = Command::new(&exe);
        c.env("LIGHTPANDA_DISABLE_TELEMETRY", "true").args(serve);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let child = cmd.spawn().map_err(|e| engine_err(format!("could not start Lightpanda: {e}")))?;
    let pattern = format!("serve --host 127.0.0.1 --port {port}$");
    let mut proc = Proc::new(child, move || {
        // Killing wsl.exe does not stop the Linux process.
        if cfg!(windows) {
            let _ = Command::new("wsl").args(["-e", "pkill", "-f", &pattern]).stdin(Stdio::null()).status();
        }
    });

    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).is_ok() {
            return Ok((proc, format!("ws://127.0.0.1:{port}/")));
        }
        if let Ok(Some(status)) = proc.child.try_wait() {
            return Err(engine_err(format!("Lightpanda exited early ({status})")));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(engine_err("Lightpanda did not open its CDP port"))
}
