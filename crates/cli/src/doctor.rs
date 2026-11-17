//! `klyro doctor`: what this machine can do. Runs in the CLI, no daemon needed.

use klyro_adapter_web::{find_chrome, find_lightpanda};
use klyro_daemon::ipc;
use serde_json::json;
use std::net::TcpStream;
use std::process::ExitCode;

pub fn run(as_json: bool) -> ExitCode {
    let chrome = find_chrome();
    let lightpanda = find_lightpanda();
    let daemon = ipc::read_info().filter(|i| TcpStream::connect(("127.0.0.1", i.port)).is_ok());
    let wsl_mirrored = cfg!(windows).then(wsl_mirrored);

    if as_json {
        println!(
            "{}",
            json!({
                "version": env!("CARGO_PKG_VERSION"),
                "state_dir": ipc::home(),
                "daemon_pid": daemon.as_ref().map(|d| d.pid),
                "chromium": chrome,
                "lightpanda": lightpanda,
                "wsl_mirrored_networking": wsl_mirrored,
            })
        );
    } else {
        println!("klyro {}", env!("CARGO_PKG_VERSION"));
        println!("state dir: {}", ipc::home().display());
        match &daemon {
            Some(d) => println!("daemon: running (pid {})", d.pid),
            None => println!("daemon: not running (starts on the first `klyro open`)"),
        }
        match &chrome {
            Some(p) => println!("visual lane (chromium): ok  {}", p.display()),
            None => println!("visual lane (chromium): MISSING  install Chrome, Chromium or Edge, or set KLYRO_CHROME"),
        }
        let where_ = if cfg!(windows) { " (inside WSL)" } else { "" };
        match &lightpanda {
            Some(p) => println!("fast lane (lightpanda): ok  {p}{where_}"),
            None => println!(
                "fast lane (lightpanda): not installed  optional; sessions then run on chromium. https://lightpanda.io{}",
                if cfg!(windows) { " (install it inside WSL)" } else { "" }
            ),
        }
        if let (Some(false), Some(_)) = (wsl_mirrored, &lightpanda) {
            println!(
                "  WSL networking: NAT, so the fast lane can't reach servers on Windows' localhost.\n  \
                 Fix: add `networkingMode=mirrored` under [wsl2] in %USERPROFILE%\\.wslconfig, then `wsl --shutdown`"
            );
        }
        println!("documents (pdf, md, html): ok  built in, rendered on the visual lane");
        println!("electron apps: `klyro connect <port>` on apps started with --remote-debugging-port=<port>");
    }
    if chrome.is_some() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// `networkingMode=mirrored` in %USERPROFILE%\.wslconfig.
fn wsl_mirrored() -> bool {
    let Some(home) = std::env::var_os("USERPROFILE") else { return false };
    std::fs::read_to_string(std::path::Path::new(&home).join(".wslconfig")).is_ok_and(|s| {
        s.lines().any(|l| {
            let l: String = l.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
            l == "networkingmode=mirrored"
        })
    })
}
