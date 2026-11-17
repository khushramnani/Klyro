//! `klyro`: thin CLI. Sends one request to the daemon, starting it if needed.

use clap::{Parser, Subcommand};
use klyro_daemon::ipc::{self, Request, Response};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "klyro", version, about = "See and use anything with a screen, from any agent.")]
struct Cli {
    /// Session name; sessions do not share pages or cookies.
    #[arg(long, global = true, default_value = "default", env = "KLYRO_SESSION")]
    session: String,
    /// JSON output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open a URL (starts the session if needed).
    Open { url: String },
    /// Print the page as a tree with refs, e.g. `- button "Login" [ref=e3]`.
    Snapshot,
    /// Close the session and its browser.
    Close,
    /// Run the background daemon (started automatically).
    #[command(hide = true)]
    Daemon,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (cmd, args) = match cli.cmd {
        Cmd::Daemon => {
            return match klyro_daemon::run() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("daemon: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        Cmd::Open { url } => ("open", vec![url]),
        Cmd::Snapshot => ("snapshot", vec![]),
        Cmd::Close => ("close", vec![]),
    };
    let req = Request { token: String::new(), session: cli.session, cmd: cmd.into(), args, json: cli.json };
    // Only `open` may start a daemon; other commands need an existing session anyway.
    match send(req, cmd == "open") {
        Ok(Response { ok: true, out }) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Ok(Response { ok: false, out }) => {
            eprintln!("error: {out}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn send(req: Request, may_start: bool) -> Result<Response, String> {
    if let Some(r) = try_send(&req) {
        return r;
    }
    if !may_start {
        return Ok(Response { ok: false, out: format!("no session '{}': run `klyro open <url>` first", req.session) });
    }
    start_daemon()?;
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if let Some(r) = try_send(&req) {
            return r;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("daemon did not start; see {}", ipc::log_path().display()))
}

/// None when no daemon is reachable.
fn try_send(req: &Request) -> Option<Result<Response, String>> {
    let info = ipc::read_info()?;
    let mut stream = TcpStream::connect(("127.0.0.1", info.port)).ok()?;
    let req = Request { token: info.token, ..req.clone() };
    let r = (|| {
        writeln!(stream, "{}", serde_json::to_string(&req).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).map_err(|e| e.to_string())?;
        serde_json::from_str(&line).map_err(|e| format!("bad daemon reply: {e}"))
    })();
    Some(r)
}

fn start_daemon() -> Result<(), String> {
    std::fs::create_dir_all(ipc::home()).map_err(|e| e.to_string())?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(ipc::log_path()).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon").stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    // Detach so Ctrl+C or closing the terminal does not kill the daemon.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x8;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        stop_std_handle_inheritance();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().map(|_| ()).map_err(|e| format!("could not start daemon: {e}"))
}

/// Windows spawns with handle inheritance on, so the long-lived daemon would otherwise keep
/// our stdout/stderr pipes open and a caller reading our output would wait for the daemon.
#[cfg(windows)]
fn stop_std_handle_inheritance() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(n: u32) -> *mut c_void;
        fn SetHandleInformation(h: *mut c_void, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE
    for n in [-10i32, -11, -12] {
        // SAFETY: plain Win32 calls on this process's own std handles; failures are harmless.
        unsafe {
            let h = GetStdHandle(n as u32);
            if !h.is_null() && h as isize != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}
