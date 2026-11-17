//! Klyro daemon: keeps browser sessions alive between CLI calls.

pub mod ipc;
pub mod router;

use ipc::{Info, Request, Response};
use klyro_adapter_web::Engine;
use router::Router;
use klyro_core::{Action, Adapter, ScreenshotOptions, Wait, diff};
use serde_json::{Value, json};
use std::time::Duration;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

type Session = Arc<Mutex<Router>>;
type Sessions = Arc<Mutex<HashMap<String, Session>>>;

/// Runs until the last session closes.
pub fn run() -> io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let token = ipc::new_token();
    ipc::write_info(&Info { port: listener.local_addr()?.port(), token: token.clone(), pid: std::process::id() })?;
    eprintln!("klyro daemon {} listening on {}", std::process::id(), listener.local_addr()?);

    let sessions: Sessions = Arc::default();
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (sessions, token) = (sessions.clone(), token.clone());
        // ponytail: thread per connection; connections are short and few.
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, &token, &sessions) {
                eprintln!("connection error: {e}");
            }
        });
    }
    Ok(())
}

fn handle(stream: TcpStream, token: &str, sessions: &Sessions) -> io::Result<()> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let (resp, exit) = match serde_json::from_str::<Request>(&line) {
        Ok(req) if req.token == token => dispatch(&req, sessions),
        Ok(_) => (err("bad token"), false),
        Err(e) => (err(format!("bad request: {e}")), false),
    };
    let mut stream = stream;
    writeln!(stream, "{}", serde_json::to_string(&resp)?)?;
    stream.flush()?;
    if exit {
        // ponytail: exits when the last session closes; a concurrent `open` racing this just restarts the daemon.
        ipc::remove_info();
        std::process::exit(0);
    }
    Ok(())
}

fn err(m: impl Into<String>) -> Response {
    Response { ok: false, out: m.into(), notes: vec![] }
}

/// Returns the response and whether the daemon should exit afterwards.
fn dispatch(req: &Request, sessions: &Sessions) -> (Response, bool) {
    let result = match req.cmd.as_str() {
        "open" => open(req, sessions),
        "connect" => connect(req, sessions),
        "snapshot" => with_session(req, sessions, |a| {
            let mut s = a.snapshot()?;
            if req.args.iter().any(|x| x == "interactive") {
                s = s.interactive();
            }
            Ok(if req.json { s.to_json() } else { s.to_text().trim_end().to_string() })
        }),
        "click" => act(req, sessions, 0, Action::Click, "clicked"),
        "fill" => match req.args.get(1) {
            Some(text) => act(req, sessions, 0, Action::Fill(text.clone()), "filled"),
            None => Err("fill needs a ref and text".into()),
        },
        "press" => match req.args.first() {
            Some(key) => act(req, sessions, 1, Action::Press(key.clone()), "pressed"),
            None => Err("press needs a key".into()),
        },
        "text" => with_session(req, sessions, |a| a.text()).map(|t| reply(req, t.clone(), json!({ "text": t }))),
        "status" => with_session(req, sessions, |a| Ok(reply(req, a.status(), a.status_json()))),
        "screenshot" => screenshot(req, sessions),
        "wait" => wait(req, sessions),
        "close" => {
            let mut map = sessions.lock().unwrap();
            let r = match map.remove(&req.session) {
                Some(s) => {
                    let _ = s.lock().unwrap().close();
                    Ok(reply(req, format!("closed session {}", req.session), json!({ "closed": req.session })))
                }
                None => Err(no_session(&req.session)),
            };
            let exit = map.is_empty();
            return (to_response(r), exit);
        }
        other => Err(format!("unknown command {other}")),
    };
    let mut resp = to_response(result);
    // Lane switches caused by this command, shown to the caller.
    if let Some(s) = sessions.lock().unwrap().get(&req.session).cloned() {
        resp.notes = s.lock().unwrap().take_notes();
    }
    (resp, false)
}

fn to_response(r: Result<String, String>) -> Response {
    match r {
        Ok(out) => Response { ok: true, out, notes: vec![] },
        Err(e) => err(e),
    }
}

fn no_session(name: &str) -> String {
    format!("no session '{name}': run `klyro open <url>` first")
}

fn open(req: &Request, sessions: &Sessions) -> Result<String, String> {
    let url = normalize_url(req.args.first().ok_or("open needs a URL")?);
    // No --engine: the router picks lanes automatically.
    let engine: Option<Engine> = req.engine.as_deref().map(str::parse).transpose()?;
    let mut created = false;
    let session = {
        let mut map = sessions.lock().unwrap();
        match map.get(&req.session) {
            Some(s) => s.clone(),
            None => {
                created = true;
                // ponytail: launches while holding the map lock (about a second); per-session init if that hurts.
                let s: Session = Arc::new(Mutex::new(Router::start(engine, req.headed).map_err(|e| e.to_string())?));
                map.insert(req.session.clone(), s.clone());
                s
            }
        }
    };
    session.lock().unwrap().open(&url).map_err(|e| e.to_string())?;
    let text = if req.headed && !created {
        format!("opened {url} (session '{}' was already running headless; close it first to get a window)", req.session)
    } else {
        format!("opened {url}")
    };
    Ok(reply(req, text, json!({ "opened": url })))
}

/// Attach a session to an app already running with a DevTools port (Electron).
fn connect(req: &Request, sessions: &Sessions) -> Result<String, String> {
    let port: u16 = req.args.first().and_then(|p| p.parse().ok()).ok_or("connect needs a port number")?;
    let mut map = sessions.lock().unwrap();
    if map.contains_key(&req.session) {
        return Err(format!("session '{}' already exists: close it or pick another with --session", req.session));
    }
    let mut router = Router::attach(port).map_err(|e| e.to_string())?;
    let snap = router.snapshot().map_err(|e| e.to_string())?;
    map.insert(req.session.clone(), Arc::new(Mutex::new(router)));
    let title = if snap.title.is_empty() { snap.url.clone() } else { snap.title.clone() };
    Ok(reply(req, format!("connected to port {port}: {title}"), json!({ "connected": port, "url": snap.url, "title": snap.title })))
}

/// Text or JSON, as the caller asked.
fn reply(req: &Request, text: String, json: Value) -> String {
    if req.json { json.to_string() } else { text }
}

/// Runs an action and reports what it changed on the page, not the whole page.
/// `ref_at` is the args index of the optional target ref.
fn act(req: &Request, sessions: &Sessions, ref_at: usize, action: Action, done: &str) -> Result<String, String> {
    let target = req.args.get(ref_at).map(|r| r.trim_start_matches('@').to_string());
    let what = match (&action, &target) {
        (Action::Press(k), Some(r)) => format!("{k} on @{r}"),
        (Action::Press(k), None) => k.clone(),
        (_, Some(r)) => format!("@{r}"),
        (_, None) => String::new(),
    };
    with_session(req, sessions, |a| {
        // Compare with what the agent last saw; take a snapshot first if it saw nothing yet.
        let before = match a.last_snapshot() {
            Some(s) => s.clone(),
            None => a.snapshot()?,
        };
        a.act(target.as_deref(), &action)?;
        let head = format!("{done} {what}");
        let after = match a.snapshot() {
            Ok(s) => s,
            // The action happened; only reading the result failed.
            Err(e) => return Ok(reply(req, format!("{head}
(could not read the page afterwards: {e})"), json!({ "done": done, "target": target }))),
        };
        let d = diff(&before, &after);
        let (text, json) = if d.is_page_change() {
            (format!("{head}
page changed: {}
{}", after.url, after.to_text()), json!({ "done": done, "target": target, "page_changed": true, "snapshot": after }))
        } else if d.is_empty() {
            (format!("{head}
no change yet (run `wait` or `snapshot` if the page updates later)"), json!({ "done": done, "target": target, "diff": d }))
        } else {
            (format!("{head}
{}", d.to_text()), json!({ "done": done, "target": target, "diff": d }))
        };
        Ok(reply(req, text.trim_end().to_string(), json))
    })
}

/// args: [absolute path, "full" | "ref=<ref>" ...]
fn screenshot(req: &Request, sessions: &Sessions) -> Result<String, String> {
    let path = req.args.first().ok_or("screenshot needs a file path")?;
    let mut opts = ScreenshotOptions::default();
    for a in &req.args[1..] {
        match a.split_once('=') {
            Some(("ref", r)) => opts.target = Some(r.trim_start_matches('@').to_string()),
            _ if a == "full" => opts.full_page = true,
            _ => return Err(format!("bad screenshot option {a}")),
        }
    }
    let png = with_session(req, sessions, |a| a.screenshot(&opts))?;
    std::fs::write(path, &png).map_err(|e| format!("could not write {path}: {e}"))?;
    let (w, h) = png_size(&png).unwrap_or((0, 0));
    Ok(if req.json {
        serde_json::json!({ "path": path, "width": w, "height": h, "bytes": png.len() }).to_string()
    } else {
        format!("saved {path} ({w}x{h}, {} KB)", png.len().div_ceil(1024))
    })
}

/// Width and height from a PNG's IHDR chunk.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.get(..8)? != b"\x89PNG\r\n\x1a\n" || png.get(12..16)? != b"IHDR" {
        return None;
    }
    let be = |r: std::ops::Range<usize>| Some(u32::from_be_bytes(png.get(r)?.try_into().ok()?));
    Some((be(16..20)?, be(20..24)?))
}

/// args: [kind, value, timeout_ms] with kind ms | text | url.
fn wait(req: &Request, sessions: &Sessions) -> Result<String, String> {
    let (kind, value) = (req.args.first().map(String::as_str), req.args.get(1).cloned().unwrap_or_default());
    let timeout = Duration::from_millis(req.args.get(2).and_then(|t| t.parse().ok()).unwrap_or(10_000));
    let cond = match kind {
        Some("ms") => Wait::Ms(value.parse().map_err(|_| format!("bad milliseconds '{value}'"))?),
        Some("text") => Wait::Text(value.clone()),
        Some("url") => Wait::Url(value.clone()),
        _ => return Err("wait needs <ms>, --text <text> or --url <part>".into()),
    };
    with_session(req, sessions, |a| a.wait(&cond, timeout))?;
    let text = match &cond {
        Wait::Ms(ms) => format!("waited {ms} ms"),
        Wait::Text(t) => format!("found text {t:?}"),
        Wait::Url(u) => format!("URL contains {u:?}"),
    };
    Ok(reply(req, text.clone(), json!({ "waited": text })))
}

fn with_session<T>(
    req: &Request,
    sessions: &Sessions,
    f: impl FnOnce(&mut Router) -> klyro_core::Result<T>,
) -> Result<T, String> {
    let s = sessions.lock().unwrap().get(&req.session).cloned().ok_or_else(|| no_session(&req.session))?;
    let mut a = s.lock().unwrap();
    f(&mut a).map_err(|e| e.to_string())
}

/// `example.com` -> `https://example.com`; anything with a scheme is left alone.
fn normalize_url(u: &str) -> String {
    if u.contains("://") || u.starts_with("about:") || u.starts_with("data:") {
        u.to_string()
    } else {
        format!("https://{u}")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn normalize_url() {
        assert_eq!(super::normalize_url("example.com"), "https://example.com");
        assert_eq!(super::normalize_url("http://127.0.0.1:8080/a"), "http://127.0.0.1:8080/a");
        assert_eq!(super::normalize_url("about:blank"), "about:blank");
    }

    #[test]
    fn png_size() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(1280u32.to_be_bytes());
        png.extend(800u32.to_be_bytes());
        assert_eq!(super::png_size(&png), Some((1280, 800)));
        assert_eq!(super::png_size(b"not a png"), None);
    }

    #[test]
    fn tokens_are_long_and_differ() {
        let (a, b) = (super::ipc::new_token(), super::ipc::new_token());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}
