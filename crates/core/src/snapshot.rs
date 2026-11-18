use serde::{Deserialize, Serialize};
use std::fmt::Write;

/// Live messages an agent must not miss (errors, confirmations).
const MESSAGE_ROLES: &[&str] = &["alert", "status", "alertdialog"];

/// One element in a snapshot. Text runs are nodes with role "text".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub role: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    /// Heading level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u32>,
    /// Current value of an input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(role: &str, name: &str) -> Self {
        Node { role: role.into(), name: name.into(), r#ref: None, level: None, value: None, children: vec![] }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub nodes: Vec<Node>,
}

impl Snapshot {
    /// agent-browser style: `- button "Submit" [ref=e3]`, two spaces per level.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for n in &self.nodes {
            render(n, 0, &mut out);
        }
        out
    }

    /// A snapshot of just one node (children included), for printing it.
    pub fn single(n: &Node) -> Snapshot {
        Snapshot { url: String::new(), title: String::new(), nodes: vec![n.clone()] }
    }

    /// Interactive elements only (refs, minus headings), as a flat list.
    pub fn interactive(&self) -> Snapshot {
        let nodes = crate::diff::flatten(&self.nodes)
            .into_iter()
            .filter(|n| n.r#ref.is_some() && n.role != "heading")
            .collect();
        Snapshot { url: self.url.clone(), title: self.title.clone(), nodes }
    }

    /// What an agent needs after landing on a new page: elements with refs (interactive ones
    /// and headings) plus alert/status messages with their text, as a flat list.
    pub fn outline(&self) -> Snapshot {
        fn text_of(n: &Node) -> String {
            let mut parts = vec![];
            if !n.name.is_empty() {
                parts.push(n.name.clone());
            }
            for c in &n.children {
                let t = text_of(c);
                if !t.is_empty() {
                    parts.push(t);
                }
            }
            parts.join(" ")
        }
        fn walk(nodes: &[Node], out: &mut Vec<Node>) {
            for n in nodes {
                if MESSAGE_ROLES.contains(&n.role.as_str()) {
                    let text = text_of(n);
                    if !text.is_empty() {
                        out.push(Node { name: text, children: vec![], ..n.clone() });
                    }
                    continue;
                }
                if n.r#ref.is_some() {
                    out.push(Node { children: vec![], ..n.clone() });
                }
                walk(&n.children, out);
            }
        }
        let mut nodes = vec![];
        walk(&self.nodes, &mut nodes);
        Snapshot { url: self.url.clone(), title: self.title.clone(), nodes }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("snapshot is always serializable")
    }
}

fn render(n: &Node, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    if n.role == "text" {
        let _ = writeln!(out, "{pad}- text: {}", n.name);
    } else {
        let _ = write!(out, "{pad}- {}", n.role);
        if !n.name.is_empty() {
            let _ = write!(out, " {:?}", n.name);
        }
        if let Some(r) = &n.r#ref {
            let _ = write!(out, " [ref={r}]");
        }
        if let Some(l) = n.level {
            let _ = write!(out, " [level={l}]");
        }
        if let Some(v) = &n.value {
            let _ = write!(out, " [value={v:?}]");
        }
        out.push('\n');
    }
    for c in &n.children {
        render(c, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Snapshot {
        let mut h = Node::new("heading", "Login");
        h.r#ref = Some("e1".into());
        h.level = Some(2);
        let mut tb = Node::new("textbox", "Username");
        tb.r#ref = Some("e2".into());
        tb.value = Some("tom".into());
        let mut form = Node::new("form", "");
        form.children = vec![Node::new("text", "Enter your name"), tb];
        Snapshot { url: "https://x.test/login".into(), title: "Login".into(), nodes: vec![h, form] }
    }

    #[test]
    fn text_format() {
        assert_eq!(
            sample().to_text(),
            "- heading \"Login\" [ref=e1] [level=2]\n\
             - form\n  \
               - text: Enter your name\n  \
               - textbox \"Username\" [ref=e2] [value=\"tom\"]\n"
        );
    }

    #[test]
    fn interactive_is_flat_refs_without_headings() {
        assert_eq!(sample().interactive().to_text(), "- textbox \"Username\" [ref=e2] [value=\"tom\"]\n");
    }

    #[test]
    fn outline_keeps_refs_headings_and_messages() {
        let mut alert = Node::new("alert", "");
        alert.children = vec![Node::new("text", "Wrong password")];
        let mut s = sample();
        s.nodes.push(alert);
        s.nodes.push(Node::new("paragraph", "just text"));
        assert_eq!(
            s.outline().to_text(),
            "- heading \"Login\" [ref=e1] [level=2]
- textbox \"Username\" [ref=e2] [value=\"tom\"]
- alert \"Wrong password\"
"
        );
    }

    #[test]
    fn quotes_in_names_are_escaped() {
        let s = Snapshot { url: String::new(), title: String::new(), nodes: vec![Node::new("button", "Say \"hi\"")] };
        assert_eq!(s.to_text(), "- button \"Say \\\"hi\\\"\"\n");
    }

    #[test]
    fn json_round_trip_and_ref_key() {
        let s = sample();
        let j = s.to_json();
        assert!(j.contains(r#""ref":"e1""#), "{j}");
        assert!(!j.contains("children\":[]"), "empty fields are skipped: {j}");
        assert_eq!(serde_json::from_str::<Snapshot>(&j).unwrap(), s);
    }
}
