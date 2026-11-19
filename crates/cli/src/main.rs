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
    /// Record what this session does (open, click, fill, press, wait, close) to a file.
    Record {
        #[command(subcommand)]
        action: RecordCmd,
    },
    /// Run recordings again, without an agent. Elements are found by role and name, so it
    /// works across runs. `${NAME}` in a file is replaced by the environment variable NAME.
    /// With several files: a session left by a failed one is closed, and a summary is printed.
    Replay {
        #[arg(required = true)]
        files: Vec<std::path::PathBuf>,
    },
    /// Test assertion: fail (exit 1) unless the page has this text, URL or element. Looks again
    /// until it holds or the timeout passes. Recorded, so a replay becomes a test.
    #[command(group(ArgGroup::new("what").required(true).args(["text", "url", "role"])))]
    Expect {
        /// The page text contains this.
        #[arg(long)]
        text: Option<String>,
        /// The URL contains this.
        #[arg(long)]
        url: Option<String>,
        /// An element with this role (button, link, textbox, heading, alert, ...) is on the page.
        #[arg(long)]
        role: Option<String>,
        /// With --role: its exact name.
        #[arg(long, requires = "role")]
        name: Option<String>,
        /// Expect the opposite: the text, URL or element is not there.
        #[arg(long)]
        not: bool,
        /// Give up after this many milliseconds.
        #[arg(long, default_value_t = 5_000)]
        timeout: u64,
    },
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
    /// Check the page for problems; exits 1 and lists them if any are found. Recorded.
    Check {
        #[command(subcommand)]
        pack: CheckCmd,
    },
    /// Check which engines this machine has, and the daemon.
    Doctor,
    /// Serve the same commands as MCP tools over stdio (for MCP clients).
    Mcp,
    /// Run the background daemon (started automatically).
    #[command(hide = true)]
    Daemon,
}

#[derive(Subcommand)]
enum CheckCmd {
    /// Accessibility: controls without a name, images without alt text, heading levels that skip.
    A11y,
}

#[derive(Subcommand)]
enum RecordCmd {
    /// Start recording the session (it need not be open yet).
    Start,
    /// Stop recording and save the steps as JSON.
    Stop { file: std::path::PathBuf },
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
        Cmd::Check { pack: CheckCmd::A11y } => ("check", vec!["a11y".into()]),
        // Recordings are files the daemon appends to; starting and saving one needs no daemon.
        Cmd::Record { action } => {
            let r = match action {
                RecordCmd::Start => klyro_daemon::replay::start(&cli.session).map(|_| format!("recording session '{}'; `klyro record stop <file>` saves it", cli.session)),
                RecordCmd::Stop { file } => klyro_daemon::replay::stop(&cli.session, &file).map(|n| format!("saved {n} steps to {}", file.display())),
            };
            return match r {
                Ok(m) => {
                    println!("{m}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        Cmd::Replay { files } => return replay(&cli.session, &files, cli.json),
        Cmd::Expect { text, url, role, name, not, timeout } => {
            let (kind, value) = match (text, url, role) {
                (Some(t), _, _) => ("text", t),
                (_, Some(u), _) => ("url", u),
                (_, _, Some(r)) => ("role", r),
                _ => unreachable!("clap requires one"),
            };
            let mode = if not { "not" } else { "is" };
            let args = [kind.to_string(), mode.into(), timeout.to_string(), value].into_iter().chain(name).collect();
            ("expect", args)
        }
        Cmd::Close => ("close", vec![]),
    };
    let req = Request { token: String::new(), session: cli.session, cmd: cmd.into(), args, json: cli.json, headed, engine };
    // Only `open`/`connect` may start a daemon; other commands need an existing session anyway.
    match send(req, matches!(cmd, "open" | "connect" | "app")) {
        Ok(Response { ok, out, notes }) => {
            for n in notes {
                eprintln!("note: {n}");
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

/// Replays each file in turn. One file: its output as is. Several: a line per file and a summary.
fn replay(session: &str, files: &[std::path::PathBuf], json: bool) -> ExitCode {
    let mut failed = vec![];
    for file in files {
        // Read and fill in variables here: the daemon's environment is not the caller's.
        let steps = std::fs::read_to_string(file).map_err(|e| format!("could not read {}: {e}", file.display())).and_then(|t| expand_vars(&t));
        let req = |cmd: &str, args| Request { token: String::new(), session: session.into(), cmd: cmd.into(), args, json, headed: false, engine: None };
        let r = steps.and_then(|s| send(req("replay", vec![s]), true)).and_then(|r| if r.ok { Ok(r.out) } else { Err(r.out) });
        match &r {
            Ok(out) if files.len() == 1 => println!("{out}"),
            Ok(_) => println!("ok      {}", file.display()),
            Err(e) if files.len() == 1 => eprintln!("error: {e}"),
            Err(e) => {
                println!("FAILED  {}
{}", file.display(), e.lines().map(|l| format!("        {l}")).collect::<Vec<_>>().join("
"));
                // The next file starts from a clean session.
                let _ = send(req("close", vec![]), false);
            }
        }
        if r.is_err() {
            failed.push(file);
        }
    }
    if files.len() > 1 {
        println!("{} passed, {} failed", files.len() - failed.len(), failed.len());
    }
    if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// `${NAME}` -> the value of environment variable NAME, escaped for a JSON string, so secrets
/// stay out of recordings.
fn expand_vars(text: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("${") {
        let end = rest[i..].find('}').ok_or("unclosed ${ in recording")? + i;
        let name = &rest[i + 2..end];
        let value = std::env::var(name).map_err(|_| format!("the recording needs environment variable {name}"))?;
        let quoted = serde_json::to_string(&value).map_err(|e| e.to_string())?;
        out.push_str(&rest[..i]);
        out.push_str(&quoted[1..quoted.len() - 1]);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn expand_vars() {
        // SAFETY: single-threaded test setting a variable only it reads.
        unsafe { std::env::set_var("KLYRO_TEST_PW", "a\"b") };
        assert_eq!(super::expand_vars(r#"["${KLYRO_TEST_PW}", "x"]"#).unwrap(), r#"["a\"b", "x"]"#);
        assert!(super::expand_vars("${KLYRO_TEST_MISSING}").unwrap_err().contains("KLYRO_TEST_MISSING"));
        assert!(super::expand_vars("${oops").is_err());
    }
}
