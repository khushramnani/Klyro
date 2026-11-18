//! Klyro benchmark (Task 14): the same tasks, run as the same logical script on three setups:
//! agent-browser on Chrome, klyro pinned to Chromium, and klyro with the router (fast lane first).
//!
//!   cargo run --release -p bench -- --all            all tasks, all setups
//!   cargo run --release -p bench -- --task docs      tasks whose file name contains "docs"
//!
//! Measured per task: success, tokens an agent reads (stdout + stderr, ~4 chars per token),
//! wall time, peak memory of the tool's processes, and (router) steps run on the fast lane.
//! Writes bench/RESULTS.md. agent-browser is optional: `npm install` in bench/agent-browser.

#[path = "../../tests/support/demo_app.rs"]
mod demo_app;

use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessesToUpdate, System};

#[derive(Deserialize)]
struct TaskFile {
    kind: String,
    #[serde(default)]
    needs: Vec<String>,
    steps: Vec<String>,
}

struct Task {
    name: String,
    kind: String,
    needs: Vec<String>,
    steps: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Setup {
    AgentBrowser,
    KlyroChromium,
    KlyroRouter,
}

impl Setup {
    fn label(self) -> &'static str {
        match self {
            Setup::AgentBrowser => "agent-browser",
            Setup::KlyroChromium => "klyro chromium",
            Setup::KlyroRouter => "klyro router",
        }
    }
}

#[derive(Default, Clone)]
struct Outcome {
    ok: bool,
    skipped: bool,
    reason: String,
    tokens: usize,
    secs: f64,
    peak_mb: f64,
    steps: usize,
    fast_steps: usize,
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn tokens(s: &str) -> usize {
    s.len().div_ceil(4) // ponytail: ~4 chars per token, the same rule for every setup
}

/// Splits on spaces, keeping "double quoted" parts together.
fn split(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (vec![], String::new(), false);
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The ref of the first `role "…name…"` line in a snapshot (klyro or agent-browser style).
fn find_ref(page: &str, role: &str, name: &str) -> Option<String> {
    let prefix = format!("{role} \"");
    page.lines().find_map(|line| {
        // Snapshot lines are "- role ...", diff lines "+ role ..." / "~ role ...".
        let l = line.trim_start();
        let l = l.strip_prefix("- ").or_else(|| l.strip_prefix("+ ")).or_else(|| l.strip_prefix("~ "))?;
        let rest = l.strip_prefix(&prefix)?;
        let shown = rest.split("\" [").next().unwrap_or(rest);
        if !shown.contains(name) {
            return None;
        }
        let r = &l[l.find("ref=")? + 4..];
        Some(r.chars().take_while(|c| c.is_ascii_alphanumeric()).collect())
    })
}

struct Tool {
    setup: Setup,
    exe: PathBuf,
    home: PathBuf,
    session: String,
    chrome: Option<PathBuf>,
}

impl Tool {
    /// (success, stdout, stderr)
    fn run(&self, args: &[String]) -> (bool, String, String) {
        let mut c = Command::new(&self.exe);
        c.arg("--session").arg(&self.session).args(args).stdin(Stdio::null());
        match self.setup {
            Setup::AgentBrowser => {
                if let Some(ch) = &self.chrome {
                    c.env("AGENT_BROWSER_EXECUTABLE_PATH", ch);
                }
            }
            Setup::KlyroChromium => {
                c.env("KLYRO_HOME", &self.home).env("KLYRO_ENGINE", "chromium");
            }
            Setup::KlyroRouter => {
                c.env("KLYRO_HOME", &self.home).env_remove("KLYRO_ENGINE");
            }
        }
        match c.output() {
            Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into()),
            Err(e) => (false, String::new(), e.to_string()),
        }
    }

    /// The router's current lane is the fast one (not counted: bookkeeping, not agent reading).
    fn on_fast_lane(&self) -> bool {
        let (_, out, _) = self.run(&["--json".into(), "status".into()]);
        out.contains("\"lane\":\"lightpanda\"")
    }
}

struct Ctx {
    demo: String,
    docs: PathBuf,
    electron_port: u16,
    shots: PathBuf,
}

fn run_task(task: &Task, tool: &Tool, ctx: &Ctx) -> Outcome {
    let mut o = Outcome::default();
    let ab = tool.setup == Setup::AgentBrowser;
    let (mut page, mut last_text, mut last_out) = (String::new(), String::new(), String::new());
    let mut fast = false;
    let start = Instant::now();
    for raw in &task.steps {
        let step = raw
            .replace("{demo}", &ctx.demo)
            .replace("{docs}", &ctx.docs.display().to_string())
            .replace("{electron_port}", &ctx.electron_port.to_string());
        let w = split(&step);
        let (cmd, rest) = (w[0].as_str(), &w[1..]);
        let arg = |i: usize| rest.get(i).cloned().unwrap_or_default();
        if cmd == "expect" {
            if !last_text.contains(&arg(0)) {
                o.reason = format!("page text did not contain {:?}", arg(0));
                return finish(o, start);
            }
            continue;
        }
        // Klyro's action output already contained the new page: no extra snapshot needed.
        if cmd == "snapshot" && !ab && last_out.contains("page changed:") {
            page = last_out.clone();
            continue;
        }
        let args: Vec<String> = match cmd {
            "open" if ab && Path::new(&arg(0)).is_file() => {
                let url = format!("file:///{}", arg(0).replace('\\', "/").trim_start_matches('/'));
                vec!["--allow-file-access".into(), "open".into(), url]
            }
            "open" | "connect" => vec![cmd.into(), arg(0)],
            "snapshot" => vec!["snapshot".into(), "-i".into()],
            "click" | "fill" => {
                let Some(r) = find_ref(&page, &arg(0), &arg(1)) else {
                    o.reason = format!("no {} {:?} in the snapshot", arg(0), arg(1));
                    return finish(o, start);
                };
                let r = if ab { format!("@{r}") } else { r };
                let mut v = vec![cmd.into(), r];
                if cmd == "fill" {
                    v.push(arg(2));
                }
                v
            }
            "press" => vec!["press".into(), arg(0)],
            "wait-text" => vec!["wait".into(), "--text".into(), arg(0)],
            "screenshot" => {
                let p = ctx.shots.join(format!("{}-{}.png", tool.setup.label().replace(' ', "-"), task.name));
                vec!["screenshot".into(), p.display().to_string()]
            }
            "text" if ab => vec!["get".into(), "text".into(), "body".into()],
            "text" => vec!["text".into()],
            other => {
                o.reason = format!("unknown step {other}");
                return finish(o, start);
            }
        };
        let (ok, out, err) = tool.run(&args);
        o.steps += 1;
        o.tokens += tokens(&out) + tokens(&err);
        if std::env::var_os("BENCH_VERBOSE").is_some() {
            eprintln!("    {:>5} tok  {step}", tokens(&out) + tokens(&err));
        }
        if tool.setup == Setup::KlyroRouter {
            if o.steps == 1 {
                fast = tool.on_fast_lane();
            } else if err.contains("switched lane") {
                fast = false;
            }
            o.fast_steps += fast as usize;
        }
        if !ok {
            let why = err.lines().chain(out.lines()).find(|l| !l.trim().is_empty()).unwrap_or("failed");
            o.reason = format!("`{}` failed: {}", step, why.trim());
            return finish(o, start);
        }
        match cmd {
            "snapshot" => page = out.clone(),
            "text" => last_text = out.clone(),
            "click" | "fill" | "press" if !ab => {
                if out.contains("page changed:") {
                    page = out.clone();
                } else {
                    // New elements appear in the diff with their refs.
                    page.extend(out.lines().filter(|l| l.starts_with("+ ")).map(|l| format!("{l}\n")));
                }
            }
            _ => {}
        }
        last_out = out;
    }
    o.ok = true;
    finish(o, start)
}

fn finish(mut o: Outcome, start: Instant) -> Outcome {
    o.secs = start.elapsed().as_secs_f64();
    o
}

/// Peak memory (bytes) of the tool's daemon and every process below it, sampled until stopped.
/// On Windows the router's Lightpanda runs inside WSL, so its memory is read there.
fn sample_memory(setup: Setup, home: PathBuf, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<u64> {
    std::thread::spawn(move || {
        let Ok(mut sys) = System::new() else { return 0 };
        let (mut peak, mut wsl_bytes, mut last_wsl) = (0u64, 0u64, Instant::now() - Duration::from_secs(10));
        while !stop.load(Ordering::Relaxed) {
            sys.refresh_processes(ProcessesToUpdate::All, true);
            let roots: Vec<Pid> = match setup {
                Setup::AgentBrowser => sys
                    .processes()
                    .iter()
                    .filter(|(_, p)| p.name().to_string_lossy().starts_with("agent-browser"))
                    .map(|(pid, _)| *pid)
                    .collect(),
                _ => std::fs::read(home.join("daemon.json"))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                    .and_then(|v| v["pid"].as_u64())
                    .map(|p| vec![Pid::from_u32(p as u32)])
                    .unwrap_or_default(),
            };
            let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
            for (pid, p) in sys.processes() {
                if let Some(parent) = p.parent() {
                    children.entry(parent).or_default().push(*pid);
                }
            }
            let (mut total, mut stack, mut seen) = (0u64, roots, std::collections::HashSet::new());
            while let Some(pid) = stack.pop() {
                if seen.insert(pid) {
                    total += sys.process(pid).map(|p| p.memory()).unwrap_or(0);
                    stack.extend(children.get(&pid).into_iter().flatten().copied());
                }
            }
            if cfg!(windows) && setup == Setup::KlyroRouter && last_wsl.elapsed() > Duration::from_secs(1) {
                wsl_bytes = wsl_lightpanda_bytes();
                last_wsl = Instant::now();
            }
            peak = peak.max(total + wsl_bytes);
            std::thread::sleep(Duration::from_millis(250));
        }
        peak
    })
}

fn wsl_lightpanda_bytes() -> u64 {
    let out = Command::new("wsl")
        .args(["-e", "sh", "-c", "ps -eo rss,args | grep '[l]ightpanda serve' | awk '{s+=$1} END {print s+0}'"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    out.ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().ok()).unwrap_or(0) * 1024
}

/// The sample Electron app on `port`, stopped on drop. None if its `npm install` was not run.
struct Electron(Child);

impl Electron {
    fn start(port: u16) -> Option<Self> {
        let pkg = root().join("tests/fixtures/electron-app/node_modules/electron");
        let bin = pkg.join("dist").join(std::fs::read_to_string(pkg.join("path.txt")).ok()?.trim());
        let child = Command::new(bin)
            .arg(".")
            .arg(format!("--remote-debugging-port={port}"))
            .current_dir(root().join("tests/fixtures/electron-app"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                std::thread::sleep(Duration::from_millis(500)); // window still loading
                return Some(Electron(child));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        None
    }
}

impl Drop for Electron {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn agent_browser_exe() -> Option<PathBuf> {
    let os = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        o => o,
    };
    let dir = root().join("bench/agent-browser/node_modules/agent-browser/bin");
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
        p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&format!("agent-browser-{os}")))
    })
}

fn load_tasks(filter: Option<&str>) -> Vec<Task> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root().join("bench/tasks"))
        .expect("bench/tasks")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .filter(|p| filter.is_none_or(|f| p.file_stem().unwrap().to_string_lossy().contains(f)))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let t: TaskFile = toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            Task { name: p.file_stem().unwrap().to_string_lossy().into(), kind: t.kind, needs: t.needs, steps: t.steps }
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let filter = args.iter().position(|a| a == "--task").and_then(|i| args.get(i + 1)).map(String::as_str);
    if !args.iter().any(|a| a == "--all") && filter.is_none() {
        eprintln!("usage: bench --all | --task <name part>");
        std::process::exit(2);
    }

    eprintln!("building klyro (release)...");
    let built = Command::new("cargo").args(["build", "--release", "--quiet", "-p", "klyro-cli"]).current_dir(root()).status();
    assert!(built.is_ok_and(|s| s.success()), "cargo build failed");
    let klyro = root().join("target/release").join(format!("klyro{}", std::env::consts::EXE_SUFFIX));

    let mut setups = vec![];
    match agent_browser_exe() {
        Some(_) => setups.push(Setup::AgentBrowser),
        None => eprintln!("agent-browser not installed (npm install in bench/agent-browser): skipping that setup"),
    }
    setups.extend([Setup::KlyroChromium, Setup::KlyroRouter]);

    let tasks = load_tasks(filter);
    let work = root().join("target/bench");
    let _ = std::fs::remove_dir_all(&work);
    let ctx = Ctx {
        demo: demo_app::demo_app(),
        docs: root().join("tests/fixtures/docs"),
        electron_port: 9455,
        shots: work.join("shots"),
    };
    std::fs::create_dir_all(&ctx.shots).unwrap();
    let chrome = klyro_adapter_web::find_chrome();

    let mut results: HashMap<(String, Setup), Outcome> = HashMap::new();
    for &setup in &setups {
        for task in &tasks {
            let tool = Tool {
                setup,
                exe: if setup == Setup::AgentBrowser { agent_browser_exe().unwrap() } else { klyro.clone() },
                home: work.join(setup.label().replace(' ', "-")),
                session: format!("bench-{}", task.name),
                chrome: chrome.clone(),
            };
            let _electron = if task.needs.iter().any(|n| n == "electron") {
                match Electron::start(ctx.electron_port) {
                    Some(e) => Some(e),
                    None => {
                        let o = Outcome { skipped: true, reason: "Electron not installed (npm install in tests/fixtures/electron-app)".into(), ..Default::default() };
                        results.insert((task.name.clone(), setup), o);
                        continue;
                    }
                }
            } else {
                None
            };
            let stop = Arc::new(AtomicBool::new(false));
            let sampler = sample_memory(setup, tool.home.clone(), stop.clone());
            let mut o = run_task(task, &tool, &ctx);
            stop.store(true, Ordering::Relaxed);
            o.peak_mb = sampler.join().unwrap_or(0) as f64 / 1_048_576.0;
            tool.run(&["close".into()]);
            eprintln!(
                "{:<16} {:<14} {:<4} {:>6} tok {:>6.1}s {:>7.0} MB {}",
                setup.label(),
                task.name,
                if o.ok { "ok" } else { "FAIL" },
                o.tokens,
                o.secs,
                o.peak_mb,
                o.reason
            );
            results.insert((task.name.clone(), setup), o);
        }
    }

    let mut report = report(&tasks, &setups, &results);
    println!("{report}");
    if filter.is_some() {
        return; // a partial run never overwrites the full results
    }
    // Keep the hand-written part (real-agent runs) across regenerations.
    let path = root().join("bench/RESULTS.md");
    if let Some(manual) = std::fs::read_to_string(&path).ok().and_then(|old| old.find(MANUAL).map(|i| old[i..].to_string())) {
        report.push('\n');
        report.push_str(&manual);
    }
    std::fs::write(path, report).unwrap();
    eprintln!("wrote bench/RESULTS.md");
}

fn report(tasks: &[Task], setups: &[Setup], r: &HashMap<(String, Setup), Outcome>) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Klyro benchmark results\n");
    let _ = writeln!(
        s,
        "Generated by `cargo run --release -p bench -- --all` on {} ({}). Each task is the same logical \
         script on every setup; tokens are what an agent would read (stdout + stderr, ~4 characters per \
         token); memory is the peak of the tool's daemon and all its processes (plus Lightpanda inside \
         WSL on Windows). agent-browser re-snapshots after navigation; klyro skips that when the action \
         already printed the new page.\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    let _ = writeln!(s, "## Summary\n");
    // Tokens are only comparable on tasks every setup finished: a failed run stops early and
    // reads less.
    let common: Vec<&Task> = tasks
        .iter()
        .filter(|t| setups.iter().all(|&st| r.get(&(t.name.clone(), st)).is_some_and(|o| o.ok)))
        .collect();
    let _ = writeln!(
        s,
        "| setup | success | tokens / task (the {} tasks all setups finished) | tokens / task (all) | time / task | peak memory | steps on fast lane |",
        common.len()
    );
    let _ = writeln!(s, "|---|---|---|---|---|---|---|");
    let mut summary: HashMap<Setup, (usize, usize, f64)> = HashMap::new(); // (succeeded, run, mean tokens on common tasks)
    for &setup in setups {
        let runs: Vec<&Outcome> = tasks.iter().filter_map(|t| r.get(&(t.name.clone(), setup))).filter(|o| !o.skipped).collect();
        let n = runs.len().max(1) as f64;
        let ok = runs.iter().filter(|o| o.ok).count();
        let tok = runs.iter().map(|o| o.tokens).sum::<usize>() as f64 / n;
        let secs = runs.iter().map(|o| o.secs).sum::<f64>() / n;
        let mb = runs.iter().map(|o| o.peak_mb).sum::<f64>() / n;
        let fast = if setup == Setup::KlyroRouter {
            let (f, all) = runs.iter().fold((0, 0), |(f, a), o| (f + o.fast_steps, a + o.steps));
            format!("{:.0}% ({f}/{all})", 100.0 * f as f64 / all.max(1) as f64)
        } else {
            "-".into()
        };
        let common_tok = common.iter().filter_map(|t| r.get(&(t.name.clone(), setup))).map(|o| o.tokens).sum::<usize>() as f64
            / common.len().max(1) as f64;
        summary.insert(setup, (ok, runs.len(), common_tok));
        let _ = writeln!(
            s,
            "| {} | {ok}/{} | {common_tok:.0} | {tok:.0} | {secs:.1} s | {mb:.0} MB | {fast} |",
            setup.label(),
            runs.len()
        );
    }

    let _ = writeln!(s, "\n## Per task\n");
    let mut header = "| task | kind |".to_string();
    let mut line = "|---|---|".to_string();
    for setup in setups {
        header.push_str(&format!(" {} |", setup.label()));
        line.push_str("---|");
    }
    let _ = writeln!(s, "{header}\n{line}");
    for t in tasks {
        let mut row = format!("| {} | {} |", t.name, t.kind);
        for &setup in setups {
            let cell = match r.get(&(t.name.clone(), setup)) {
                Some(o) if o.skipped => "skipped".to_string(),
                Some(o) => format!(
                    "{} {} tok, {:.1} s, {:.0} MB{}",
                    if o.ok { "ok" } else { "FAIL" },
                    o.tokens,
                    o.secs,
                    o.peak_mb,
                    if setup == Setup::KlyroRouter { format!(", fast {}/{}", o.fast_steps, o.steps) } else { String::new() }
                ),
                None => "-".into(),
            };
            row.push_str(&format!(" {cell} |"));
        }
        let _ = writeln!(s, "{row}");
    }

    let _ = writeln!(s, "\n## Failures\n");
    let mut any = false;
    for t in tasks {
        for &setup in setups {
            if let Some(o) = r.get(&(t.name.clone(), setup)).filter(|o| !o.ok) {
                any = true;
                let _ = writeln!(s, "- **{}** on {}: {}", t.name, setup.label(), o.reason);
            }
        }
    }
    if !any {
        let _ = writeln!(s, "None.");
    }

    // Checkpoint E bar from the plan.
    let _ = writeln!(s, "\n## Checkpoint E bar\n");
    let rate = |st: Setup| summary.get(&st).map(|(ok, n, _)| 100.0 * *ok as f64 / (*n).max(1) as f64);
    if let (Some(router), Some(chromium)) = (rate(Setup::KlyroRouter), rate(Setup::KlyroChromium)) {
        let _ = writeln!(s, "- Router success within 5 points of Chromium-only: {} ({router:.0}% vs {chromium:.0}%)", yes(chromium - router <= 5.0));
    }
    let fast: (usize, usize) = tasks
        .iter()
        .filter_map(|t| r.get(&(t.name.clone(), Setup::KlyroRouter)).filter(|o| !o.skipped))
        .fold((0, 0), |(f, a), o| (f + o.fast_steps, a + o.steps));
    let share = 100.0 * fast.0 as f64 / fast.1.max(1) as f64;
    let _ = writeln!(s, "- At least 60% of steps on the fast lane: {} ({share:.0}%)", yes(share >= 60.0));
    if let (Some((_, _, router)), Some((_, _, ab))) = (summary.get(&Setup::KlyroRouter), summary.get(&Setup::AgentBrowser)) {
        let _ = writeln!(
            s,
            "- Tokens per task no higher than agent-browser's (tasks both finished): {} ({router:.0} vs {ab:.0})",
            yes(router <= ab)
        );
    }
    s
}

/// Everything from this line on in RESULTS.md is written by hand and kept by the bench.
const MANUAL: &str = "<!-- hand-written below: kept when the bench regenerates this file -->";

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_quotes_together() {
        assert_eq!(split(r#"fill textbox "What needs to be done?" "buy milk""#), ["fill", "textbox", "What needs to be done?", "buy milk"]);
    }

    #[test]
    fn find_ref_in_both_snapshot_styles() {
        let klyro = "- heading \"Sign in\" [ref=e1] [level=1]\n- textbox \"Username\" [ref=e2]\n+ button \"Log in\" [ref=e9]";
        assert_eq!(find_ref(klyro, "textbox", "Username").as_deref(), Some("e2"));
        assert_eq!(find_ref(klyro, "button", "Log in").as_deref(), Some("e9"));
        let ab = "- heading \"Login Page\" [level=2, ref=e4]\n- button \"\u{f08b} Login\" [ref=e6]";
        assert_eq!(find_ref(ab, "button", "Login").as_deref(), Some("e6"));
        assert_eq!(find_ref(ab, "heading", "Login Page").as_deref(), Some("e4"));
        assert_eq!(find_ref(ab, "link", "Login"), None);
    }
}
