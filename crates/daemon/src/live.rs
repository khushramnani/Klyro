//! Live view: what agents do, for a human to watch.
//!
//! The daemon keeps a short log of commands and, while someone is watching, a frame per
//! session after each step. `klyro watch` prints the log in the terminal; the daemon's port
//! also serves a small read-only page (its own random key; it can't send commands).

use crate::ipc::{Request, Response};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, Write};
use std::net::TcpStream;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const KEEP_EVENTS: usize = 500;
/// Frames are captured only if someone looked within this window.
const WATCH_WINDOW: Duration = Duration::from_secs(15);
/// Long command output is cut in the log.
const MAX_OUT: usize = 4000;

struct Frame {
    seq: u64,
    bytes: Vec<u8>,
    mime: &'static str,
}

struct Live {
    key: String,
    port: u16,
    events: VecDeque<Value>,
    next: u64,
    frames: HashMap<String, Frame>,
    watched_at: Option<Instant>,
}

static LIVE: OnceLock<Mutex<Live>> = OnceLock::new();

fn live() -> std::sync::MutexGuard<'static, Live> {
    LIVE.get().expect("live::init runs at daemon start").lock().unwrap_or_else(|e| e.into_inner())
}

/// Call once when the daemon starts.
pub fn init(port: u16) {
    let key = crate::ipc::new_token()[..32].to_string();
    let _ = LIVE.set(Mutex::new(Live { key, port, events: VecDeque::new(), next: 1, frames: HashMap::new(), watched_at: None }));
}

pub fn view_url() -> String {
    let l = live();
    format!("http://127.0.0.1:{}/live?k={}", l.port, l.key)
}

/// Someone looked at the log or the page just now.
pub fn mark_watched() {
    live().watched_at = Some(Instant::now());
}

pub fn watching() -> bool {
    live().watched_at.is_some_and(|t| t.elapsed() < WATCH_WINDOW)
}

/// Commands whose result is worth a fresh frame.
pub fn changes_view(cmd: &str) -> bool {
    matches!(cmd, "open" | "connect" | "app" | "click" | "fill" | "press" | "wait" | "snapshot" | "screenshot")
}

/// Adds a command to the log; returns its sequence number.
pub fn record(req: &Request, resp: &Response) -> u64 {
    let mut args = req.args.clone();
    // Show typed text only if the page itself shows it back in the field; anything else
    // (password fields, whatever their mask looks like) is hidden.
    if req.cmd == "fill" && args.len() > 1 && !resp.out.contains(&format!("[value={:?}]", args[1])) {
        args[1] = "*****".into();
    }
    let args: Vec<String> = args.iter().map(|a| if a.contains(' ') { format!("{a:?}") } else { a.clone() }).collect();
    let mut out = resp.out.clone();
    if out.len() > MAX_OUT {
        let mut cut = MAX_OUT;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n…");
    }
    let at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let mut l = live();
    let seq = l.next;
    l.next += 1;
    l.events.push_back(json!({
        "seq": seq, "at": at, "session": req.session, "cmd": req.cmd, "args": args.join(" "),
        "ok": resp.ok, "out": out, "notes": resp.notes,
    }));
    if l.events.len() > KEEP_EVENTS {
        l.events.pop_front();
    }
    seq
}

pub fn set_frame(session: &str, frame: Option<(Vec<u8>, &'static str)>, seq: u64) {
    let mut l = live();
    match frame {
        Some((bytes, mime)) => {
            l.frames.insert(session.to_string(), Frame { seq, bytes, mime });
        }
        None => {
            l.frames.remove(session);
        }
    }
}

pub fn forget(session: &str) {
    live().frames.remove(session);
}

/// Events after `after`, plus which sessions have a frame.
pub fn events_json(after: u64) -> Value {
    let l = live();
    let events: Vec<&Value> = l.events.iter().filter(|e| e["seq"].as_u64().is_some_and(|s| s > after)).collect();
    let frames: HashMap<&String, u64> = l.frames.iter().map(|(k, f)| (k, f.seq)).collect();
    json!({ "view": format!("http://127.0.0.1:{}/live?k={}", l.port, l.key), "events": events, "frames": frames })
}

/// Answers a browser request on the daemon port. `first` is the request line.
pub fn http(mut reader: impl BufRead, mut stream: &TcpStream, first: &str) -> io::Result<()> {
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        line.clear();
    }
    let target = first.split_whitespace().nth(1).unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let q: HashMap<&str, String> = query.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k, decode(v))).collect();
    let key_ok = q.get("k").is_some_and(|k| *k == live().key);

    let (status, ctype, body): (&str, &str, Vec<u8>) = match (path, key_ok) {
        (_, false) => ("403 Forbidden", "text/plain", b"wrong or missing key; open the link `klyro watch` prints".to_vec()),
        ("/live", true) => ("200 OK", "text/html; charset=utf-8", PAGE.as_bytes().to_vec()),
        ("/events", true) => {
            mark_watched();
            let after = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
            ("200 OK", "application/json", events_json(after).to_string().into_bytes())
        }
        ("/frame", true) => {
            let l = live();
            match q.get("session").and_then(|s| l.frames.get(s)) {
                Some(f) => ("200 OK", f.mime, f.bytes.clone()),
                None => ("404 Not Found", "text/plain", b"no frame".to_vec()),
            }
        }
        _ => ("404 Not Found", "text/plain", b"not found".to_vec()),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The watch page: the selected session's screen (or, on the text-only fast lane, its last
/// output) and the action log. Plain HTML/JS; polls the daemon.
const PAGE: &str = r##"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Klyro live</title>
<style>
  :root { --bg: #f6f7f9; --panel: #fff; --text: #1d2330; --muted: #667085; --line: #e3e6ec;
          --ok: #1a7f4b; --fail: #c4321c; --accent: #3557d5; }
  @media (prefers-color-scheme: dark) {
    :root { --bg: #111418; --panel: #1a1e24; --text: #e6e9ef; --muted: #98a2b3; --line: #2a3039;
            --ok: #4cc38a; --fail: #ff7a66; --accent: #8aa4ff; }
  }
  * { box-sizing: border-box; }
  body { margin: 0; background: var(--bg); color: var(--text); font: 14px/1.45 system-ui, sans-serif; }
  header { display: flex; gap: 12px; align-items: center; padding: 10px 16px; border-bottom: 1px solid var(--line); background: var(--panel); }
  header h1 { font-size: 15px; margin: 0; }
  #state { color: var(--muted); font-size: 13px; }
  select { margin-left: auto; font: inherit; padding: 4px 8px; background: var(--panel); color: var(--text); border: 1px solid var(--line); border-radius: 6px; }
  main { display: grid; grid-template-columns: minmax(0, 3fr) minmax(280px, 2fr); gap: 16px; padding: 16px; height: calc(100vh - 50px); }
  @media (max-width: 800px) { main { grid-template-columns: 1fr; height: auto; } }
  section { background: var(--panel); border: 1px solid var(--line); border-radius: 8px; overflow: auto; min-height: 0; }
  #screen { display: flex; align-items: flex-start; justify-content: center; padding: 12px; }
  #screen img { max-width: 100%; height: auto; border: 1px solid var(--line); border-radius: 4px; }
  #screen pre { margin: 0; width: 100%; }
  .hint { color: var(--muted); font-size: 13px; margin: 0 0 8px; }
  pre { white-space: pre-wrap; word-break: break-word; font: 12px/1.45 ui-monospace, Consolas, monospace; }
  #log { padding: 0; margin: 0; list-style: none; }
  #log li { padding: 10px 12px; border-bottom: 1px solid var(--line); }
  .line { display: flex; gap: 8px; align-items: baseline; }
  .time { color: var(--muted); font-size: 12px; font-variant-numeric: tabular-nums; }
  .sess { color: var(--accent); font-size: 12px; }
  .cmd { font-weight: 600; }
  .args { color: var(--muted); overflow-wrap: anywhere; }
  .badge { margin-left: auto; font-size: 12px; font-weight: 600; }
  .ok { color: var(--ok); } .fail { color: var(--fail); }
  #log pre { margin: 6px 0 0; max-height: 9em; overflow: hidden; cursor: pointer; color: var(--muted); }
  #log pre.open { max-height: none; }
  .note { color: var(--accent); font-size: 12px; margin-top: 4px; }
</style>
<header>
  <h1>Klyro live</h1>
  <span id="state">connecting…</span>
  <select id="sessions" aria-label="Session"><option value="">all sessions</option></select>
</header>
<main>
  <section id="screen" aria-label="Screen"><p class="hint">Waiting for an agent to do something…</p></section>
  <section aria-label="Actions"><ul id="log"></ul></section>
</main>
<script>
  const key = new URLSearchParams(location.search).get('k');
  let after = 0, frames = {}, follow = '', lastBySession = {};
  const $ = id => document.getElementById(id);
  const esc = s => String(s).replace(/[&<>]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[c]);
  const current = () => $('sessions').value || follow;

  function addSession(name) {
    if ([...$('sessions').options].some(o => o.value === name)) return;
    const o = document.createElement('option'); o.value = o.textContent = name; $('sessions').append(o);
  }

  function renderScreen() {
    const s = current();
    if (!s) return;
    if (frames[s]) {
      $('screen').innerHTML = `<img alt="Screen of session ${esc(s)}" src="/frame?k=${key}&session=${encodeURIComponent(s)}&v=${frames[s]}">`;
    } else if (lastBySession[s]) {
      $('screen').innerHTML = `<div style="width:100%"><p class="hint">Session ${esc(s)}: no pixels here (fast lane or no screen yet); its last output:</p><pre>${esc(lastBySession[s])}</pre></div>`;
    }
  }

  async function poll() {
    try {
      const r = await fetch(`/events?k=${key}&after=${after}`, { cache: 'no-store' });
      if (!r.ok) throw new Error(r.status);
      const d = await r.json();
      frames = d.frames;
      for (const e of d.events) {
        after = e.seq; follow = e.session; addSession(e.session);
        if (e.out) lastBySession[e.session] = e.out;
        const li = document.createElement('li');
        const t = new Date(e.at).toLocaleTimeString();
        li.innerHTML = `<div class="line"><span class="time">${t}</span><span class="sess">${esc(e.session)}</span>` +
          `<span class="cmd">${esc(e.cmd)}</span><span class="args">${esc(e.args)}</span>` +
          `<span class="badge ${e.ok ? 'ok' : 'fail'}">${e.ok ? 'ok' : 'failed'}</span></div>` +
          (e.out ? `<pre title="Click to expand">${esc(e.out)}</pre>` : '') +
          e.notes.map(n => `<div class="note">${esc(n)}</div>`).join('');
        const pre = li.querySelector('pre'); if (pre) pre.onclick = () => pre.classList.toggle('open');
        $('log').prepend(li);
      }
      $('state').textContent = 'live';
      renderScreen();
    } catch (e) {
      $('state').textContent = 'daemon not running (it stops when the last session closes)';
    }
    setTimeout(poll, 700);
  }
  $('sessions').onchange = renderScreen;
  poll();
</script>
</html>
"##;

#[cfg(test)]
mod tests {
    #[test]
    fn decode_query_values() {
        assert_eq!(super::decode("my%20session"), "my session");
        assert_eq!(super::decode("a+b"), "a b");
        assert_eq!(super::decode("100%"), "100%");
    }
}
