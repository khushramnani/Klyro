//! Spike (Task 2): the Task 1 flow against Lightpanda's CDP server.
//!
//! cargo run -p spike-lightpanda -- <url> [click <ref>]
//! Windows: starts `~/lightpanda serve` inside WSL. Linux/macOS: runs `lightpanda` from PATH.
//! Set KLYRO_LIGHTPANDA_WS=ws://host:port/ to use an already running server instead.
// ponytail: throwaway spike; shares the CDP code with spike-chromium via #[path].

#[path = "../../chromium/src/cdp.rs"]
mod cdp;

use cdp::R;
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PORT: u16 = 9333;

/// Stops Lightpanda on drop. Killing wsl.exe does not reliably stop the Linux process, so pkill it too.
struct Lightpanda(Child);
impl Drop for Lightpanda {
    fn drop(&mut self) {
        if cfg!(windows) {
            let _ = Command::new("wsl").args(["-e", "pkill", "-f", &format!("lightpanda serve.*{PORT}")]).status();
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn launch() -> R<(Lightpanda, String)> {
    let port = PORT.to_string();
    let serve = ["serve", "--host", "127.0.0.1", "--port", &port];
    let mut cmd = if cfg!(windows) {
        // WSL2 forwards 127.0.0.1 ports to Windows.
        let mut c = Command::new("wsl");
        c.args(["-e", "bash", "-lc", &format!("exec ~/lightpanda {}", serve.join(" "))]);
        c
    } else {
        let mut c = Command::new("lightpanda");
        c.args(serve);
        c
    };
    let child = cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|e| format!("start lightpanda: {e}"))?;
    let lp = Lightpanda(child);
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if TcpStream::connect(("127.0.0.1", PORT)).is_ok() {
            return Ok((lp, format!("ws://127.0.0.1:{PORT}/")));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("Lightpanda did not open its CDP port".into())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = match std::env::var("KLYRO_LIGHTPANDA_WS") {
        Ok(ws) => cdp::run(&ws, &args),
        Err(_) => launch().and_then(|(_lp, ws)| cdp::run(&ws, &args)),
    };
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
