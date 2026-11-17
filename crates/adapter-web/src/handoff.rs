//! What moves when a session changes engine: the URL, the cookies, form field values and ref
//! numbering. Other in-page JavaScript state does not move; `--engine chromium` keeps it.

use crate::page::WebPage;
use klyro_core::Result;
use serde_json::{Value, json};

impl WebPage {
    pub fn url(&mut self) -> Result<String> {
        Ok(self.eval("location.href")?.as_str().unwrap_or("").to_string())
    }

    /// Every cookie the browser holds.
    pub fn cookies(&mut self) -> Result<Vec<Value>> {
        let r = match self.cdp.call("Storage.getCookies", json!({})) {
            Ok(r) => r,
            Err(_) => self.cdp.call("Network.getCookies", json!({}))?,
        };
        Ok(r["cookies"].as_array().cloned().unwrap_or_default())
    }

    pub fn set_cookies(&mut self, cookies: &[Value]) -> Result<()> {
        let cookies: Vec<Value> = cookies.iter().filter_map(clean_cookie).collect();
        if !cookies.is_empty() {
            self.cdp.call("Network.setCookies", json!({ "cookies": cookies }))?;
        }
        Ok(())
    }

    /// Values of the page's form fields, in document order (null for fields that carry nothing).
    pub fn form_values(&mut self) -> Result<Value> {
        self.eval(COLLECT_FORM)
    }

    /// Puts saved form values back, firing input/change events so the page reacts as if typed.
    /// Skipped when the page has a different set of fields. Returns how many fields changed.
    pub fn restore_form_values(&mut self, saved: &Value) -> Result<usize> {
        if !saved.as_array().is_some_and(|a| a.iter().any(|v| !v.is_null())) {
            return Ok(0);
        }
        let n = self.eval(&format!("({RESTORE_FORM})({saved})"))?;
        Ok(n.as_u64().unwrap_or(0) as usize)
    }

    pub fn next_ref(&self) -> u32 {
        self.refs.next_index()
    }

    /// Continue ref numbering from the previous engine (call before the first snapshot).
    pub fn continue_refs_from(&mut self, next: u32) {
        self.refs = klyro_core::RefMap::starting_at(next);
    }
}

const COLLECT_FORM: &str = "[...document.querySelectorAll('input, textarea, select')].map(el => {
    const t = (el.type || '').toLowerCase();
    if (['hidden', 'file', 'submit', 'button', 'reset', 'image'].includes(t)) return null;
    return (t === 'checkbox' || t === 'radio') ? { checked: el.checked } : { value: el.value };
})";

const RESTORE_FORM: &str = "(saved) => {
    const els = [...document.querySelectorAll('input, textarea, select')];
    if (els.length !== saved.length) return 0;
    let n = 0;
    els.forEach((el, i) => {
        const s = saved[i];
        if (!s) return;
        if ('checked' in s) {
            const toggle = s.checked ? !el.checked : (el.checked && el.type === 'checkbox');
            if (toggle) { el.click(); n++; }
            return;
        }
        if (el.value === s.value) return;
        // The prototype's setter, so frameworks that track the value (React) see the change.
        const set = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), 'value')?.set;
        set ? set.call(el, s.value) : (el.value = s.value);
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
        n++;
    });
    return n;
}";

/// A getCookies entry -> a setCookies param. Keeps only fields setCookies accepts;
/// session cookies (expires <= 0, as Lightpanda reports them) get no expiry.
fn clean_cookie(c: &Value) -> Option<Value> {
    let mut out = json!({ "name": c["name"].as_str()?, "value": c["value"].as_str().unwrap_or("") });
    for key in ["domain", "path"] {
        if let Some(v) = c[key].as_str().filter(|v| !v.is_empty()) {
            out[key] = json!(v);
        }
    }
    out.get("domain")?;
    for key in ["secure", "httpOnly"] {
        if let Some(v) = c[key].as_bool() {
            out[key] = json!(v);
        }
    }
    if let Some(s) = c["sameSite"].as_str().filter(|s| ["Strict", "Lax", "None"].contains(s)) {
        out["sameSite"] = json!(s);
    }
    if let Some(e) = c["expires"].as_f64().filter(|e| *e > 0.0) {
        out["expires"] = json!(e);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::clean_cookie;
    use serde_json::json;

    #[test]
    fn session_cookie_from_lightpanda() {
        let c = json!({ "name": "sid", "value": "ok", "domain": "127.0.0.1", "path": "/", "expires": -1,
                        "httpOnly": false, "secure": false, "size": 5, "session": true, "sameSite": "Lax" });
        assert_eq!(
            clean_cookie(&c).unwrap(),
            json!({ "name": "sid", "value": "ok", "domain": "127.0.0.1", "path": "/", "secure": false,
                    "httpOnly": false, "sameSite": "Lax" })
        );
    }

    #[test]
    fn persistent_cookie_keeps_expiry_and_drops_bad_fields() {
        let c = json!({ "name": "a", "value": "b", "domain": ".x.test", "expires": 1900000000.5, "sameSite": "Weird" });
        let out = clean_cookie(&c).unwrap();
        assert_eq!(out["expires"], 1900000000.5);
        assert!(out.get("sameSite").is_none());
    }

    #[test]
    fn cookie_without_name_or_domain_is_skipped() {
        assert!(clean_cookie(&json!({ "value": "b", "domain": "x" })).is_none());
        assert!(clean_cookie(&json!({ "name": "a", "value": "b" })).is_none());
    }
}
