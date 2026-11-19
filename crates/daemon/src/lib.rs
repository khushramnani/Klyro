//! Klyro daemon: keeps browser sessions alive between CLI calls.

pub mod client;
pub mod ipc;
pub mod live;
pub mod replay;
pub mod router;
pub mod surface;

use ipc::{Info, Request, Response};
use klyro_adapter_web::Engine;
use router::Router;
use surface::Surface;
use klyro_core::{Action, Adapter, ScreenshotOptions, Wait, diff};
use serde_json::{Value, json};
use std::time::Duration;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// Requests being handled. The idle check must not exit under one (e.g. a `close` that just
/// emptied the session map but has not answered yet).
static BUSY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

type Session = Arc<Mutex<Surface>>;
type Sessions = Arc<Mutex<HashMap<String, Session>>>;

/// Runs until the last session closes.
pub fn run() -> io::Result<()> {
    kill_children_with_me();
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let token = ipc::new_token();
    let port = listener.local_addr()?.port();
    ipc::write_info(&Info { port, token: token.clone(), pid: std::process::id() })?;
    live::init(port);
    eprintln!("klyro daemon {} listening on {}", std::process::id(), listener.local_addr()?);

    let sessions: Sessions = Arc::default();
    // Nothing open and nobody watching: stop. (Closing the last session stops it at once unless
    // someone is watching; this catches the watcher leaving later.)
    let idle = sessions.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(5));
            if idle.lock().unwrap().is_empty() && !live::watching() && BUSY.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                ipc::remove_info();
                std::process::exit(0);
            }
        }
    });
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (sessions, token) = (sessions.clone(), token.clone());
        BUSY.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // ponytail: thread per connection; connections are short and few.
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, &token, &sessions) {
                eprintln!("connection error: {e}");
            }
            BUSY.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    }
    Ok(())
}

/// Browsers must not outlive the daemon, even when it is force-killed.
/// Windows: the daemon joins a job object that kills every process in it when the daemon's
/// handle closes (i.e. when the daemon dies, however it dies); children join automatically.
#[cfg(windows)]
fn kill_children_with_me() {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time: i64,
        per_job_user_time: i64,
        limit_flags: u32,
        min_working_set: usize,
        max_working_set: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attrs: *mut c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(job: *mut c_void, class: i32, info: *const c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
    }
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
    let mut info = ExtendedLimits::default();
    info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: plain Win32 calls with a correctly laid out struct; the job handle is
    // deliberately never closed, so it closes (and kills the job) when the daemon exits.
    let ok = unsafe {
        let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
        !job.is_null()
            && SetInformationJobObject(job, JOB_OBJECT_EXTENDED_LIMIT_INFORMATION, (&raw const info).cast(), size_of::<ExtendedLimits>() as u32) != 0
            && AssignProcessToJobObject(job, GetCurrentProcess()) != 0
    };
    if !ok {
        eprintln!("warning: could not tie browsers to the daemon; a force-killed daemon may leave them running");
    }
}

/// ponytail: Linux's PR_SET_PDEATHSIG fires when the spawning *thread* exits, and browsers start
/// on short-lived request threads, so it would kill them at once. A force-killed daemon can leave
/// browsers running here; a reaper keyed on the profile dir is the upgrade if that bites.
#[cfg(not(windows))]
fn kill_children_with_me() {}

fn handle(stream: TcpStream, token: &str, sessions: &Sessions) -> io::Result<()> {
    let mut line = String::new();
    let mut reader = BufReader::new(stream.try_clone()?);
    reader.read_line(&mut line)?;
    // A browser on the live view page (read-only, its own key).
    if line.starts_with("GET ") {
        return live::http(reader, &stream, &line);
    }
    let (resp, exit) = match serde_json::from_str::<Request>(&line) {
        Ok(req) if req.token == token => dispatch(&req, sessions),
        Ok(_) => (err("bad token"), false),
        Err(e) => (err(format!("bad request: {e}")), false),
    };
    // Unlist ourselves before answering, so the caller's next command starts a new daemon
    // instead of reaching this one while it exits.
    if exit {
        ipc::remove_info();
    }
    let mut stream = stream;
    writeln!(stream, "{}", serde_json::to_string(&resp)?)?;
    stream.flush()?;
    if exit {
        // Exiting with the socket open can reset it and lose the reply (Windows), so wait until
        // the caller has read it and hung up.
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = io::Read::read(&mut stream, &mut [0u8; 1]);
        // ponytail: a command already connected in this instant still fails; a client retry would close that.
        std::process::exit(0);
    }
    Ok(())
}

fn err(m: impl Into<String>) -> Response {
    Response { ok: false, out: m.into(), notes: vec![] }
}

/// Returns the response and whether the daemon should exit afterwards.
fn dispatch(req: &Request, sessions: &Sessions) -> (Response, bool) {
    // Recording: name the element before the action changes the page.
    let target = replay::ref_to_locate(req).and_then(|r| {
        let s = sessions.lock().unwrap().get(&req.session).cloned()?;
        let (role, name, nth) = s.lock().unwrap().last_snapshot()?.locate(r)?;
        Some(replay::Target { role, name, nth })
    });
    let result = match req.cmd.as_str() {
        "open" => open(req, sessions),
        "connect" => connect(req, sessions),
        "app" => app(req, sessions),
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
        "replay" => replay_steps(req, sessions),
        "expect" => expect(req, sessions),
        "check" => check(req, sessions),
        // The live log, for `klyro watch` (not itself logged).
        "events" => {
            live::mark_watched();
            let after = req.args.first().and_then(|a| a.parse().ok()).unwrap_or(0);
            return (Response { ok: true, out: live::events_json(after).to_string(), notes: vec![] }, false);
        }
        "close" => {
            let mut map = sessions.lock().unwrap();
            let r = match map.remove(&req.session) {
                Some(s) => {
                    let _ = s.lock().unwrap().close();
                    Ok(reply(req, format!("closed session {}", req.session), json!({ "closed": req.session })))
                }
                None => Err(no_session(&req.session)),
            };
            // Someone watching keeps the daemon (and their view) up; the idle check ends it later.
            let exit = map.is_empty() && !live::watching();
            let resp = to_response(r);
            live::record(req, &resp);
            replay::push(req, &resp, None);
            live::forget(&req.session);
            return (resp, exit);
        }
        other => Err(format!("unknown command {other}")),
    };
    let mut resp = to_response(result);
    let session = sessions.lock().unwrap().get(&req.session).cloned();
    // Lane switches caused by this command, shown to the caller.
    if let Some(s) = &session {
        resp.notes = s.lock().unwrap().take_notes();
    }
    resp.notes.extend(replay::push(req, &resp, target));
    let seq = live::record(req, &resp);
    // A fresh frame for the live view, only while someone watches (agents pay nothing otherwise).
    if let Some(s) = session.filter(|_| resp.ok && live::watching() && live::changes_view(&req.cmd)) {
        let frame = s.lock().unwrap().frame();
        live::set_frame(&req.session, frame, seq);
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
                let is_doc = std::path::Path::new(&url).is_file();
                let s: Session = Arc::new(Mutex::new(Surface::Web(Router::start(engine, req.headed, is_doc).map_err(|e| e.to_string())?)));
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
    let mut router = Surface::Web(Router::attach(port).map_err(|e| e.to_string())?);
    let snap = router.snapshot().map_err(|e| e.to_string())?;
    map.insert(req.session.clone(), Arc::new(Mutex::new(router)));
    let title = if snap.title.is_empty() { snap.url.clone() } else { snap.title.clone() };
    Ok(reply(req, format!("connected to port {port}: {title}"), json!({ "connected": port, "url": snap.url, "title": snap.title })))
}

/// A native desktop app: args ["attach", title] or ["launch", program, args...].
fn app(req: &Request, sessions: &Sessions) -> Result<String, String> {
    let mut map = sessions.lock().unwrap();
    if map.contains_key(&req.session) {
        return Err(format!("session '{}' already exists: close it or pick another with --session", req.session));
    }
    let desktop = match req.args.split_first() {
        Some((mode, rest)) if mode == "attach" && !rest.is_empty() => klyro_adapter_desktop::DesktopApp::attach(&rest.join(" ")),
        Some((mode, rest)) if mode == "launch" && !rest.is_empty() => klyro_adapter_desktop::DesktopApp::launch(rest),
        _ => return Err("app needs a window title, or --launch <program> [args]".into()),
    };
    let mut surface = Surface::App(desktop.map_err(|e| e.to_string())?);
    let snap = surface.snapshot().map_err(|e| e.to_string())?;
    let status = surface.status();
    map.insert(req.session.clone(), Arc::new(Mutex::new(surface)));
    Ok(reply(req, format!("{status}\n{}", snap.outline().to_text().trim_end()), json!({ "app": snap.title, "outline": snap.outline() })))
}

/// A test assertion: args [text|url|role, is|not, timeout_ms, value, name?]. Looks again until
/// it holds or the timeout passes, then fails with what it saw.
fn expect(req: &Request, sessions: &Sessions) -> Result<String, String> {
    let [kind, mode, timeout, value, rest @ ..] = req.args.as_slice() else {
        return Err("expect needs --text, --url or --role".into());
    };
    let want = mode != "not";
    let timeout: u64 = timeout.parse().map_err(|_| format!("bad timeout {timeout}"))?;
    let name = rest.first().map(String::as_str);
    let what = match (kind.as_str(), name) {
        ("text", _) => format!("text {value:?}"),
        ("url", _) => format!("a URL containing {value:?}"),
        ("role", Some(n)) => format!("{value} {n:?}"),
        ("role", None) => format!("any {value}"),
        _ => return Err(format!("bad expect kind {kind}")),
    };
    let end = std::time::Instant::now() + Duration::from_millis(timeout);
    loop {
        let (holds, url) = with_session(req, sessions, |a| match kind.as_str() {
            "text" => Ok((a.text()?.contains(value.as_str()), None)),
            "url" => a.snapshot().map(|s| (s.url.contains(value.as_str()), Some(s.url))),
            _ => a.snapshot().map(|s| (s.has(value, name), None)),
        })?;
        if holds == want {
            let text = if want { format!("ok: page has {what}") } else { format!("ok: page has no {what}") };
            return Ok(reply(req, text, json!({ "passed": true })));
        }
        if std::time::Instant::now() >= end {
            let seen = url.map(|u| format!(" (URL is {u})")).unwrap_or_default();
            let (expected, found) = if want { ("", "not found") } else { ("no ", "still there") };
            return Err(format!("expected page to have {expected}{what}: {found} after {timeout} ms{seen}"));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Check packs: args ["a11y"] or ["layout", width?]. Problems found fail the command, so a
/// replay stops there.
fn check(req: &Request, sessions: &Sessions) -> Result<String, String> {
    let (kind, at, problems) = match req.args.first().map(String::as_str) {
        Some("a11y") => {
            let s = with_session(req, sessions, |a| {
                a.need_chromium("the accessibility check")?;
                a.snapshot()
            })?;
            ("accessibility", String::new(), klyro_core::a11y(&s))
        }
        Some("layout") => {
            let width = req.args.get(1).map(|w| w.parse().map_err(|_| format!("bad width {w}"))).transpose()?;
            let (w, problems) = with_session(req, sessions, |a| a.layout(width))?;
            ("layout", format!(" at {w}px wide"), problems)
        }
        _ => return Err("check needs a11y or layout".into()),
    };
    if req.json {
        let j = json!({ "problems": problems }).to_string();
        return if problems.is_empty() { Ok(j) } else { Err(j) };
    }
    match problems.len() {
        0 => Ok(format!("no {kind} problems found{at}")),
        n => Err(format!("{n} {kind} problem{}{at}:\n{}", if n == 1 { "" } else { "s" }, problems.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n"))),
    }
}

/// args: [the recording's JSON, already read by the caller]. Runs in this session.
fn replay_steps(req: &Request, sessions: &Sessions) -> Result<String, String> {
    let steps: Vec<replay::Step> = serde_json::from_str(req.args.first().ok_or("replay needs a recording")?).map_err(|e| format!("not a klyro recording: {e}"))?;
    let find = |t: &replay::Target| with_session(req, sessions, |a| a.snapshot()).map(|s| s.find(&t.role, &t.name, t.nth));
    let out = replay::replay(&req.session, &steps, Duration::from_secs(10), find, |r| dispatch(r, sessions).0)?;
    Ok(reply(req, out, json!({ "replayed": steps.len() })))
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
            // The new page's outline (refs, headings, messages): what the agent needs next.
            // The full tree is one `snapshot` away, the text one `text` away.
            let outline = after.outline();
            let title = if after.title.is_empty() { String::new() } else { format!(" ({})", after.title) };
            (
                format!("{head}
page changed: {}{title}
{}", after.url, outline.to_text()),
                json!({ "done": done, "target": target, "page_changed": true, "outline": outline }),
            )
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

/// args: [absolute path, "full" | "ref=<ref>" | "page=<n>" ...]
fn screenshot(req: &Request, sessions: &Sessions) -> Result<String, String> {
    let path = req.args.first().ok_or("screenshot needs a file path")?;
    let mut opts = ScreenshotOptions::default();
    for a in &req.args[1..] {
        match a.split_once('=') {
            Some(("ref", r)) => opts.target = Some(r.trim_start_matches('@').to_string()),
            Some(("page", n)) => opts.page = Some(n.parse().map_err(|_| format!("bad page number {n}"))?),
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
    f: impl FnOnce(&mut Surface) -> klyro_core::Result<T>,
) -> Result<T, String> {
    let s = sessions.lock().unwrap().get(&req.session).cloned().ok_or_else(|| no_session(&req.session))?;
    let mut a = s.lock().unwrap();
    f(&mut a).map_err(|e| e.to_string())
}

/// `example.com` -> `https://example.com`; anything with a scheme, or a local file, is left alone.
fn normalize_url(u: &str) -> String {
    if u.contains("://") || u.starts_with("about:") || u.starts_with("data:") || std::path::Path::new(u).is_file() {
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
