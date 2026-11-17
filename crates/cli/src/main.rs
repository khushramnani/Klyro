//! `klyro`: thin CLI. Sends one request to the daemon, starting it if needed.

mod doctor;

use clap::{ArgGroup, Parser, Subcommand};
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
    /// Open a URL, or a local PDF, Markdown or HTML file (starts the session if needed).
    Open {
        url: String,
        /// Show a real browser window, to watch or debug. Only applies when the session starts.
        #[arg(long)]
        headed: bool,
        /// Pin the engine: chromium (visual) or lightpanda (fast, no pixels). Default: automatic,
        /// starting on lightpanda when available. Only applies when the session starts.
        #[arg(long, env = "KLYRO_ENGINE")]
        engine: Option<String>,
    },
    /// Attach to a running app (Electron, or Chrome) started with --remote-debugging-port=PORT.
    /// The app keeps running after `close`.
    Connect { port: u16 },
    /// Print the page as a tree with refs, e.g. `- button "Login" [ref=e3]`.
    Snapshot {
        /// Interactive elements only, as a flat list.
        #[arg(short, long)]
        interactive: bool,
    },
    /// Click an element.
    Click {
        #[arg(value_name = "REF")]
        target: String,
    },
    /// Replace a text field's content.
    Fill {
        #[arg(value_name = "REF")]
        target: String,
        text: String,
    },
    /// Press a key, e.g. Enter, Tab, Control+a. Goes to REF if given, else the focused element.
    Press {
        key: String,
        #[arg(value_name = "REF")]
        target: Option<String>,
    },
    /// Print the page's readable text.
    Text,
    /// Save a PNG of the viewport, the full page, or one element.
    Screenshot {
        #[arg(default_value = "screenshot.png")]
        path: std::path::PathBuf,
        /// The whole page, not just the viewport.
        #[arg(long, conflicts_with = "target")]
        full: bool,
        /// Only this element.
        #[arg(long = "ref", value_name = "REF", conflicts_with = "page")]
        target: Option<String>,
        /// Only this page of a PDF (1-based).
        #[arg(long, conflicts_with = "full")]
        page: Option<u32>,
    },
    /// Wait for a delay, some text, or a URL.
    #[command(group(ArgGroup::new("until").required(true).args(["ms", "text", "url"])))]
    Wait {
        /// Milliseconds to wait.
        ms: Option<u64>,
        /// Wait until the page text contains this.
        #[arg(long)]
        text: Option<String>,
        /// Wait until the URL contains this.
        #[arg(long)]
        url: Option<String>,
        /// Give up after this many milliseconds.
        #[arg(long, default_value_t = 10_000)]
        timeout: u64,
    },
    /// Show the session's lane (engine) and every lane switch with its reason.
    Status,
    /// Close the session and its browser.
    Close,
    /// Check which engines this machine has, and the daemon.
    Doctor,
    /// Run the background daemon (started automatically).
    #[command(hide = true)]
    Daemon,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (mut headed, mut engine) = (false, None);
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
        Cmd::Doctor => return doctor::run(cli.json),
        Cmd::Open { url, headed: h, engine: e } => {
            headed = h;
            engine = e;
            // A local document: the daemon runs elsewhere, so send an absolute path.
            let url = match std::path::Path::new(&url) {
                p if p.is_file() => std::path::absolute(p).map(|p| p.display().to_string()).unwrap_or(url),
                _ => url,
            };
            ("open", vec![url])
        }
        Cmd::Snapshot { interactive } => ("snapshot", if interactive { vec!["interactive".into()] } else { vec![] }),
        Cmd::Click { target } => ("click", vec![target]),
        Cmd::Fill { target, text } => ("fill", vec![target, text]),
        Cmd::Press { key, target } => ("press", [Some(key), target].into_iter().flatten().collect()),
        Cmd::Text => ("text", vec![]),
        Cmd::Screenshot { path, full, target, page } => {
            // The daemon writes the file, so resolve relative paths against our cwd.
            let path = std::env::current_dir().map(|d| d.join(&path)).unwrap_or(path);
            let mut args = vec![path.display().to_string()];
            if full {
                args.push("full".into());
            }
            if let Some(r) = target {
                args.push(format!("ref={r}"));
            }
            if let Some(n) = page {
                args.push(format!("page={n}"));
            }
            ("screenshot", args)
        }
        Cmd::Wait { ms, text, url, timeout } => {
            let (kind, value) = match (ms, text, url) {
                (Some(ms), _, _) => ("ms", ms.to_string()),
                (_, Some(t), _) => ("text", t),
                (_, _, Some(u)) => ("url", u),
                _ => unreachable!("clap requires one"),
            };
            ("wait", vec![kind.into(), value, timeout.to_string()])
        }
        Cmd::Connect { port } => ("connect", vec![port.to_string()]),
        Cmd::Status => ("status", vec![]),
        Cmd::Close => ("close", vec![]),
    };
    let req = Request { token: String::new(), session: cli.session, cmd: cmd.into(), args, json: cli.json, headed, engine };
    // Only `open`/`connect` may start a daemon; other commands need an existing session anyway.
    match send(req, cmd == "open" || cmd == "connect") {
        Ok(Response { ok, out, notes }) => {
            for n in notes {
                eprintln!("note: switched lane: {n}");
            }
            if ok {
                println!("{out}");
                ExitCode::SUCCESS
            } else {
                eprintln!("error: {out}");
                ExitCode::FAILURE
            }
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
        return Ok(Response { ok: false, out: format!("no session '{}': run `klyro open <url>` first", req.session), notes: vec![] });
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
