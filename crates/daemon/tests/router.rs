//! Router: start on the fast lane, move to Chromium when a step needs pixels.
//! Needs Lightpanda and Chrome; skips (loudly) without Lightpanda.

#[path = "../../../tests/support/demo_app.rs"]
mod demo_app;

use demo_app::demo_app;
use klyro_adapter_web::{Engine, find_lightpanda};
use klyro_core::{Action, Adapter, Error, Node, ScreenshotOptions, Wait};
use klyro_daemon::router::Router;
use std::sync::Mutex;
use std::time::Duration;

/// Browser tests take turns (see the CLI tests).
static TURN: Mutex<()> = Mutex::new(());

fn have_lightpanda() -> bool {
    let found = find_lightpanda().is_some();
    if !found {
        eprintln!("SKIP: Lightpanda not installed");
    }
    found
}

/// Ref of the first node with this role and name.
fn find(nodes: &[Node], role: &str, name: &str) -> Option<String> {
    nodes.iter().find_map(|n| {
        if n.role == role && n.name == name { n.r#ref.clone() } else { find(&n.children, role, name) }
    })
}

fn log_in(r: &mut Router, app: &str) {
    r.open(&format!("{app}/login")).unwrap();
    let s = r.snapshot().unwrap();
    r.act(find(&s.nodes, "textbox", "Username").as_deref(), &Action::Fill("demo".into())).unwrap();
    r.act(find(&s.nodes, "textbox", "Password").as_deref(), &Action::Fill("secret".into())).unwrap();
    r.act(None, &Action::Press("Enter".into())).unwrap();
    r.wait(&Wait::Url("/dashboard".into()), Duration::from_secs(10)).unwrap();
}

#[test]
fn router_login_on_fast_lane_then_screenshot_shows_logged_in_page() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    if !have_lightpanda() {
        return;
    }
    let app = demo_app();
    let mut r = Router::start(None, false).unwrap();
    assert_eq!(r.lane(), Engine::Lightpanda, "auto sessions start on the fast lane");

    log_in(&mut r, &app);
    assert_eq!(r.lane(), Engine::Lightpanda, "the login itself stays on the fast lane");

    let png = r.screenshot(&ScreenshotOptions::default()).unwrap();
    assert_eq!(&png[..4], b"\x89PNG");
    assert_eq!(r.lane(), Engine::Chromium);
    assert_eq!(r.switches().len(), 1);
    assert!(r.switches()[0].contains("screenshot needs it"), "{:?}", r.switches());
    assert!(r.take_notes().len() == 1 && r.take_notes().is_empty(), "each switch is reported once");
    assert!(r.status().contains("lane: chromium (auto)"), "{}", r.status());

    // The screenshot was taken of the logged-in page: cookies and URL came across.
    assert!(r.text().unwrap().contains("You are logged in."));
    r.close().unwrap();
}

#[test]
fn router_maps_refs_across_a_switch() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    if !have_lightpanda() {
        return;
    }
    let app = demo_app();
    let mut r = Router::start(None, false).unwrap();
    r.open(&format!("{app}/login")).unwrap();
    let s = r.snapshot().unwrap();
    let user = find(&s.nodes, "textbox", "Username").unwrap();

    r.screenshot(&ScreenshotOptions::default()).unwrap(); // moves to Chromium
    assert_eq!(r.lane(), Engine::Chromium);

    // The agent still holds the fast-lane ref; it must hit the same field on Chromium.
    r.act(Some(&user), &Action::Fill("demo".into())).unwrap();
    let s = r.snapshot().unwrap();
    let field = find(&s.nodes, "textbox", "Username").unwrap();
    assert_ne!(field, user, "Chromium numbers its refs after the fast lane's");
    assert!(r.text().unwrap().contains("Sign in"));
    r.close().unwrap();
}

#[test]
fn router_off_when_engine_is_pinned() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    if !have_lightpanda() {
        return;
    }
    let mut r = Router::start(Some(Engine::Lightpanda), false).unwrap();
    r.open(&format!("{}/login", demo_app())).unwrap();
    assert!(matches!(r.screenshot(&ScreenshotOptions::default()), Err(Error::Unsupported(_))));
    assert_eq!(r.lane(), Engine::Lightpanda);
    assert!(r.switches().is_empty());
    assert!(r.status().contains("pinned by --engine"));
    r.close().unwrap();
}

#[test]
fn router_timeouts_do_not_switch() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    if !have_lightpanda() {
        return;
    }
    let mut r = Router::start(None, false).unwrap();
    r.open(&format!("{}/login", demo_app())).unwrap();
    let res = r.wait(&Wait::Text("never there".into()), Duration::from_millis(300));
    assert!(matches!(res, Err(Error::Timeout(_))), "{res:?}");
    assert_eq!(r.lane(), Engine::Lightpanda);
    r.close().unwrap();
}
