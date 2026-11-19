//! Accessibility pack: problems readable straight from the accessibility tree, so the check
//! works on every surface (web pages, Electron, Windows apps). Contrast needs computed
//! style and is not checked.

use crate::{Node, Snapshot};

/// Controls a user operates; each needs a name a screen reader can announce.
const CONTROLS: &[&str] = &[
    "link", "button", "textbox", "searchbox", "checkbox", "radio", "combobox", "listbox", "menuitem", "tab", "switch", "slider", "spinbutton",
];

/// One line per problem, in page order. Empty means nothing found.
pub fn a11y(s: &Snapshot) -> Vec<String> {
    let mut out = vec![];
    let mut last_level = 0;
    let mut headings = vec![];
    walk(&s.nodes, "", &mut |n, around| {
        let at = |what: &str| match &n.r#ref {
            Some(r) => format!("{what} [ref={r}]"),
            None if !around.is_empty() => format!("{what} (in {around})"),
            None => what.to_string(),
        };
        if CONTROLS.contains(&n.role.as_str()) && n.name.trim().is_empty() {
            out.push(format!("{} has no name: give it text, a label or aria-label", at(&n.role)));
        }
        if matches!(n.role.as_str(), "image" | "img") && n.name.trim().is_empty() {
            out.push(format!("{} has no alt text", at("image")));
        }
        if let (true, Some(l)) = (n.role == "heading", n.level) {
            if last_level > 0 && l > last_level + 1 {
                out.push(format!("{} jumps from level {last_level} to {l}", at(&format!("heading {:?}", n.name))));
            }
            last_level = l;
            headings.push(l);
        }
    });
    // Only pages that use headings at all: a desktop app has none.
    if !headings.is_empty() && !headings.contains(&1) {
        out.push("the page has headings but no level-1 heading".into());
    }
    out
}

/// Calls `f` on every node with a short description of its nearest ancestor with a ref or name.
fn walk(nodes: &[Node], around: &str, f: &mut impl FnMut(&Node, &str)) {
    for n in nodes {
        f(n, around);
        let inner = match (&n.r#ref, n.name.is_empty() || n.role == "text") {
            (Some(r), _) => format!("{} [ref={r}]", n.role),
            (None, false) => format!("{} {:?}", n.role, n.name),
            (None, true) => around.to_string(),
        };
        walk(&n.children, &inner, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(role: &str, name: &str, r: Option<&str>, children: Vec<Node>) -> Node {
        Node { r#ref: r.map(Into::into), children, ..Node::new(role, name) }
    }
    fn heading(name: &str, level: u32) -> Node {
        Node { level: Some(level), ..node("heading", name, Some("h"), vec![]) }
    }

    #[test]
    fn finds_unnamed_controls_images_and_heading_jumps() {
        let s = Snapshot {
            url: String::new(),
            title: String::new(),
            nodes: vec![
                heading("Shop", 1),
                node("link", "Home", Some("e2"), vec![node("image", "", None, vec![])]),
                node("button", "", Some("e3"), vec![]),
                node("button", "Buy", Some("e4"), vec![]),
                heading("Details", 3),
                node("image", "Logo", None, vec![]),
            ],
        };
        assert_eq!(
            a11y(&s),
            [
                "image (in link [ref=e2]) has no alt text",
                "button [ref=e3] has no name: give it text, a label or aria-label",
                "heading \"Details\" [ref=h] jumps from level 1 to 3",
            ]
        );
    }

    #[test]
    fn headings_without_h1_and_clean_pages() {
        let s = Snapshot { url: String::new(), title: String::new(), nodes: vec![heading("A", 2), heading("B", 3)] };
        assert_eq!(a11y(&s), ["the page has headings but no level-1 heading"]);
        let app = Snapshot { url: String::new(), title: String::new(), nodes: vec![node("button", "OK", Some("e1"), vec![])] };
        assert!(a11y(&app).is_empty(), "no headings at all is fine (desktop apps)");
    }
}
