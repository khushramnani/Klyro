# Spike notes

## Task 1: Chromium over raw CDP

Run: `cargo run -p spike-chromium -- https://example.com [click e1]`

Client: our own thin client, sync `tungstenite` + `serde_json`, no TLS (ws://127.0.0.1 only).
Chose this over chromiumoxide so the same code can probe Lightpanda's partial CDP in Task 2
and show exactly which calls fail.

### Launch
`chrome --headless=new --remote-debugging-port=0 --user-data-dir=<tmp>`, then read
`<tmp>/DevToolsActivePort` (line 1 port, line 2 browser ws path). No HTTP `/json` call needed.
Chrome is killed and the profile removed on drop.

### CDP calls used
| Step | Calls |
|------|-------|
| Page session | `Target.createTarget {url: about:blank}`, `Target.attachToTarget {flatten: true}`; `sessionId` on every later call |
| Navigate | `Page.enable`, `Page.navigate`, wait for `Page.loadEventFired` |
| Snapshot | `Accessibility.getFullAXTree` |
| Click | `DOM.getDocument {depth:0}`, `DOM.scrollIntoViewIfNeeded {backendNodeId}`, `DOM.getBoxModel {backendNodeId}`, `Input.dispatchMouseEvent` x3 (moved/pressed/released at the content-quad centre) |
| URL check | `Runtime.evaluate {expression: location.href}` |

### How refs map to nodes
- Walk the AX tree from the root. Drop `ignored` nodes and `InlineTextBox`/`LineBreak`;
  flatten `generic`/`none`/`RootWebArea` (print their children one level up).
- `StaticText` prints as `- text: ...` unless it repeats the parent's name.
- A node gets a ref when it has a name or an interactive role (link, button, textbox, ...).
  Refs are `e1, e2, ...` in tree order, stored as `ref -> backendDOMNodeId`.
- `backendDOMNodeId` is stable for the life of the document, so it is what actions target.
  It does not survive navigation; the next snapshot rebuilds the map.

### Result (2026-11-17)
example.com no longer has an `<h1>`; the page is paragraphs plus one link:
```
- paragraph
  - text: This domain is for use in documentation examples ...
- link "Learn more" [ref=e1]
```
`click e1`: url before `https://example.com/`, after `https://www.iana.org/help/example-domains`.

### Gotchas
- Cross-origin click: `Page.loadEventFired` did not arrive on our session (new renderer
  process / target swap), but the URL did change. The spike now polls `location.href`; the
  daemon should do the same or watch `Target.targetInfoChanged`.
- `click()` falls back to `DOM.resolveNode` + `Runtime.callFunctionOn(this.click())` when the
  box model is missing or all zeros.
- Windows build: use the MSVC toolchain (VS Build Tools, C++ workload). The GNU toolchain
  failed on `getrandom` (its bundled `dlltool` needs an assembler that isn't shipped).

## Task 2: Lightpanda (1.0.4, in WSL2 on Windows)

Run: `cargo run -p spike-lightpanda -- <url> [click <ref> | probe]`

Same CDP code as Task 1 (`spikes/chromium/src/cdp.rs`, shared via `#[path]`). The spike starts
`~/lightpanda serve --host 127.0.0.1 --port 9333` inside WSL; WSL2 forwards the port to
Windows, ws URL is `ws://127.0.0.1:9333/`. Killing `wsl.exe` does not stop the Linux process,
so the spike also runs `wsl -e pkill -f "lightpanda serve.*9333"` on exit.
`KLYRO_LIGHTPANDA_WS` skips the launch and uses a running server.

### Snapshot shape: same, three sites
| Site | Chromium | Lightpanda |
|------|----------|------------|
| Static: example.com | 7 paragraphs + `link "Learn more"`; `click e1` goes to iana.org | Identical output; `click e1` goes to iana.org |
| Login demo: the-internet.herokuapp.com/login | 10 refs; textboxes, `button " Login"` | 15 refs; same textboxes, `button "Login"` |
| JS app (React): demo.playwright.dev/todomvc | 5 refs; heading, textbox | 5 refs; same heading and textbox |

Differences (all fixable in our tree walk, none block):
- Role names differ slightly: Chromium `sectionheader` vs Lightpanda `region` > `banner`;
  Chromium emits `LabelText`, Lightpanda emits plain text.
- Lightpanda names `emphasis` nodes, so they get refs. Task 3 should give refs only to
  interactive roles + headings, which makes both engines match.
- Chromium hides `Fork me on GitHub` (off screen / CSS); Lightpanda lists it. **Lightpanda
  does not know visibility.** Hidden elements will show up in fast-lane snapshots.
- Chromium keeps icon-font text in names (`" Login"`); trim names.

### Gaps (probe on the login page)
Every probed call returned success on Lightpanda: `DOM.getDocument/describeNode/getBoxModel/
focus/resolveNode`, `Accessibility.queryAXTree/getPartialAXTree`, `Input.insertText/
dispatchKeyEvent/dispatchMouseEvent`, `Runtime.evaluate`, `Network.getCookies/setCookies`,
`Storage.getCookies`, `Page.getLayoutMetrics/captureScreenshot/getFrameTree`,
`Emulation.setDeviceMetricsOverride`. "Success" is not the same as "real":
- `DOM.getBoxModel` is fake: every element is a 5x5 box stacked down the page. Mouse clicks
  at those coords still worked on example.com, but they must not be trusted. **The fast
  lane should click with JS `this.click()`, never coordinates.**
- `Page.captureScreenshot` returns a PNG of a plain text layout: no CSS, input boxes are not
  drawn. **Not usable as pixels; the router must send every screenshot to Chromium.**
- Cookies come back with `expires: -1` and no leading-dot domain. Check the handoff copies
  them correctly in Task 8.
- `Page.loadEventFired` arrives normally.

### Fast lane per system
- Linux: Lightpanda native (official builds). Not run yet on this machine; CI in Task 3.
- macOS: Lightpanda native (official builds). Not run yet; CI in Task 3.
- Windows: Lightpanda in WSL2 works (tested above). Without WSL: Chromium only.
- Obscura on native Windows: **not tested.** It is not installed, and downloading a release
  binary was blocked in this session. Open item for Checkpoint A.

### Checkpoint A input
Both spikes produce the same snapshot shape. Lightpanda gives a usable snapshot on all three
site kinds. Recommendation: **go on the fast lane**, with JS clicks and every screenshot on
Chromium.
