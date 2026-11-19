//! Demo app for tests, shared by test crates via `#[path]`. Plain HTTP on 127.0.0.1.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

const LOGIN: &str = include_str!("../fixtures/demo-app/login.html");
const DASHBOARD: &str = include_str!("../fixtures/demo-app/dashboard.html");
/// One of each accessibility problem the a11y pack finds.
const A11Y_BROKEN: &str = r#"<!doctype html><title>Broken</title>
<h2>Shop</h2>
<a href="/"><img src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></a>
<button><svg width="10" height="10"></svg></button>
<input type="text">
<h4>Details</h4>
<img src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" alt="Logo">
<button>Buy</button>"#;

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
                (_, "/a11y-broken") => html(A11Y_BROKEN),
                (_, "/long") => html("<!doctype html><title>Long</title><h1>Top</h1><div style=\"height:3000px\"></div><p>Bottom</p>"),
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

