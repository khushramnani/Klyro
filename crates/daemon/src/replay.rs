//! Record once, replay without a model. While a session records, every successful command
//! that changes something becomes a step. A ref is saved as role + name (+ nth when several
//! match), because refs are per run; replay finds the element again on the live page.

use crate::ipc::{self, Request, Response};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Commands worth replaying: changes, waits and test assertions. Reads (snapshot, text,
/// status) change nothing.
const RECORDED: &[&str] = &["open", "connect", "app", "click", "fill", "press", "wait", "expect", "check", "close"];

/// Appends to recording files take turns.
static WRITE: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub cmd: String,
    /// The command's arguments, minus the ref (that is `target`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub headed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub role: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub nth: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Where a command's ref goes in its args.
pub fn ref_slot(cmd: &str) -> Option<usize> {
    match cmd {
        "click" | "fill" => Some(0),
        "press" => Some(1),
        _ => None,
    }
}

/// The ref this request acts on, if it is being recorded (so its element must be located
/// before the action changes the page).
pub fn ref_to_locate(req: &Request) -> Option<&str> {
    if !recording(&req.session) {
        return None;
    }
    req.args.get(ref_slot(&req.cmd)?).map(String::as_str)
}

/// A session records while its file exists: one JSON step per line, appended as it goes, so a
/// recording outlives daemon restarts and never keeps the daemon alive.
fn file(session: &str) -> PathBuf {
    let name: String = session.bytes().map(|b| format!("{b:02x}")).collect();
    ipc::home().join("recordings").join(format!("{name}.jsonl"))
}

pub fn recording(session: &str) -> bool {
    file(session).exists()
}

/// Starts (or restarts, dropping earlier steps) the session's recording.
pub fn start(session: &str) -> Result<(), String> {
    let f = file(session);
    std::fs::create_dir_all(f.parent().unwrap()).and_then(|_| std::fs::write(&f, "")).map_err(|e| format!("could not start recording: {e}"))
}

/// Ends the recording and saves it to `to` as a JSON array. Returns the step count.
pub fn stop(session: &str, to: &Path) -> Result<usize, String> {
    let _turn = WRITE.lock().unwrap();
    let f = file(session);
    let text = std::fs::read_to_string(&f).map_err(|_| format!("session '{session}' is not recording: run `klyro record start` first"))?;
    let steps = text.lines().filter(|l| !l.is_empty()).map(serde_json::from_str).collect::<Result<Vec<Step>, _>>().map_err(|e| format!("damaged recording {}: {e}", f.display()))?;
    let body = serde_json::to_string_pretty(&steps).map_err(|e| e.to_string())?;
    std::fs::write(to, body + "\n").map_err(|e| format!("could not write {}: {e}", to.display()))?;
    let _ = std::fs::remove_file(f);
    Ok(steps.len())
}

/// Adds a successful command to the session's recording, if it records. `target`: the located
/// ref. Returns a note when the step will not replay as recorded.
pub fn push(req: &Request, resp: &Response, target: Option<Target>) -> Option<String> {
    if !resp.ok || !RECORDED.contains(&req.cmd.as_str()) || !recording(&req.session) {
        return None;
    }
    let mut args = req.args.clone();
    let mut note = None;
    if let Some(slot) = ref_slot(&req.cmd).filter(|s| *s < args.len()) {
        if target.is_some() {
            args.remove(slot);
        } else {
            note = Some(format!("recording: could not identify @{} by role and name; the step keeps the ref and may not replay", args[slot].trim_start_matches('@')));
        }
    }
    let (headed, engine) = if req.cmd == "open" { (req.headed, req.engine.clone()) } else { (false, None) };
    let step = Step { cmd: req.cmd.clone(), args, target, headed, engine };
    let line = serde_json::to_string(&step).expect("steps serialize") + "\n";
    let _turn = WRITE.lock().unwrap();
    let saved = std::fs::OpenOptions::new().append(true).open(file(&req.session)).and_then(|mut f| f.write_all(line.as_bytes()));
    if let Err(e) = saved {
        return Some(format!("recording: could not save this step: {e}"));
    }
    note
}

impl Step {
    /// One line, like the command an agent would type: `click button "Log in"`.
    pub fn describe(&self) -> String {
        // args: [kind, is|not, timeout, value, name?] -> `expect --not --role button --name "X"`
        if let [kind, mode, _, value, rest @ ..] = self.args.as_slice()
            && self.cmd == "expect"
        {
            let not = if mode == "not" { " --not" } else { "" };
            let name = rest.first().map(|n| format!(" --name {n:?}")).unwrap_or_default();
            return format!("expect{not} --{kind} {value:?}{name}");
        }
        let mut parts = vec![self.cmd.clone()];
        if let Some(t) = &self.target {
            let nth = if t.nth > 0 { format!(" #{}", t.nth + 1) } else { String::new() };
            parts.push(format!("{} {:?}{nth}", t.role, t.name));
        }
        parts.extend(self.args.iter().map(|a| if a.contains(' ') { format!("{a:?}") } else { a.clone() }));
        parts.join(" ")
    }

    /// The request for this step, with `r` (the element's current ref) put back in place.
    pub fn request(&self, session: &str, r: Option<String>) -> Request {
        let mut args = self.args.clone();
        if let (Some(r), Some(slot)) = (r, ref_slot(&self.cmd)) {
            args.insert(slot.min(args.len()), r);
        }
        Request { token: String::new(), session: session.into(), cmd: self.cmd.clone(), args, json: false, headed: self.headed, engine: self.engine.clone() }
    }
}

/// Runs the steps in order through `run` (the daemon's dispatch); `find` returns the current
/// ref of a target, or None while it is not on the page. Stops at the first failing step.
pub fn replay(
    session: &str,
    steps: &[Step],
    timeout: Duration,
    mut find: impl FnMut(&Target) -> Result<Option<String>, String>,
    mut run: impl FnMut(&Request) -> Response,
) -> Result<String, String> {
    let mut done = vec![];
    for (i, step) in steps.iter().enumerate() {
        let line = format!("{}/{} {}", i + 1, steps.len(), step.describe());
        let fail = |done: &[String], why: String| {
            let mut out = done.join("\n");
            if !out.is_empty() {
                out.push('\n');
            }
            Err(format!("{out}{line}  FAILED: {why}"))
        };
        let r = match &step.target {
            None => None,
            // The page may still be loading: look again until it shows up.
            Some(t) => {
                let end = Instant::now() + timeout;
                loop {
                    match find(t) {
                        Ok(Some(r)) => break Some(r),
                        Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(250)),
                        Ok(None) => return fail(&done, format!("no {} {:?} on the page", t.role, t.name)),
                        Err(e) => return fail(&done, e),
                    }
                }
            }
        };
        let resp = run(&step.request(session, r));
        if !resp.ok {
            return fail(&done, resp.out);
        }
        done.push(format!("{line}  ok"));
    }
    done.push(format!("replayed {} steps", steps.len()));
    Ok(done.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(session: &str, cmd: &str, args: &[&str]) -> Request {
        Request { token: String::new(), session: session.into(), cmd: cmd.into(), args: args.iter().map(|a| a.to_string()).collect(), json: false, headed: false, engine: None }
    }
    fn ok() -> Response {
        Response { ok: true, out: String::new(), notes: vec![] }
    }
    fn button(name: &str) -> Option<Target> {
        Some(Target { role: "button".into(), name: name.into(), nth: 0 })
    }

    #[test]
    fn records_changes_not_reads_and_drops_refs() {
        let home = std::env::temp_dir().join(format!("klyro-unit-{}", std::process::id()));
        // SAFETY: the only test in this crate that touches the state dir.
        unsafe { std::env::set_var("KLYRO_HOME", &home) };
        start("rec-a").unwrap();
        assert!(ref_to_locate(&req("rec-a", "press", &["Enter", "@e4"])).is_some_and(|r| r == "@e4"));
        assert!(ref_to_locate(&req("rec-other", "click", &["e4"])).is_none(), "other sessions are not recording");
        push(&req("rec-a", "open", &["https://x.test"]), &ok(), None);
        push(&req("rec-a", "snapshot", &[]), &ok(), None);
        push(&req("rec-a", "click", &["e3"]), &Response { ok: false, ..ok() }, button("Nope"));
        push(&req("rec-a", "press", &["Enter", "e4"]), &ok(), button("Go"));
        assert!(push(&req("rec-a", "click", &["e5"]), &ok(), None).is_some_and(|n| n.contains("@e5")));
        let out = home.join("out.json");
        assert_eq!(stop("rec-a", &out).unwrap(), 3);
        let steps: Vec<Step> = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(steps.iter().map(Step::describe).collect::<Vec<_>>(), ["open https://x.test", "press button \"Go\" Enter", "click e5"]);
        assert_eq!(steps[1].request("s", Some("e9".into())).args, ["Enter", "e9"]);
        let e = Step { cmd: "expect".into(), args: vec!["role".into(), "not".into(), "5000".into(), "alert".into()], target: None, headed: false, engine: None };
        assert_eq!(e.describe(), "expect --not --role \"alert\"");
        assert!(stop("rec-a", &out).unwrap_err().contains("not recording"));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn replay_finds_targets_and_stops_at_first_failure() {
        let steps: Vec<Step> = serde_json::from_str(
            r#"[{"cmd":"open","args":["https://x.test"]},
                {"cmd":"fill","target":{"role":"textbox","name":"User"},"args":["tom"]},
                {"cmd":"click","target":{"role":"button","name":"Missing"}}]"#,
        )
        .unwrap();
        let mut sent = vec![];
        let err = replay(
            "s",
            &steps,
            Duration::ZERO,
            |t| Ok((t.name == "User").then(|| "e7".to_string())),
            |r| {
                sent.push(format!("{} {}", r.cmd, r.args.join(" ")));
                ok()
            },
        )
        .unwrap_err();
        assert_eq!(sent, ["open https://x.test", "fill e7 tom"]);
        assert!(err.ends_with("3/3 click button \"Missing\"  FAILED: no button \"Missing\" on the page"), "{err}");
        assert!(err.contains("2/3 fill textbox \"User\" tom  ok"), "{err}");
    }
}
