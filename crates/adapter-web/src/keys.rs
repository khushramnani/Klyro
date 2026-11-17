//! "Control+Shift+a" -> CDP Input.dispatchKeyEvent fields.

pub struct Key {
    pub key: String,
    pub code: String,
    pub vk: u32,
    /// Character the key types, if any (none while Control/Alt/Meta is held).
    pub text: Option<String>,
    /// CDP bit mask: Alt=1, Control=2, Meta=4, Shift=8.
    pub modifiers: u32,
}

pub fn parse(spec: &str) -> Result<Key, String> {
    let mut parts: Vec<&str> = spec.split('+').collect();
    // "Control++" means Control and the "+" key.
    if spec.ends_with("++") {
        parts.truncate(parts.len() - 2);
        parts.push("+");
    }
    let name = parts.pop().filter(|n| !n.is_empty()).ok_or_else(|| format!("bad key '{spec}'"))?;
    let mut modifiers = 0;
    for m in parts {
        modifiers |= match m.to_ascii_lowercase().as_str() {
            "alt" | "option" => 1,
            "control" | "ctrl" => 2,
            "meta" | "cmd" | "command" | "super" => 4,
            "shift" => 8,
            _ => return Err(format!("unknown modifier '{m}' in '{spec}'")),
        };
    }

    let named: Option<(&str, u32, Option<&str>)> = match name {
        "Enter" | "Return" => Some(("Enter", 13, Some("\r"))),
        "Tab" => Some(("Tab", 9, None)),
        "Escape" | "Esc" => Some(("Escape", 27, None)),
        "Backspace" => Some(("Backspace", 8, None)),
        "Delete" => Some(("Delete", 46, None)),
        "Space" | " " => Some((" ", 32, Some(" "))),
        "ArrowUp" | "Up" => Some(("ArrowUp", 38, None)),
        "ArrowDown" | "Down" => Some(("ArrowDown", 40, None)),
        "ArrowLeft" | "Left" => Some(("ArrowLeft", 37, None)),
        "ArrowRight" | "Right" => Some(("ArrowRight", 39, None)),
        "Home" => Some(("Home", 36, None)),
        "End" => Some(("End", 35, None)),
        "PageUp" => Some(("PageUp", 33, None)),
        "PageDown" => Some(("PageDown", 34, None)),
        _ => None,
    };
    let (key, code, vk, text) = match named {
        Some((k, vk, text)) => {
            let code = if k == " " { "Space" } else { k };
            (k.to_string(), code.to_string(), vk, text.map(String::from))
        }
        None => {
            let mut chars = name.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return Err(format!("unknown key '{name}'; use a single character or Enter, Tab, Escape, Backspace, Delete, Space, Arrow*, Home, End, PageUp, PageDown"));
            };
            let c = if modifiers & 8 != 0 { c.to_ascii_uppercase() } else { c };
            let (code, vk) = match c {
                'a'..='z' | 'A'..='Z' => (format!("Key{}", c.to_ascii_uppercase()), c.to_ascii_uppercase() as u32),
                '0'..='9' => (format!("Digit{c}"), c as u32),
                _ => (String::new(), 0),
            };
            (c.to_string(), code, vk, Some(c.to_string()))
        }
    };
    let text = if modifiers & (1 | 2 | 4) != 0 { None } else { text };
    Ok(Key { key, code, vk, text, modifiers })
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn named_keys_and_modifiers() {
        let k = parse("Enter").unwrap();
        assert_eq!((k.key.as_str(), k.vk, k.text.as_deref(), k.modifiers), ("Enter", 13, Some("\r"), 0));

        let k = parse("Control+a").unwrap();
        assert_eq!((k.key.as_str(), k.code.as_str(), k.vk, k.text, k.modifiers), ("a", "KeyA", 65, None, 2));

        let k = parse("Shift+Tab").unwrap();
        assert_eq!((k.key.as_str(), k.modifiers), ("Tab", 8));

        assert_eq!(parse("Control++").unwrap().key, "+");
    }

    #[test]
    fn bad_keys_are_errors() {
        assert!(parse("Foo").is_err());
        assert!(parse("Hyper+a").is_err());
        assert!(parse("").is_err());
    }
}
