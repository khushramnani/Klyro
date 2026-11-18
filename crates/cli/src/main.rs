//! `klyro`: thin CLI. Sends one request to the daemon, starting it if needed.

mod doctor;

use clap::{ArgGroup, Parser, Subcommand};
use klyro_daemon::client::send;
use klyro_daemon::ipc::{Request, Response};
use std::process::ExitCode;

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
    /// Drive a native Windows app: attach to an open window by title, or --launch a program.
    /// Then snapshot, click, fill, press, text, wait and screenshot work as on web pages.
    App {
        /// A window title (or part of it), or with --launch the program to start.
        target: String,
        /// Start TARGET (a program) instead of attaching to a window; `close` stops it.
        #[arg(long)]
        launch: bool,
        /// Arguments for the launched program (after `--`).
        #[arg(last = true)]
        args: Vec<String>,
    },
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
    /// Watch what agents do: prints each action here and opens a live page with the screen.
    Watch {
        /// Don't open the browser; just print the page's link.
        #[arg(long)]
        no_open: bool,
        /// Print what happened so far and exit.
        #[arg(long)]
        once: bool,
    },
    /// Check which engines this machine has, and the daemon.
    Doctor,
    /// Serve the same commands as MCP tools over stdio (for MCP clients).
    Mcp,
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
        Cmd::Watch { no_open, once } => return watch(no_open, once, cli.json),
        Cmd::Mcp => {
            return match klyro_mcp::serve(std::io::stdin().lock(), std::io::stdout().lock()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("mcp: {e}");
                    ExitCode::FAILURE
                }
            };
        }
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
        Cmd::App { target, launch, args } => {
            let mode = if launch { "launch" } else { "attach" };
            ("app", [vec![mode.to_string(), target], args].concat())
        }
        Cmd::Status => ("status", vec![]),
        Cmd::Close => ("close", vec![]),
    };
    let req = Request { token: String::new(), session: cli.session, cmd: cmd.into(), args, json: cli.json, headed, engine };
    // Only `open`/`connect` may start a daemon; other commands need an existing session anyway.
    match send(req, matches!(cmd, "open" | "connect" | "app")) {
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

/// Polls the daemon's live log and prints new actions until stopped (Ctrl+C).
fn watch(no_open: bool, once: bool, json: bool) -> ExitCode {
    let poll = |after: u64| -> Result<serde_json::Value, String> {
        let req = Request {
            token: String::new(),
            session: String::new(),
            cmd: "events".into(),
            args: vec![after.to_string()],
            json: true,
            headed: false,
            engine: None,
        };
        let resp = send(req, true)?;
        serde_json::from_str(&resp.out).map_err(|e| format!("bad reply: {e}"))
    };
    let first = match poll(0) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let url = first["view"].as_str().unwrap_or("").to_string();
    if json && once {
        println!("{first}");
        return ExitCode::SUCCESS;
    }
    eprintln!("live view: {url}");
    if !no_open && !once {
        open_in_browser(&url);
    }
    // Times are shown relative to the first action seen.
    let mut start: Option<u64> = None;
    let mut after = 0;
    let mut data = first;
    loop {
        for e in data["events"].as_array().into_iter().flatten() {
            after = e["seq"].as_u64().unwrap_or(after);
            let t0 = *start.get_or_insert(e["at"].as_u64().unwrap_or(0));
            print_event(e, t0);
        }
        if once {
            return ExitCode::SUCCESS;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        data = match poll(after) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("daemon stopped");
                return ExitCode::SUCCESS;
            }
        };
    }
}

fn print_event(e: &serde_json::Value, start_ms: u64) {
    let secs = e["at"].as_u64().unwrap_or(0).saturating_sub(start_ms) as f64 / 1000.0;
    let status = if e["ok"] == true { "ok" } else { "FAILED" };
    println!("+{secs:>6.1}s  [{}] {} {}  {status}", e["session"].as_str().unwrap_or(""), e["cmd"].as_str().unwrap_or(""), e["args"].as_str().unwrap_or(""));
    let out = e["out"].as_str().unwrap_or("");
    let lines: Vec<&str> = out.lines().collect();
    for l in lines.iter().take(6) {
        println!("          {l}");
    }
    if lines.len() > 6 {
        println!("          … {} more lines", lines.len() - 6);
    }
    for n in e["notes"].as_array().into_iter().flatten() {
        println!("          note: {}", n.as_str().unwrap_or(""));
    }
}

fn open_in_browser(url: &str) {
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}
