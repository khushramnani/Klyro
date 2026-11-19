//! Layout pack: problems only a real layout engine can see, measured in the page.

use crate::page::WebPage;
use klyro_core::{Error, Result};
use serde_json::json;

/// Returns {w: viewport width, out: [problem, ...]}. Heuristics, tuned to stay quiet on
/// deliberate tricks: skip links parked fully off-screen, screen-reader-only text (1px boxes),
/// and text truncated on purpose (ellipsis, line clamp) are not reported.
const CHECK: &str = r#"(() => {
  const W = innerWidth, H = innerHeight, out = [];
  const d = el => {
    const t = el.tagName.toLowerCase();
    const n = (el.getAttribute('aria-label') || el.innerText || el.value || el.getAttribute('alt') || '').trim().replace(/\s+/g, ' ').slice(0, 40);
    return n ? `${t} "${n}"` : t + (el.id ? '#' + el.id : el.classList[0] ? '.' + el.classList[0] : '');
  };
  const shown = el => {
    const s = getComputedStyle(el), r = el.getBoundingClientRect();
    return s.visibility !== 'hidden' && r.width > 2 && r.height > 2;
  };
  const all = [...document.body.querySelectorAll('*')].filter(shown);
  const sw = document.documentElement.scrollWidth;
  if (sw > W + 1) {
    // The outermost elements that stick out; their children stick out with them.
    const sticks = el => el.getBoundingClientRect().right > W + 1;
    const wide = all.filter(el => sticks(el) && !(el.parentElement !== document.body && sticks(el.parentElement)));
    out.push(`the page is ${sw}px wide in a ${W}px viewport, so it scrolls sideways` +
      (wide.length ? ` (sticking out: ${wide.slice(0, 3).map(d).join(', ')})` : ''));
  }
  const controls = all.filter(el => el.matches('a[href], button, input:not([type=hidden]), select, textarea, [role=button], [role=link], [tabindex]:not([tabindex="-1"])'));
  for (const el of controls) {
    const r = el.getBoundingClientRect();
    if ((r.left < 0 && r.right > 0) || (r.left < W && r.right > W + 1 && sw <= W + 1)) out.push(`${d(el)} is cut off by the edge of the page`);
    const x = r.left + r.width / 2, y = r.top + r.height / 2;
    if (x < 0 || y < 0 || x >= W || y >= H) continue; // only what is on screen now
    const top = document.elementFromPoint(x, y);
    if (top && top !== el && !el.contains(top) && !top.contains(el)) out.push(`${d(el)} is covered by ${d(top)}`);
  }
  for (const el of all) {
    if (el.children.length || !el.textContent.trim()) continue;
    const s = getComputedStyle(el);
    if (s.textOverflow === 'ellipsis' || (s.webkitLineClamp && s.webkitLineClamp !== 'none')) continue;
    const cut = (/hidden|clip/.test(s.overflowX) && el.scrollWidth > el.clientWidth + 1) || (/hidden|clip/.test(s.overflowY) && el.scrollHeight > el.clientHeight + 1);
    if (cut) out.push(`text is cut off in ${d(el)}`);
  }
  return { w: W, out: out.length > 20 ? [...out.slice(0, 20), `... and ${out.length - 20} more`] : out };
})()"#;

impl WebPage {
    /// (viewport width, problems). `width`: check at this viewport width, then put it back.
    pub fn layout_problems(&mut self, width: Option<u32>) -> Result<(u32, Vec<String>)> {
        if !self.engine().has_layout() {
            return Err(Error::Unsupported("the layout check"));
        }
        if let Some(w) = width {
            let h = self.eval("innerHeight")?.as_u64().unwrap_or(800);
            self.cdp.call("Emulation.setDeviceMetricsOverride", json!({ "width": w, "height": h, "deviceScaleFactor": 1, "mobile": false }))?;
        }
        let r = self.eval(CHECK);
        if width.is_some() {
            self.restore_viewport()?;
        }
        let v = r?;
        let problems = v["out"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)).collect();
        Ok((v["w"].as_u64().unwrap_or(0) as u32, problems))
    }
}
