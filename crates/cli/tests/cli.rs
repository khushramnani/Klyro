//! End-to-end: real `klyro` binary, real daemon, real Chromium, local demo app.

mod common;
use common::{Klyro, demo_app, have_lightpanda, ref_of};
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
    assert_eq!(k.ok(&["fill", &user, "demo"]).trim(), format!("filled @{user}"));
    k.ok(&["fill", &pass, "nope"]);
    assert_eq!(k.ok(&["click", &submit]).trim(), format!("clicked @{submit}"));
    assert!(k.ok(&["text"]).contains("Wrong username or password"));
    assert!(k.fails(&["fill", &user, "demo"]).contains("stale ref"));

    // Right password, submitted with Enter on the password field.
    let snap = k.ok(&["snapshot"]);
    k.ok(&["fill", &ref_of(&snap, "Username"), "demo"]);
    k.ok(&["fill", &ref_of(&snap, "Password"), "secret"]);
    assert_eq!(k.ok(&["press", "Enter", &ref_of(&snap, "Password")]).trim(), format!("pressed Enter on @{}", ref_of(&snap, "Password")));
    k.ok(&["wait", "--url", "/dashboard"]);

    // `text` is readable text, not HTML.
    let text = k.ok(&["text"]);
    assert!(text.contains("You are logged in.") && !text.contains("<p>"), "{text}");

    // fill fires real input events (the page echoes the field on `input`).
    let snap = k.ok(&["snapshot"]);
    k.ok(&["fill", &ref_of(&snap, "Note"), "hello"]);
    k.ok(&["wait", "--text", "Note says: hello", "--timeout", "3000"]);
}

#[test]
fn action_errors_are_clear_and_exit_nonzero() {
    let app = demo_app();
    let k = Klyro::new("errors");
    k.ok(&["open", &format!("{app}/login")]);
    let snap = k.ok(&["snapshot"]);

    assert!(k.fails(&["click", "e99"]).contains("unknown ref @e99"));
    assert!(k.fails(&["fill", &ref_of(&snap, "Sign in"), "x"]).contains("is not a text field"));
    assert!(k.fails(&["press", "Hyper+q"]).contains("unknown modifier"));
    assert!(k.fails(&["wait", "--text", "never there", "--timeout", "300"]).contains("timed out"));
    assert!(k.fails(&["open", "http://127.0.0.1:1/"]).contains("could not open"));
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
