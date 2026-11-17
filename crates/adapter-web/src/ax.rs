//! CDP accessibility tree -> Klyro snapshot nodes. Shared by every CDP engine.

use klyro_core::{Node, RefMap};
use serde_json::Value;
use std::collections::HashMap;

/// (loaderId, backendDOMNodeId): unique across documents, since some engines restart node ids per page.
pub type Handle = (String, i64);

/// Roles that get a ref. Headings too, so agents can point at sections.
const REF_ROLES: &[&str] = &[
    "link", "button", "textbox", "searchbox", "checkbox", "radio", "combobox", "listbox", "option",
    "menuitem", "menuitemcheckbox", "menuitemradio", "tab", "switch", "slider", "spinbutton", "heading",
];
/// Dropped with their subtree.
const DROP: &[&str] = &["InlineTextBox", "LineBreak"];
/// Not printed; their children move up one level.
const FLATTEN: &[&str] = &["generic", "none", "RootWebArea", "WebArea", "LabelText", "presentation"];

/// Returns (page title, top-level nodes). Assigns refs through `refs` (one snapshot).
pub fn build(nodes: &[Value], loader: &str, refs: &mut RefMap<Handle>) -> (String, Vec<Node>) {
    let by_id: HashMap<&str, &Value> = nodes.iter().filter_map(|n| Some((n["nodeId"].as_str()?, n))).collect();
    refs.begin();
    let mut w = Walk { by_id: &by_id, loader, refs };
    let (title, out) = match nodes.first() {
        Some(root) => (clean(str_at(root, "name")), w.walk(root, "")),
        None => (String::new(), vec![]),
    };
    refs.finish();
    (title, out)
}

struct Walk<'a> {
    by_id: &'a HashMap<&'a str, &'a Value>,
    loader: &'a str,
    refs: &'a mut RefMap<Handle>,
}

impl Walk<'_> {
    fn walk(&mut self, n: &Value, parent_name: &str) -> Vec<Node> {
        let role = n["role"]["value"].as_str().unwrap_or("");
        let name = clean(str_at(n, "name"));
        if DROP.contains(&role) {
            return vec![];
        }
        if role == "StaticText" {
            return if name.is_empty() || name == parent_name { vec![] } else { vec![Node::new("text", &name)] };
        }
        let ignored = n["ignored"].as_bool().unwrap_or(false);
        if ignored || FLATTEN.contains(&role) {
            return self.children(n, parent_name);
        }

        let mut node = Node::new(role, &name);
        if REF_ROLES.contains(&role)
            && let Some(b) = n["backendDOMNodeId"].as_i64()
        {
            node.r#ref = Some(self.refs.assign((self.loader.to_string(), b)));
        }
        node.level = property(n, "level").and_then(|v| v.as_u64()).map(|l| l as u32);
        let value = clean(str_at(n, "value"));
        if !value.is_empty() && value != name {
            node.value = Some(value);
        }
        node.children = self.children(n, &name);
        vec![node]
    }

    fn children(&mut self, n: &Value, parent_name: &str) -> Vec<Node> {
        let ids: Vec<&str> = n["childIds"].as_array().into_iter().flatten().filter_map(|c| c.as_str()).collect();
        ids.into_iter()
            .filter_map(|id| self.by_id.get(id).copied())
            .flat_map(|c| self.walk(c, parent_name))
            .collect()
    }
}

fn str_at<'v>(n: &'v Value, key: &str) -> &'v str {
    n[key]["value"].as_str().unwrap_or("")
}

fn property<'v>(n: &'v Value, name: &str) -> Option<&'v Value> {
    n["properties"].as_array()?.iter().find(|p| p["name"] == name).map(|p| &p["value"]["value"])
}

/// Collapse whitespace and drop private-use characters (icon fonts like " Login").
fn clean(s: &str) -> String {
    let s: String = s.chars().filter(|c| !('\u{E000}'..='\u{F8FF}').contains(c)).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ax(id: &str, role: &str, name: &str, backend: i64, children: &[&str]) -> Value {
        json!({ "nodeId": id, "role": { "value": role }, "name": { "value": name },
                "backendDOMNodeId": backend, "childIds": children })
    }

    #[test]
    fn builds_tree_with_refs_and_flattening() {
        let nodes = vec![
            ax("1", "RootWebArea", "Login", 1, &["2", "3"]),
            ax("2", "heading", "Sign in", 2, &["4"]),
            ax("3", "generic", "", 3, &["5", "6"]),
            ax("4", "StaticText", "Sign in", 4, &[]),
            ax("5", "button", "\u{f08b} Login", 5, &["7"]),
            ax("6", "paragraph", "", 6, &["8"]),
            ax("7", "StaticText", "Login", 7, &[]),
            ax("8", "StaticText", "Hello  world", 8, &[]),
        ];
        let mut refs = RefMap::new();
        let (title, out) = build(&nodes, "L1", &mut refs);
        let snap = klyro_core::Snapshot { url: String::new(), title, nodes: out };
        assert_eq!(snap.title, "Login");
        assert_eq!(
            snap.to_text(),
            "- heading \"Sign in\" [ref=e1]\n- button \"Login\" [ref=e2]\n- paragraph\n  - text: Hello world\n"
        );
        assert_eq!(refs.resolve("e2").unwrap(), ("L1".to_string(), 5));
    }

    #[test]
    fn same_backend_id_on_new_document_gets_new_ref() {
        let nodes = vec![ax("1", "RootWebArea", "", 1, &["2"]), ax("2", "button", "Go", 7, &[])];
        let mut refs = RefMap::new();
        build(&nodes, "L1", &mut refs);
        build(&nodes, "L2", &mut refs);
        assert!(refs.resolve("e1").is_err(), "old document's ref must be stale");
        assert_eq!(refs.resolve("e2").unwrap(), ("L2".to_string(), 7));
    }
}
