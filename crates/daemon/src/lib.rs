//! Klyro daemon: keeps browser sessions alive between CLI calls.

pub mod ipc;

use ipc::{Info, Request, Response};
use klyro_adapter_web::Chromium;
use klyro_core::{Action, Adapter, Wait};
use std::time::Duration;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

type Session = Arc<Mutex<Box<dyn Adapter + Send>>>;
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
    Response { ok: false, out: m.into() }
}

/// Returns the response and whether the daemon should exit afterwards.
fn dispatch(req: &Request, sessions: &Sessions) -> (Response, bool) {
    let result = match req.cmd.as_str() {
        "open" => open(req, sessions),
        "snapshot" => with_session(req, sessions, |a| {
            let s = a.snapshot()?;
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
        "text" => with_session(req, sessions, |a| a.text()),
        "wait" => wait(req, sessions),
        "close" => {
            let mut map = sessions.lock().unwrap();
            let r = match map.remove(&req.session) {
                Some(s) => {
                    let _ = s.lock().unwrap().close();
                    Ok(format!("closed session {}", req.session))
                }
                None => Err(no_session(&req.session)),
            };
            let exit = map.is_empty();
            return (to_response(r), exit);
        }
        other => Err(format!("unknown command {other}")),
    };
    (to_response(result), false)
}

fn to_response(r: Result<String, String>) -> Response {
    match r {
        Ok(out) => Response { ok: true, out },
        Err(e) => err(e),
    }
}

fn no_session(name: &str) -> String {
    format!("no session '{name}': run `klyro open <url>` first")
}

fn open(req: &Request, sessions: &Sessions) -> Result<String, String> {
    let url = normalize_url(req.args.first().ok_or("open needs a URL")?);
    let mut created = false;
    let session = {
        let mut map = sessions.lock().unwrap();
        match map.get(&req.session) {
            Some(s) => s.clone(),
            None => {
                created = true;
                // ponytail: launches while holding the map lock (about a second); per-session init if that hurts.
                let s: Session = Arc::new(Mutex::new(Box::new(Chromium::launch(req.headed).map_err(|e| e.to_string())?)));
                map.insert(req.session.clone(), s.clone());
                s
            }
        }
    };
    session.lock().unwrap().open(&url).map_err(|e| e.to_string())?;
    Ok(if req.headed && !created {
        format!("opened {url} (session '{}' was already running headless; close it first to get a window)", req.session)
    } else {
        format!("opened {url}")
    })
}

/// Runs an action; `ref_at` is the args index of the optional target ref.
fn act(req: &Request, sessions: &Sessions, ref_at: usize, action: Action, done: &str) -> Result<String, String> {
    let target = req.args.get(ref_at).map(|r| r.trim_start_matches('@').to_string());
    with_session(req, sessions, |a| a.act(target.as_deref(), &action))?;
    let what = match (&action, &target) {
        (Action::Press(k), Some(r)) => format!("{k} on @{r}"),
        (Action::Press(k), None) => k.clone(),
        (_, Some(r)) => format!("@{r}"),
        (_, None) => String::new(),
    };
    Ok(format!("{done} {what}"))
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
    Ok(match cond {
        Wait::Ms(ms) => format!("waited {ms} ms"),
        Wait::Text(t) => format!("found text {t:?}"),
        Wait::Url(u) => format!("URL contains {u:?}"),
    })
}

fn with_session<T>(
    req: &Request,
    sessions: &Sessions,
    f: impl FnOnce(&mut Box<dyn Adapter + Send>) -> klyro_core::Result<T>,
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
    fn tokens_are_long_and_differ() {
        let (a, b) = (super::ipc::new_token(), super::ipc::new_token());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}
