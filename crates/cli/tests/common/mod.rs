//! Shared test helpers: the demo app server and a klyro runner with an isolated daemon.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

const LOGIN: &str = include_str!("../../../../tests/fixtures/demo-app/login.html");
const DASHBOARD: &str = include_str!("../../../../tests/fixtures/demo-app/dashboard.html");

/// Demo app on 127.0.0.1: `/login` (POST demo/secret sets a cookie and redirects to
/// `/dashboard`), `/dashboard` (needs the cookie). Returns the base URL.
pub fn demo_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut first = String::new();
            let _ = reader.read_line(&mut first);
            let (mut len, mut cookie) = (0usize, String::new());
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
                if lower.starts_with("cookie:") {
                    cookie = line.clone();
                }
                line.clear();
            }
            let mut body = vec![0; len];
            let _ = reader.read_exact(&mut body);
            let body = String::from_utf8_lossy(&body);
            let path = first.split_whitespace().nth(1).unwrap_or("/");

            let resp = match (first.split_whitespace().next(), path) {
                (Some("POST"), "/login") if body.contains("user=demo") && body.contains("pass=secret") => {
                    "HTTP/1.1 303 See Other\r\nLocation: /dashboard\r\nSet-Cookie: sid=ok; Path=/\r\nContent-Length: 0\r\n\r\n".to_string()
                }
                (Some("POST"), "/login") => html(&LOGIN.replace("<!--ERROR-->", "<p role=\"alert\">Wrong username or password</p>")),
                (_, "/dashboard") if cookie.contains("sid=ok") => html(DASHBOARD),
                (_, "/dashboard") => "HTTP/1.1 303 See Other\r\nLocation: /login\r\nContent-Length: 0\r\n\r\n".to_string(),
                _ => html(LOGIN),
            };
            let _ = (&stream).write_all(resp.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn html(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

/// `- textbox "Username" [ref=e2]` -> "e2", for the first line with a ref containing `needle`.
pub fn ref_of(snapshot: &str, needle: &str) -> String {
    let line = snapshot
        .lines()
        .find(|l| l.contains(needle) && l.contains("[ref="))
        .unwrap_or_else(|| panic!("no ref for {needle:?} in:\n{snapshot}"));
    let start = line.find("[ref=").unwrap() + 5;
    line[start..].split(']').next().unwrap().to_string()
}

pub struct Klyro {
    pub home: PathBuf,
}

impl Klyro {
    pub fn new(name: &str) -> Self {
        let home = std::env::temp_dir().join(format!("klyro-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        Klyro { home }
    }

    /// (success, stdout, stderr)
    pub fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_klyro")).args(args).env("KLYRO_HOME", &self.home).output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
    }

    pub fn ok(&self, args: &[&str]) -> String {
        let (ok, out, err) = self.run(args);
        assert!(ok, "klyro {args:?} failed: {err}");
        out
    }

    pub fn fails(&self, args: &[&str]) -> String {
        let (ok, out, err) = self.run(args);
        assert!(!ok, "klyro {args:?} should fail, printed: {out}");
        err
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
