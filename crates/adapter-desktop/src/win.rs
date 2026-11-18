//! Windows apps through UI Automation.
//!
//! UIA objects belong to the thread that created them (COM), so each app session owns one
//! worker thread; the session handle sends it jobs. The window is attached by title, or
//! launched and found by process id.

use crate::keys::to_send_keys;
use klyro_core::{Action, Adapter, Capabilities, Error, Node, RefMap, Result, ScreenshotOptions, Snapshot, Wait};
use std::collections::HashMap;
use std::process::{Child, Command};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use uiautomation::patterns::{UIExpandCollapsePattern, UIInvokePattern, UISelectionItemPattern, UITogglePattern, UIValuePattern};
use uiautomation::types::{ControlType, ExpandCollapseState, ToggleState};
use uiautomation::inputs::Keyboard;
use uiautomation::{UIAutomation, UIElement, UITreeWalker};

/// Snapshot limits: some apps (File Explorer, IDEs) expose huge trees.
const MAX_DEPTH: usize = 30;
const MAX_NODES: usize = 3000;
/// UIA has no "page loaded" signal; give the app a moment to redraw after an action.
const SETTLE: Duration = Duration::from_millis(300);

type Job = Box<dyn FnOnce(&mut State) + Send>;

/// One native app window. Cheap to hold; the UIA work happens on its own thread.
pub struct DesktopApp {
    jobs: mpsc::Sender<Job>,
    title: String,
    launched: bool,
    last: Option<Snapshot>,
}

struct State {
    // Held so the UIA client outlives the elements and walker made from it.
    _uia: UIAutomation,
    walker: UITreeWalker,
    window: UIElement,
    refs: RefMap<Vec<i32>>,
    /// Elements from the latest snapshot, by runtime id.
    elements: HashMap<Vec<i32>, UIElement>,
    child: Option<Child>,
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(c) = &mut self.child {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn uia_err(e: uiautomation::Error) -> Error {
    Error::Engine(e.to_string())
}

impl DesktopApp {
    /// Attach to the first top-level window whose title contains `title` (any case).
    /// `close` leaves the app running.
    pub fn attach(title: &str) -> Result<Self> {
        let want = title.to_lowercase();
        let (shown, launched) = (title.to_string(), false);
        Self::start(move |uia, walker| {
            let found = top_windows(uia, walker)?.into_iter().find(|w| w.get_name().is_ok_and(|n| n.to_lowercase().contains(&want)));
            let window = found.ok_or_else(|| Error::Invalid(format!("no open window has {shown:?} in its title")))?;
            Ok((window, None))
        })
        .map(|(app, name)| DesktopApp { title: name, launched, ..app })
    }

    /// Start `command[0]` with the remaining arguments and attach to its first window.
    /// `close` stops it.
    pub fn launch(command: &[String]) -> Result<Self> {
        let (program, args) = command.split_first().ok_or_else(|| Error::Invalid("app --launch needs a program".into()))?;
        let (program, args) = (program.clone(), args.to_vec());
        Self::start(move |uia, walker| {
            let mut child = Command::new(&program)
                .args(&args)
                .spawn()
                .map_err(|e| Error::Invalid(format!("could not start {program}: {e}")))?;
            let pid = child.id();
            let end = Instant::now() + Duration::from_secs(20);
            while Instant::now() < end {
                if let Some(w) = top_windows(uia, walker)?.into_iter().find(|w| w.get_process_id().is_ok_and(|p| p == pid)) {
                    return Ok((w, Some(child)));
                }
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(Error::Engine(format!(
                        "{program} exited ({status}) without opening a window of its own; if it hands off to another \
                         process, start it yourself and use `klyro app \"<window title>\"`"
                    )));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            let _ = child.kill();
            Err(Error::Engine(format!("{program} opened no window within 20 s")))
        })
        .map(|(app, name)| DesktopApp { title: name, launched: true, ..app })
    }

    /// Spawns the worker thread, which builds its own UIA objects with `find_window`.
    fn start(
        find_window: impl FnOnce(&UIAutomation, &UITreeWalker) -> Result<(UIElement, Option<Child>)> + Send + 'static,
    ) -> Result<(Self, String)> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (ready_tx, ready) = mpsc::channel::<Result<String>>();
        std::thread::spawn(move || {
            // Physical pixels everywhere: element bounds then match the screen capture on
            // scaled displays (125%, 150%...). Without this, crops land in the wrong place.
            // SAFETY: changes only this worker thread's DPI awareness.
            unsafe {
                use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
                SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            }
            let init = (|| {
                let uia = UIAutomation::new().map_err(uia_err)?;
                let walker = uia.get_control_view_walker().map_err(uia_err)?;
                let (window, child) = find_window(&uia, &walker)?;
                let name = window.get_name().unwrap_or_default();
                Ok((State { _uia: uia, walker, window, refs: RefMap::new(), elements: HashMap::new(), child }, name))
            })();
            let mut state = match init {
                Ok((state, name)) => {
                    let _ = ready_tx.send(Ok(name));
                    state
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            // Runs until the session handle (and so the sender) is dropped.
            for job in inbox {
                job(&mut state);
            }
        });
        let name = ready.recv().map_err(|_| Error::Engine("the app worker stopped".into()))??;
        Ok((DesktopApp { jobs, title: name.clone(), launched: false, last: None }, name))
    }

    /// Runs `f` on the worker thread and waits for its answer.
    fn on_worker<T: Send + 'static>(&self, f: impl FnOnce(&mut State) -> Result<T> + Send + 'static) -> Result<T> {
        let (tx, rx) = mpsc::channel();
        self.jobs
            .send(Box::new(move |s| {
                let _ = tx.send(f(s));
            }))
            .map_err(|_| Error::Engine("the app session has ended".into()))?;
        rx.recv().map_err(|_| Error::Engine("the app session has ended".into()))?
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Klyro started this app (and `close` stops it).
    pub fn launched(&self) -> bool {
        self.launched
    }

    pub fn last_snapshot(&self) -> Option<&Snapshot> {
        self.last.as_ref()
    }
}

/// Top-level windows on the desktop.
fn top_windows(uia: &UIAutomation, walker: &UITreeWalker) -> Result<Vec<UIElement>> {
    let root = uia.get_root_element().map_err(uia_err)?;
    let mut out = vec![];
    let mut cur = walker.get_first_child(&root).ok();
    while let Some(w) = cur {
        cur = walker.get_next_sibling(&w).ok();
        out.push(w);
    }
    Ok(out)
}

/// UIA control type -> ARIA-like role, and whether it gets a ref.
fn role(ct: ControlType) -> Option<(&'static str, bool)> {
    use uiautomation::types::ControlType::*;
    Some(match ct {
        Button | SplitButton => ("button", true),
        CheckBox => ("checkbox", true),
        RadioButton => ("radio", true),
        ComboBox => ("combobox", true),
        Edit => ("textbox", true),
        Document => ("document", true),
        Hyperlink => ("link", true),
        ListItem => ("listitem", true),
        MenuItem => ("menuitem", true),
        TabItem => ("tab", true),
        TreeItem => ("treeitem", true),
        Slider => ("slider", true),
        Spinner => ("spinbutton", true),
        DataItem => ("row", true),
        HeaderItem => ("columnheader", true),
        List => ("list", false),
        Menu => ("menu", false),
        MenuBar => ("menubar", false),
        Tab => ("tablist", false),
        Tree => ("tree", false),
        Text => ("text", false),
        Image => ("image", false),
        Group => ("group", false),
        ToolBar => ("toolbar", false),
        StatusBar => ("status", false),
        DataGrid | Table => ("table", false),
        Header => ("header", false),
        ProgressBar => ("progressbar", false),
        ToolTip => ("tooltip", false),
        Window => ("window", false),
        Calendar => ("calendar", false),
        // Containers with nothing to say: their children move up.
        Pane | Custom | TitleBar => return None,
        // Noise for an agent.
        ScrollBar | Thumb | Separator => ("", false),
        _ => return None,
    })
}

impl State {
    fn snapshot(&mut self) -> Result<Snapshot> {
        self.refs.begin();
        self.elements.clear();
        let mut count = 0;
        let window = self.window.clone();
        let nodes = self.walk_children(&window, 0, &mut count);
        self.refs.finish();
        let title = self.window.get_name().unwrap_or_default();
        Ok(Snapshot { url: format!("app://{title}"), title, nodes })
    }

    fn walk_children(&mut self, parent: &UIElement, depth: usize, count: &mut usize) -> Vec<Node> {
        let mut out = vec![];
        if depth >= MAX_DEPTH {
            return out;
        }
        let mut cur = self.walker.get_first_child(parent).ok();
        while let Some(el) = cur {
            cur = self.walker.get_next_sibling(&el).ok();
            if *count >= MAX_NODES {
                break;
            }
            *count += 1;
            out.extend(self.node(&el, depth, count));
        }
        out
    }

    fn node(&mut self, el: &UIElement, depth: usize, count: &mut usize) -> Vec<Node> {
        let Ok(ct) = el.get_control_type() else { return vec![] };
        let Some((role, wants_ref)) = role(ct) else { return self.walk_children(el, depth + 1, count) };
        if role.is_empty() {
            return vec![];
        }
        let name = el.get_name().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
        let mut n = Node::new(role, &name);
        if role == "text" {
            return if name.is_empty() { vec![] } else { vec![n] };
        }
        if wants_ref && let Ok(id) = el.get_runtime_id() {
            n.r#ref = Some(self.refs.assign(id.clone()));
            self.elements.insert(id, el.clone());
        }
        n.value = value_of(el, ct);
        if !matches!(ct, ControlType::Edit | ControlType::ComboBox) {
            n.children = self.walk_children(el, depth + 1, count);
        }
        vec![n]
    }

    fn element(&self, r: &str) -> Result<UIElement> {
        let id = self.refs.resolve(r)?;
        self.elements.get(&id).cloned().ok_or_else(|| Error::StaleRef(r.trim_start_matches('@').into()))
    }

    fn focus_window(&self) {
        let _ = self.window.set_focus();
    }

    fn act(&mut self, target: Option<&str>, action: &Action) -> Result<()> {
        let need = |what: &str| target.ok_or_else(|| Error::Invalid(format!("{what} needs a ref, e.g. {what} e3")));
        match action {
            Action::Click => {
                let el = self.element(need("click")?)?;
                click(&el, &self.window)?;
            }
            Action::Fill(text) => {
                let r = need("fill")?;
                let el = self.element(r)?;
                let ct = el.get_control_type().map_err(uia_err)?;
                if !matches!(ct, ControlType::Edit | ControlType::Document | ControlType::ComboBox) {
                    return Err(Error::Invalid(format!("@{} is not a text field", r.trim_start_matches('@'))));
                }
                match el.get_pattern::<UIValuePattern>() {
                    Ok(v) if !v.is_readonly().unwrap_or(true) => v.set_value(text).map_err(uia_err)?,
                    // No settable value (some rich editors): select all and type.
                    _ => {
                        self.focus_window();
                        el.set_focus().map_err(uia_err)?;
                        Keyboard::new().send_keys("{ctrl}(a){DELETE}").map_err(uia_err)?;
                        el.send_text(text, 10).map_err(uia_err)?;
                    }
                }
            }
            Action::Press(key) => {
                let keys = to_send_keys(key).map_err(Error::Invalid)?;
                self.focus_window();
                if let Some(r) = target {
                    self.element(r)?.set_focus().map_err(uia_err)?;
                }
                Keyboard::new().send_keys(&keys).map_err(uia_err)?;
            }
        }
        std::thread::sleep(SETTLE);
        Ok(())
    }

    fn text(&mut self) -> String {
        let mut lines = vec![];
        let mut count = 0;
        fn collect(s: &State, el: &UIElement, depth: usize, count: &mut usize, lines: &mut Vec<String>) {
            if depth >= MAX_DEPTH || *count >= MAX_NODES {
                return;
            }
            let mut cur = s.walker.get_first_child(el).ok();
            while let Some(c) = cur {
                cur = s.walker.get_next_sibling(&c).ok();
                *count += 1;
                let ct = c.get_control_type().ok();
                let shown = match ct {
                    Some(ControlType::Edit | ControlType::Document) => c.get_pattern::<UIValuePattern>().and_then(|v| v.get_value()).ok(),
                    Some(ControlType::ScrollBar | ControlType::Thumb) => None,
                    _ => c.get_name().ok(),
                };
                if let Some(t) = shown.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
                    && lines.last() != Some(&t)
                {
                    lines.push(t);
                }
                collect(s, &c, depth + 1, count, lines);
            }
        }
        collect(self, &self.window.clone(), 0, &mut count, &mut lines);
        lines.join("\n")
    }

    fn screenshot(&mut self, opts: &ScreenshotOptions) -> Result<Vec<u8>> {
        if opts.page.is_some() {
            return Err(Error::Invalid("screenshot --page is for PDF documents".into()));
        }
        let el = match &opts.target {
            Some(r) => self.element(r)?,
            None => self.window.clone(),
        };
        // The capture reads the screen, so the window must be in front.
        self.focus_window();
        std::thread::sleep(Duration::from_millis(150));
        let shot = match (&opts.target, visible_frame(&self.window)) {
            // A whole window: its visible frame, not the invisible resize border around it.
            (None, Some(rect)) => uiautomation::screenshots::Screenshot::capture_rect(rect),
            _ => el.screenshot(),
        }
        .map_err(uia_err)?;
        let path = std::env::temp_dir().join(format!("klyro-app-shot-{}.png", std::process::id()));
        shot.save_png(&path).map_err(uia_err)?;
        let png = std::fs::read(&path).map_err(|e| Error::Engine(e.to_string()));
        let _ = std::fs::remove_file(&path);
        png
    }
}

/// The window's visible frame. Windows 10/11 report bounds that include an invisible ~7 px
/// resize border, which would capture whatever is behind the window.
fn visible_frame(window: &UIElement) -> Option<uiautomation::types::Rect> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
    let hwnd: HWND = window.get_native_window_handle().ok()?.into();
    let mut r = RECT::default();
    // SAFETY: a valid out-pointer to a RECT of the size we pass.
    unsafe { DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, (&raw mut r).cast(), size_of::<RECT>() as u32) }.ok()?;
    Some(uiautomation::types::Rect::new(r.left, r.top, r.right, r.bottom))
}

fn value_of(el: &UIElement, ct: ControlType) -> Option<String> {
    match ct {
        ControlType::Edit | ControlType::ComboBox | ControlType::Spinner | ControlType::Slider => {
            let v = el.get_pattern::<UIValuePattern>().ok()?;
            let password = el.is_password().unwrap_or(false);
            let s = v.get_value().ok()?;
            if s.is_empty() { None } else if password { Some("*****".into()) } else { Some(s) }
        }
        ControlType::CheckBox | ControlType::RadioButton => {
            let on = match el.get_pattern::<UITogglePattern>() {
                Ok(t) => t.get_toggle_state().ok().map(|s| s == ToggleState::On),
                Err(_) => el.get_pattern::<UISelectionItemPattern>().and_then(|s| s.is_selected()).ok(),
            }?;
            Some(if on { "checked" } else { "unchecked" }.into())
        }
        _ => None,
    }
}

/// The element's own action first (no mouse needed); a real mouse click only as a last resort.
fn click(el: &UIElement, window: &UIElement) -> Result<()> {
    if let Ok(p) = el.get_pattern::<UIInvokePattern>()
        && p.invoke().is_ok()
    {
        return Ok(());
    }
    if let Ok(p) = el.get_pattern::<UITogglePattern>()
        && p.toggle().is_ok()
    {
        return Ok(());
    }
    if let Ok(p) = el.get_pattern::<UISelectionItemPattern>()
        && p.select().is_ok()
    {
        return Ok(());
    }
    if let Ok(p) = el.get_pattern::<UIExpandCollapsePattern>() {
        let done = match p.get_state() {
            Ok(ExpandCollapseState::Collapsed) => p.expand(),
            _ => p.collapse(),
        };
        if done.is_ok() {
            return Ok(());
        }
    }
    let _ = window.set_focus();
    el.click().map_err(uia_err)
}

impl Adapter for DesktopApp {
    fn capabilities(&self) -> Capabilities {
        Capabilities { pixels: true, structure: true }
    }

    fn open(&mut self, _target: &str) -> Result<()> {
        Err(Error::Invalid("this session is a desktop app; open URLs and files in another session (--session)".into()))
    }

    fn snapshot(&mut self) -> Result<Snapshot> {
        let s = self.on_worker(|s| s.snapshot())?;
        self.last = Some(s.clone());
        Ok(s)
    }

    fn act(&mut self, target: Option<&str>, action: &Action) -> Result<()> {
        let (target, action) = (target.map(String::from), action.clone());
        self.on_worker(move |s| s.act(target.as_deref(), &action))
    }

    fn text(&mut self) -> Result<String> {
        self.on_worker(|s| Ok(s.text()))
    }

    fn wait(&mut self, cond: &Wait, timeout: Duration) -> Result<()> {
        match cond {
            Wait::Ms(ms) => {
                std::thread::sleep(Duration::from_millis(*ms));
                Ok(())
            }
            Wait::Text(t) => {
                let end = Instant::now() + timeout;
                while Instant::now() < end {
                    if self.text()?.contains(t.as_str()) {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(Error::Timeout(format!("timed out after {}s waiting for text {t:?}", timeout.as_secs_f32())))
            }
            Wait::Url(_) => Err(Error::Invalid("wait --url is for web pages".into())),
        }
    }

    fn screenshot(&mut self, opts: &ScreenshotOptions) -> Result<Vec<u8>> {
        let opts = opts.clone();
        self.on_worker(move |s| s.screenshot(&opts))
    }

    fn close(&mut self) -> Result<()> {
        // Stopping the worker drops its state, which stops a launched app. Attached apps keep running.
        let (tx, _) = mpsc::channel();
        self.jobs = tx;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles() {
        assert_eq!(role(ControlType::Edit), Some(("textbox", true)));
        assert_eq!(role(ControlType::Text), Some(("text", false)));
        assert_eq!(role(ControlType::Pane), None);
        assert_eq!(role(ControlType::ScrollBar), Some(("", false)));
    }
}
