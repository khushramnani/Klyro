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
  process / target swap), but the URL did change. The daemon should wait on
  `Target.targetInfoChanged` or poll `location.href`, not only the load event.
- Mouse clicks need layout (`DOM.getBoxModel`). Lightpanda has no layout, so Task 2 needs a
  `DOM.resolveNode` + `Runtime.callFunctionOn(this.click())` fallback; `click()` is the one
  function to swap.
- Windows build: use the MSVC toolchain (VS Build Tools, C++ workload). The GNU toolchain
  failed on `getrandom` (its bundled `dlltool` needs an assembler that isn't shipped).
