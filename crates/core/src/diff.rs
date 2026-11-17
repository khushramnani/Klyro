//! What changed between two snapshots, so actions can report a few lines instead of the page.
//!
//! Elements with refs are matched by ref (refs are stable across snapshots). Other nodes
//! (text, named containers) are matched by role and name as a multiset.

use crate::{Node, Snapshot};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Diff {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<(String, String)>,
    pub added: Vec<Node>,
    pub removed: Vec<Node>,
    /// The element as it is now.
    pub changed: Vec<Node>,
    /// Nodes in the newer snapshot, to judge how big the change is.
    #[serde(skip)]
    pub after_len: usize,
    /// Refs present before and after.
    #[serde(skip)]
    pub kept_refs: usize,
    /// Refs present before.
    #[serde(skip)]
    pub before_refs: usize,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.url.is_none() && self.title.is_none() && self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }

    /// A new document (no element survived) or most of the page is new: the full snapshot
    /// is then shorter and clearer than the diff.
    pub fn is_page_change(&self) -> bool {
        let new_document = self.before_refs > 0 && self.kept_refs == 0;
        new_document || self.added.len() + self.removed.len() > (self.after_len / 2).max(10)
    }

    /// `+ added`, `- removed`, `~ changed`, one element per line.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if let Some((a, b)) = &self.url {
            out.push_str(&format!("url: {a} -> {b}\n"));
        }
        if let Some((a, b)) = &self.title {
            out.push_str(&format!("title: {a:?} -> {b:?}\n"));
        }
        for (sign, nodes) in [("+", &self.added), ("-", &self.removed), ("~", &self.changed)] {
            for n in nodes {
                out.push_str(sign);
                out.push(' ');
                out.push_str(Snapshot::single(n).to_text().trim_start_matches("- "));
            }
        }
        out
    }
}

/// Every node that carries meaning (a ref or a name), without children, in document order.
pub(crate) fn flatten(nodes: &[Node]) -> Vec<Node> {
    let mut out = vec![];
    fn walk(nodes: &[Node], out: &mut Vec<Node>) {
        for n in nodes {
            if n.r#ref.is_some() || !n.name.is_empty() {
                out.push(Node { children: vec![], ..n.clone() });
            }
            walk(&n.children, out);
        }
    }
    walk(nodes, &mut out);
    out
}

pub fn diff(before: &Snapshot, after: &Snapshot) -> Diff {
    let (a, b) = (flatten(&before.nodes), flatten(&after.nodes));
    let mut d = Diff { after_len: b.len(), ..Diff::default() };
    if before.url != after.url {
        d.url = Some((before.url.clone(), after.url.clone()));
    }
    if before.title != after.title {
        d.title = Some((before.title.clone(), after.title.clone()));
    }

    // Elements with refs: by ref.
    let a_refs: HashMap<&str, &Node> = a.iter().filter_map(|n| Some((n.r#ref.as_deref()?, n))).collect();
    let b_refs: HashMap<&str, &Node> = b.iter().filter_map(|n| Some((n.r#ref.as_deref()?, n))).collect();
    d.before_refs = a_refs.len();
    d.kept_refs = a_refs.keys().filter(|r| b_refs.contains_key(*r)).count();
    for n in &b {
        if let Some(r) = n.r#ref.as_deref() {
            match a_refs.get(r) {
                None => d.added.push(n.clone()),
                Some(old) if *old != n => d.changed.push(n.clone()),
                _ => {}
            }
        }
    }

    // Everything else: by (role, name), counting duplicates.
    let key = |n: &Node| (n.role.clone(), n.name.clone());
    let mut count: HashMap<(String, String), i64> = HashMap::new();
    for n in a.iter().filter(|n| n.r#ref.is_none()) {
        *count.entry(key(n)).or_default() += 1;
    }
    for n in b.iter().filter(|n| n.r#ref.is_none()) {
        let c = count.entry(key(n)).or_default();
        if *c > 0 {
            *c -= 1;
        } else {
            d.added.push(n.clone());
        }
    }
    for n in &a {
        match n.r#ref.as_deref() {
            Some(r) if !b_refs.contains_key(r) => d.removed.push(n.clone()),
            None => {
                let c = count.entry(key(n)).or_default();
                if *c > 0 {
                    *c -= 1;
                    d.removed.push(n.clone());
                }
            }
            _ => {}
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(role: &str, name: &str, r: Option<&str>) -> Node {
        Node { r#ref: r.map(String::from), ..Node::new(role, name) }
    }

    fn snap(url: &str, nodes: Vec<Node>) -> Snapshot {
        Snapshot { url: url.into(), title: "T".into(), nodes }
    }

    fn login_form(user_value: Option<&str>) -> Vec<Node> {
        let mut user = el("textbox", "Username", Some("e2"));
        user.value = user_value.map(String::from);
        let mut form = el("form", "", None);
        form.children = vec![user, el("textbox", "Password", Some("e3")), el("button", "Log in", Some("e4"))];
        vec![el("heading", "Sign in", Some("e1")), form, el("text", "Use demo / secret.", None)]
    }

    #[test]
    fn diff_reports_only_what_changed() {
        let before = snap("u", login_form(None));
        let mut after_nodes = login_form(Some("demo"));
        after_nodes.insert(1, el("text", "Wrong username or password", None));
        after_nodes[2].children.pop(); // the button is gone
        let d = diff(&before, &snap("u", after_nodes));

        assert_eq!(d.added, [el("text", "Wrong username or password", None)]);
        assert_eq!(d.removed, [el("button", "Log in", Some("e4"))]);
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].value.as_deref(), Some("demo"));
        assert!(!d.is_page_change());
        assert_eq!(
            d.to_text(),
            "+ text: Wrong username or password\n- button \"Log in\" [ref=e4]\n~ textbox \"Username\" [ref=e2] [value=\"demo\"]\n"
        );
    }

    #[test]
    fn small_page_reload_is_a_page_change() {
        // Same URL, same few elements, but every ref is new: a fresh document.
        let renumbered: Vec<Node> = vec![el("heading", "Sign in", Some("e5")), el("button", "Log in", Some("e6"))];
        let before = snap("u", vec![el("heading", "Sign in", Some("e1")), el("button", "Log in", Some("e2"))]);
        assert!(diff(&before, &snap("u", renumbered)).is_page_change());
    }

    #[test]
    fn diff_of_identical_snapshots_is_empty() {
        let s = snap("u", login_form(None));
        assert!(diff(&s, &s).is_empty());
    }

    #[test]
    fn diff_counts_repeated_text() {
        let before = snap("u", vec![el("text", "Item", None)]);
        let after = snap("u", vec![el("text", "Item", None), el("text", "Item", None)]);
        assert_eq!(diff(&before, &after).added, [el("text", "Item", None)]);
        assert_eq!(diff(&after, &before).removed, [el("text", "Item", None)]);
    }

    #[test]
    fn diff_flags_navigation_as_page_change() {
        let before = snap("a", login_form(None));
        let after = snap("b", (0..30).map(|i| el("link", &format!("L{i}"), Some(&format!("e{}", 10 + i)))).collect());
        let d = diff(&before, &after);
        assert_eq!(d.url, Some(("a".into(), "b".into())));
        assert!(d.is_page_change());
        assert!(d.to_text().starts_with("url: a -> b\n"));
    }
}
