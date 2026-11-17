//! End-to-end: real `klyro` binary, real daemon, real Chromium, local demo app.

mod common;
use common::{Klyro, demo_app, have_lightpanda, ref_of};
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn open_snapshot_close_with_named_sessions() {
    let app = demo_app();
    let k = Klyro::new("sessions");

    // No daemon yet: snapshot fails cleanly, without starting one.
    assert!(k.fails(&["snapshot"]).contains("no session 'default'"));

    k.ok(&["--session", "a", "open", &format!("{app}/login")]);
    k.ok(&["--session", "b", "open", &format!("{app}/login")]);

    let snap = k.ok(&["--session", "a", "snapshot"]);
    assert!(snap.contains("- heading \"Sign in\" [ref=e1] [level=1]"), "{snap}");
    assert!(snap.contains("textbox \"Username\" [ref="), "{snap}");

    let json: serde_json::Value = serde_json::from_str(&k.ok(&["--session", "a", "snapshot", "--json"])).unwrap();
    assert_eq!(json["url"], format!("{app}/login"));
    assert_eq!(json["title"], "Demo login");

    // Log in on a only; b must not share its cookies.
    k.ok(&["--session", "a", "fill", &ref_of(&snap, "Username"), "demo"]);
    k.ok(&["--session", "a", "fill", &ref_of(&snap, "Password"), "secret"]);
    k.ok(&["--session", "a", "press", "Enter"]);
    k.ok(&["--session", "b", "open", &format!("{app}/dashboard")]);
    assert!(k.ok(&["--session", "b", "text"]).contains("Sign in"), "session b saw session a's login");

    assert!(k.ok(&["--session", "a", "close"]).contains("closed session a"));
    assert!(k.fails(&["--session", "a", "snapshot"]).contains("no session 'a'"));
    assert!(k.ok(&["--session", "b", "snapshot"]).contains("Sign in"), "closing a must not touch b");

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
fn login_flow() {
    login_flow_on("chromium");
}

#[test]
fn login_flow_lightpanda() {
    if have_lightpanda() {
        login_flow_on("lightpanda");
    }
}

/// The plan's demo flow, CLI commands only. Identical on every engine.
fn login_flow_on(engine: &'static str) {
    let app = demo_app();
    let k = Klyro::with_engine("login", engine);
    k.ok(&["open", &format!("{app}/login")]);
    let snap = k.ok(&["snapshot"]);
    let (user, pass, submit) = (ref_of(&snap, "Username"), ref_of(&snap, "Password"), ref_of(&snap, "Log in"));

    // Wrong password via click: the page shows an error, and the old refs are stale.
    let out = k.ok(&["fill", &user, "demo"]);
    assert_eq!(first(&out), format!("filled @{user}"));
    assert_eq!(out.lines().count(), 2, "fill reports only the changed field: {out}");
    assert!(out.contains(&format!("~ textbox \"Username\" [ref={user}] [value=\"demo\"]")), "{out}");
    k.ok(&["fill", &pass, "nope"]);
    let out = k.ok(&["click", &submit]);
    assert_eq!(first(&out), format!("clicked @{submit}"));
    assert!(out.contains("page changed:") && out.contains("Wrong username or password"), "{out}");
    assert!(k.ok(&["text"]).contains("Wrong username or password"));
    assert!(k.fails(&["fill", &user, "demo"]).contains("stale ref"));

    // Right password, submitted with Enter on the password field.
    let snap = k.ok(&["snapshot"]);
    k.ok(&["fill", &ref_of(&snap, "Username"), "demo"]);
    k.ok(&["fill", &ref_of(&snap, "Password"), "secret"]);
    let out = k.ok(&["press", "Enter", &ref_of(&snap, "Password")]);
    assert_eq!(first(&out), format!("pressed Enter on @{}", ref_of(&snap, "Password")));
    k.ok(&["wait", "--url", "/dashboard"]);

    // `text` is readable text, not HTML.
    let text = k.ok(&["text"]);
    assert!(text.contains("You are logged in.") && !text.contains("<p>"), "{text}");

    // fill fires real input events (the page echoes the field on `input`), and the diff shows the echo.
    let snap = k.ok(&["snapshot"]);
    let out = k.ok(&["fill", &ref_of(&snap, "Note"), "hello"]);
    k.ok(&["wait", "--text", "Note says: hello", "--timeout", "3000"]);
    assert!(out.lines().count() <= 4 && out.contains("hello"), "small diff expected: {out}");
}

fn first(out: &str) -> &str {
    out.lines().next().unwrap_or("")
}

#[test]
fn action_errors_are_clear_and_exit_nonzero() {
    let app = demo_app();
    let k = Klyro::with_engine("errors", ""); // automatic lanes
    k.ok(&["open", &format!("{app}/login")]);
    let snap = k.ok(&["snapshot"]);

    assert!(k.fails(&["click", "e99"]).contains("unknown ref @e99"));
    assert!(k.fails(&["fill", &ref_of(&snap, "Sign in"), "x"]).contains("is not a text field"));
    assert!(k.fails(&["press", "Hyper+q"]).contains("unknown modifier"));
    assert!(k.fails(&["wait", "--text", "never there", "--timeout", "300"]).contains("timed out"));
    // Caller mistakes are not engine faults: the session stays on its lane.
    if have_lightpanda() {
        assert!(k.ok(&["status"]).contains("lane: lightpanda (auto)
switches: none"), "{}", k.ok(&["status"]));
    }
    assert!(k.fails(&["open", "http://127.0.0.1:1/"]).contains("could not open"));
}

/// Checkpoint C: the demo flow with the router on, through the CLI, then a correct screenshot.
#[test]
fn demo_flow_with_router_then_screenshot() {
    let app = demo_app();
    let k = Klyro::with_engine("router-flow", "");
    k.ok(&["open", &format!("{app}/login")]);
    let snap = k.ok(&["snapshot", "-i"]);
    k.ok(&["fill", &ref_of(&snap, "Username"), "demo"]);
    k.ok(&["fill", &ref_of(&snap, "Password"), "secret"]);
    let out = k.ok(&["click", &ref_of(&snap, "Log in")]);
    assert!(out.contains("page changed:") && out.contains("Dashboard"), "{out}");

    let shot = k.home.join("dashboard.png");
    let (ok, out, err) = k.run(&["screenshot", shot.to_str().unwrap()]);
    assert!(ok, "{err}");
    assert!(out.contains("(1280x800,"), "{out}");
    assert_eq!(png_dims(&shot), (1280, 800));
    assert!(k.ok(&["text"]).contains("You are logged in."), "screenshot session must be logged in");
    if have_lightpanda() {
        assert!(err.contains("switched lane: lightpanda -> chromium"), "{err}");
        let status = k.ok(&["status"]);
        assert!(status.contains("lane: chromium (auto)") && status.contains("screenshot needs it"), "{status}");
    }
    if let Ok(dir) = std::env::var("KLYRO_KEEP_SHOT") {
        std::fs::copy(&shot, std::path::Path::new(&dir).join("checkpoint-c.png")).unwrap();
    }
}

#[test]
fn lightpanda_unsupported_is_typed() {
    if !have_lightpanda() {
        return;
    }
    let k = Klyro::new("lp-unsupported");
    let err = k.fails(&["open", "--headed", "--engine", "lightpanda", "about:blank"]);
    assert!(err.contains("is not supported here"), "{err}");
    assert!(k.fails(&["open", "--engine", "firefox", "about:blank"]).contains("unknown engine 'firefox'"));
}

/// (width, height) from a PNG file's IHDR chunk.
fn png_dims(path: &std::path::Path) -> (u32, u32) {
    let b = std::fs::read(path).unwrap();
    assert_eq!(&b[..8], b"\x89PNG\r\n\x1a\n", "{} is not a PNG", path.display());
    let be = |i: usize| u32::from_be_bytes(b[i..i + 4].try_into().unwrap());
    (be(16), be(20))
}

#[test]
fn screenshot() {
    let app = demo_app();
    let k = Klyro::new("screenshot");
    let dir = k.home.join("shots");
    std::fs::create_dir_all(&dir).unwrap();
    k.ok(&["open", &format!("{app}/long")]);

    let view = dir.join("view.png");
    let out = k.ok(&["screenshot", view.to_str().unwrap()]);
    assert!(out.contains("view.png (1280x800,"), "path and size printed: {out}");
    assert_eq!(png_dims(&view), (1280, 800));

    let full = dir.join("full.png");
    k.ok(&["screenshot", "--full", full.to_str().unwrap()]);
    let (_, h) = png_dims(&full);
    assert!(h > 3000, "full page should include the 3000px block, got height {h}");

    let snap = k.ok(&["snapshot"]);
    let el = dir.join("el.png");
    k.ok(&["screenshot", "--ref", &ref_of(&snap, "Top"), el.to_str().unwrap()]);
    let (w, h) = png_dims(&el);
    assert!(w > 10 && w <= 1280 && h > 10 && h < 200, "heading crop looks wrong: {w}x{h}");

    let json: serde_json::Value = serde_json::from_str(&k.ok(&["screenshot", "--json", view.to_str().unwrap()])).unwrap();
    assert_eq!((json["width"].as_u64(), json["height"].as_u64()), (Some(1280), Some(800)));

    assert!(k.fails(&["screenshot", "--ref", "e99", view.to_str().unwrap()]).contains("unknown ref"));
}

#[test]
fn screenshot_is_unsupported_on_lightpanda() {
    if !have_lightpanda() {
        return;
    }
    let k = Klyro::with_engine("shot-lp", "lightpanda");
    k.ok(&["open", &format!("{}/long", demo_app())]);
    let err = k.fails(&["screenshot", k.home.join("x.png").to_str().unwrap()]);
    assert!(err.contains("not supported here"), "{err}");
}

#[test]
fn status_shows_lane_and_switch_note() {
    if !have_lightpanda() {
        return;
    }
    let app = demo_app();
    let k = Klyro::with_engine("status", "");
    k.ok(&["open", &format!("{app}/login")]);
    assert!(k.ok(&["status"]).contains("lane: lightpanda (auto)"));

    let shot = k.home.join("s.png");
    let (ok, out, err) = k.run(&["screenshot", shot.to_str().unwrap()]);
    assert!(ok, "{err}");
    assert!(out.starts_with("saved "), "stdout stays clean: {out}");
    assert!(err.contains("note: switched lane: lightpanda -> chromium: screenshot needs it"), "{err}");

    let status = k.ok(&["status"]);
    assert!(status.contains("lane: chromium (auto)") && status.contains("screenshot needs it"), "{status}");
}

#[test]
fn snapshot_interactive_and_json_everywhere() {
    let app = demo_app();
    let k = Klyro::with_engine("json", "chromium");
    let opened: serde_json::Value = serde_json::from_str(&k.ok(&["--json", "open", &format!("{app}/login")])).unwrap();
    assert_eq!(opened["opened"], format!("{app}/login"));

    let i = k.ok(&["snapshot", "-i"]);
    assert!(i.lines().all(|l| l.starts_with("- ") && l.contains("[ref=")), "flat, refs only: {i}");
    assert!(i.contains("textbox \"Username\"") && !i.contains("heading"), "{i}");

    let user = ref_of(&i, "Username");
    let filled: serde_json::Value = serde_json::from_str(&k.ok(&["--json", "fill", &user, "demo"])).unwrap();
    assert_eq!(filled["done"], "filled");
    assert_eq!(filled["diff"]["changed"][0]["value"], "demo");

    for args in [&["--json", "text"][..], &["--json", "status"], &["--json", "wait", "10"]] {
        let out = k.ok(args);
        assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok(), "{args:?} is not JSON: {out}");
    }
    let closed: serde_json::Value = serde_json::from_str(&k.ok(&["--json", "close"])).unwrap();
    assert_eq!(closed["closed"], "default");
}

/// Tokens an agent reads per step: before = the action line plus a full snapshot to see what
/// happened; after = Klyro's action output (a diff). Run with --nocapture to see the table.
#[test]
fn token_budget() {
    let tokens = |s: &str| s.len().div_ceil(4); // ponytail: ~4 chars per token; a real tokenizer if this gets tuned
    let app = demo_app();
    let k = Klyro::with_engine("tokens", "chromium");
    k.ok(&["open", &format!("{app}/login")]);
    let snap = k.ok(&["snapshot"]);
    let (user, pass) = (ref_of(&snap, "Username"), ref_of(&snap, "Password"));

    let mut rows = vec![];
    let mut step = |name: &str, args: &[&str]| {
        let out = k.ok(args);
        let full = k.ok(&["snapshot"]);
        rows.push((name.to_string(), tokens(first(&out)) + tokens(&full), tokens(&out)));
    };
    step("fill username", &["fill", &user, "demo"]);
    step("fill password", &["fill", &pass, "secret"]);
    step("press Enter (navigates)", &["press", "Enter"]);
    let note = ref_of(&k.ok(&["snapshot"]), "Note");
    step("fill note", &["fill", &note, "hello"]);

    eprintln!("
| step | before (action + snapshot) | after (diff) |
|---|---|---|");
    for (n, b, a) in &rows {
        eprintln!("| {n} | {b} | {a} |");
    }
    let (b, a): (usize, usize) = rows.iter().fold((0, 0), |(x, y), r| (x + r.1, y + r.2));
    eprintln!("| total | {b} | {a} |");
    assert!(a * 2 < b, "diffs should at least halve the tokens: {a} vs {b}");
}

/// The sample Electron app, started with a DevTools port; stopped on drop.
struct ElectronApp(std::process::Child);

impl Drop for ElectronApp {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Electron from the fixture's `npm install`, or None (the test then skips).
fn electron_binary() -> Option<std::path::PathBuf> {
    let pkg = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/electron-app/node_modules/electron");
    let rel = std::fs::read_to_string(pkg.join("path.txt")).ok()?;
    let bin = pkg.join("dist").join(rel.trim());
    if bin.exists() {
        Some(bin)
    } else {
        eprintln!("SKIP: run `npm install` in tests/fixtures/electron-app");
        None
    }
}

#[test]
fn electron_connect_snapshot_click_screenshot() {
    let Some(bin) = electron_binary() else { return };
    let k = Klyro::new("electron");
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port().to_string();

    // Nothing listening yet: a clear error.
    assert!(k.fails(&["connect", &port]).contains(&format!("nothing is listening on port {port}")));

    let app_dir = bin.ancestors().nth(4).unwrap(); // .../electron-app/node_modules/electron/dist/<bin>
    let child = std::process::Command::new(&bin)
        .arg(".")
        .arg(format!("--remote-debugging-port={port}"))
        .current_dir(app_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let _app = ElectronApp(child);

    let end = Instant::now() + Duration::from_secs(20);
    let connected = loop {
        let (ok, out, err) = k.run(&["--session", "a", "connect", &port]);
        if ok || Instant::now() > end {
            assert!(ok, "could not connect: {err}");
            break out;
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    assert!(connected.contains("Klyro sample app"), "{connected}");

    let snap = k.ok(&["--session", "a", "snapshot"]);
    assert!(snap.contains("button \"Add one\""), "{snap}");
    let out = k.ok(&["--session", "a", "click", &ref_of(&snap, "Add one")]);
    assert!(out.contains("+ text: 1"), "click shows the counter change: {out}");

    let shot = k.home.join("app.png");
    k.ok(&["--session", "a", "screenshot", shot.to_str().unwrap()]);
    let (w, h) = png_dims(&shot);
    assert!(w > 100 && h > 100, "{w}x{h}");
    assert!(k.ok(&["--session", "a", "status"]).contains("attached to a running app"));

    // Closing the session detaches; the app keeps running.
    k.ok(&["--session", "a", "close"]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(std::net::TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).is_ok(), "app must still run");
}

fn fixture(name: &str) -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/docs").join(name).display().to_string()
}

#[test]
fn documents_pdf_markdown_html() {
    let k = Klyro::new("docs");

    // PDF: text per page, one page as an image.
    k.ok(&["open", &fixture("three-pages.pdf")]);
    let text = k.ok(&["text"]);
    assert!(text.contains("--- page 2 ---\nThe second page says hello"), "{text}");
    assert!(text.contains("--- page 3 ---\nPage three is the last page"), "{text}");
    assert!(k.ok(&["snapshot"]).contains("heading \"Page 3\""));
    let p2 = k.home.join("p2.png");
    k.ok(&["screenshot", "--page", "2", p2.to_str().unwrap()]);
    assert_eq!(png_dims(&p2), (918, 1188), "US Letter at 1.5x");
    assert!(k.fails(&["screenshot", "--page", "4", p2.to_str().unwrap()]).contains("this PDF has 3 page(s)"));

    // Markdown: rendered, relative images load from the document's folder.
    k.ok(&["open", &fixture("notes.md")]);
    let snap = k.ok(&["snapshot"]);
    assert!(snap.contains("heading \"Release notes\"") && snap.contains("cell \"42\""), "{snap}");
    assert!(!snap.contains("ListMarker"), "{snap}");
    let loaded = k.ok(&["--json", "text"]);
    assert!(loaded.contains("Item two"), "{loaded}");
    assert!(k.fails(&["screenshot", "--page", "1", p2.to_str().unwrap()]).contains("--page is for PDF documents"));

    // HTML file, opened by a relative path from the CLI's folder.
    let out = Command::new(env!("CARGO_BIN_EXE_klyro"))
        .args(["open", "page.html"])
        .current_dir(std::path::Path::new(&fixture("page.html")).parent().unwrap())
        .env("KLYRO_HOME", &k.home)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(k.ok(&["snapshot"]).contains("button \"Press me\""));

    // Office files: a clear "not yet", and no lane switch for it.
    let deck = k.home.join("deck.pptx");
    std::fs::write(&deck, b"x").unwrap();
    assert!(k.fails(&["open", deck.to_str().unwrap()]).contains(".pptx files are not supported yet"));
}

/// A minimal MCP client over the `klyro mcp` process's stdio.
struct Mcp {
    child: std::process::Child,
    out: std::io::BufReader<std::process::ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(home: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_klyro"))
            .arg("mcp")
            .env("KLYRO_HOME", home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let out = std::io::BufReader::new(child.stdout.take().unwrap());
        Mcp { child, out, next: 0 }
    }

    fn send(&mut self, msg: serde_json::Value) {
        use std::io::Write;
        writeln!(self.child.stdin.as_mut().unwrap(), "{msg}").unwrap();
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        use std::io::BufRead;
        self.next += 1;
        self.send(serde_json::json!({ "jsonrpc": "2.0", "id": self.next, "method": method, "params": params }));
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], self.next);
        v["result"].clone()
    }

    /// (text of the first text content, isError, full result)
    fn tool(&mut self, name: &str, args: serde_json::Value) -> (String, bool, serde_json::Value) {
        let r = self.request("tools/call", serde_json::json!({ "name": name, "arguments": args }));
        let text = r["content"].as_array().unwrap().iter().find(|c| c["type"] == "text").unwrap()["text"].as_str().unwrap().to_string();
        (text, r["isError"] == true, r)
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_client_lists_tools_and_completes_demo_flow() {
    let app = demo_app();
    let k = Klyro::with_engine("mcp", ""); // automatic lanes; its Drop closes the session
    let mut m = Mcp::start(&k.home);

    let init = m.request("initialize", serde_json::json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }));
    assert_eq!(init["serverInfo"]["name"], "klyro");
    m.send(serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let tools = m.request("tools/list", serde_json::json!({}));
    assert!(tools["tools"].as_array().unwrap().len() >= 11);

    let (out, err, _) = m.tool("open", serde_json::json!({ "url": format!("{app}/login") }));
    assert!(!err && out.starts_with("opened "), "{out}");
    let (snap, ..) = m.tool("snapshot", serde_json::json!({ "interactive": true }));
    // Same compact text as the CLI, from the same session.
    assert_eq!(snap.trim_end(), k.ok(&["snapshot", "-i"]).trim_end());

    let (out, ..) = m.tool("fill", serde_json::json!({ "ref": ref_of(&snap, "Username"), "text": "demo" }));
    assert!(out.contains("~ textbox \"Username\"") && out.contains("[value=\"demo\"]"), "{out}");
    m.tool("fill", serde_json::json!({ "ref": ref_of(&snap, "Password"), "text": "secret" }));
    let (out, ..) = m.tool("click", serde_json::json!({ "ref": ref_of(&snap, "Log in") }));
    assert!(out.contains("page changed:") && out.contains("Dashboard"), "{out}");

    let (out, err, full) = m.tool("screenshot", serde_json::json!({}));
    assert!(!err && out.contains("(1280x800,"), "{out}");
    let img = full["content"].as_array().unwrap().iter().find(|c| c["type"] == "image").expect("inline image");
    assert_eq!(img["mimeType"], "image/png");
    assert!(img["data"].as_str().unwrap().starts_with("iVBORw0KGgo"), "base64 PNG");

    let (text, ..) = m.tool("text", serde_json::json!({}));
    assert!(text.contains("You are logged in."), "{text}");

    let (out, err, _) = m.tool("click", serde_json::json!({ "ref": "e999" }));
    assert!(err && out.contains("unknown ref"), "errors come back as tool errors: {out}");
    let (out, err, _) = m.tool("close", serde_json::json!({}));
    assert!(!err && out.contains("closed session default"), "{out}");
}
