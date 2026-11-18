//! Klyro key names ("Control+a", "Enter", "ArrowDown") -> uiautomation send_keys syntax.

pub fn to_send_keys(spec: &str) -> Result<String, String> {
    let mut parts: Vec<&str> = spec.split('+').collect();
    if spec.ends_with("++") {
        parts.truncate(parts.len() - 2);
        parts.push("+");
    }
    let key = parts.pop().filter(|k| !k.is_empty()).ok_or_else(|| format!("bad key '{spec}'"))?;
    let mut out = String::new();
    for m in parts {
        out.push_str(match m.to_ascii_lowercase().as_str() {
            "control" | "ctrl" => "{ctrl}",
            "alt" | "option" => "{alt}",
            "shift" => "{shift}",
            "meta" | "win" | "cmd" | "command" | "super" => "{win}",
            _ => return Err(format!("unknown modifier '{m}' in '{spec}'")),
        });
    }
    let named = match key {
        "Enter" | "Return" => Some("ENTER"),
        "Tab" => Some("TAB"),
        "Escape" | "Esc" => Some("ESC"),
        "Backspace" => Some("BACKSPACE"),
        "Delete" => Some("DELETE"),
        "Space" | " " => Some("SPACE"),
        "ArrowUp" | "Up" => Some("UP"),
        "ArrowDown" | "Down" => Some("DOWN"),
        "ArrowLeft" | "Left" => Some("LEFT"),
        "ArrowRight" | "Right" => Some("RIGHT"),
        "Home" => Some("HOME"),
        "End" => Some("END"),
        "PageUp" => Some("PAGEUP"),
        "PageDown" => Some("PAGEDOWN"),
        _ => None,
    };
    let body = if let Some(n) = named {
        format!("{{{n}}}")
    } else if let Some(f) = key.strip_prefix('F').filter(|n| n.parse::<u8>().is_ok_and(|n| (1..=24).contains(&n))) {
        format!("{{F{f}}}")
    } else {
        let mut chars = key.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return Err(format!("unknown key '{key}'; use a single character or Enter, Tab, Escape, Backspace, Delete, Space, Arrow*, Home, End, PageUp, PageDown, F1-F24"));
        };
        // `{ } ( )` are syntax in send_keys; quote them as `{c}`.
        if "{}()".contains(c) { format!("{{{c}}}") } else { c.to_string() }
    };
    // A held modifier applies to a following `( )` group.
    Ok(if out.is_empty() { body } else { format!("{out}({body})") })
}

#[cfg(test)]
mod tests {
    use super::to_send_keys as k;

    #[test]
    fn keys() {
        assert_eq!(k("Enter").unwrap(), "{ENTER}");
        assert_eq!(k("Control+a").unwrap(), "{ctrl}(a)");
        assert_eq!(k("Shift+Tab").unwrap(), "{shift}({TAB})");
        assert_eq!(k("F5").unwrap(), "{F5}");
        assert_eq!(k("(").unwrap(), "{(}");
        assert_eq!(k("Control++").unwrap(), "{ctrl}(+)");
        assert!(k("Hyper+a").is_err());
        assert!(k("Foo").is_err());
    }
}
