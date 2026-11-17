//! End-to-end: real `klyro` binary, real daemon, real Chromium, local test pages.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const PAGE_A: &str = "<!doctype html><title>Page A</title><h1>Alpha</h1><button>Go A</button>";
const PAGE_B: &str = "<!doctype html><title>Page B</title><h1>Beta</h1><a href=\"/a\">Back to A</a>";

/// Tiny HTTP server: /b serves PAGE_B, everything else PAGE_A.
fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut first = String::new();
            let _ = reader.read_line(&mut first);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let body = if first.starts_with("GET /b ") { PAGE_B } else { PAGE_A };
            let _ = write!(
                &stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

struct Klyro {
    home: PathBuf,
}

impl Klyro {
    fn new(name: &str) -> Self {
        let home = std::env::temp_dir().join(format!("klyro-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        Klyro { home }
    }

    /// (success, stdout, stderr)
    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_klyro")).args(args).env("KLYRO_HOME", &self.home).output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, out, err) = self.run(args);
        assert!(ok, "klyro {args:?} failed: {err}");
        out
    }
}

impl Drop for Klyro {
    /// Never leave a daemon or browser behind, even when an assert fails.
    fn drop(&mut self) {
        for s in ["a", "b", "default"] {
            self.run(&["--session", s, "close"]);
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[test]
fn open_snapshot_close_with_named_sessions() {
    let port = serve();
    let k = Klyro::new("sessions");
    let (a, b) = (format!("http://127.0.0.1:{port}/a"), format!("http://127.0.0.1:{port}/b"));

    // No daemon yet: snapshot fails cleanly, without starting one.
    let (ok, _, err) = k.run(&["snapshot"]);
    assert!(!ok && err.contains("no session 'default'"), "{err}");

    k.ok(&["--session", "a", "open", &a]);
    k.ok(&["--session", "b", "open", &b]);

    let snap_a = k.ok(&["--session", "a", "snapshot"]);
    assert!(snap_a.contains("- heading \"Alpha\" [ref=e1] [level=1]"), "{snap_a}");
    assert!(snap_a.contains("- button \"Go A\" [ref=e2]"), "{snap_a}");

    let snap_b = k.ok(&["--session", "b", "snapshot"]);
    assert!(snap_b.contains("heading \"Beta\"") && !snap_b.contains("Alpha"), "sessions mixed: {snap_b}");

    let json: serde_json::Value = serde_json::from_str(&k.ok(&["--session", "a", "snapshot", "--json"])).unwrap();
    assert_eq!(json["url"], a.as_str());
    assert_eq!(json["title"], "Page A");

    assert!(k.ok(&["--session", "a", "close"]).contains("closed session a"));
    let (ok, _, err) = k.run(&["--session", "a", "snapshot"]);
    assert!(!ok && err.contains("no session 'a'"), "{err}");
    assert!(k.ok(&["--session", "b", "snapshot"]).contains("Beta"), "closing a must not touch b");

    // Last session closed: the daemon exits and removes its state file.
    k.ok(&["--session", "b", "close"]);
    let info = k.home.join("daemon.json");
    let end = Instant::now() + Duration::from_secs(5);
    while info.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!info.exists(), "daemon did not exit after the last session closed");
}

#[test]
fn bad_url_is_a_clear_error() {
    let k = Klyro::new("badurl");
    let (ok, _, err) = k.run(&["open", "http://127.0.0.1:1/"]);
    assert!(!ok && err.contains("could not open"), "{err}");
}
