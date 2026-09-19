//! `press_key`: key names and combos (`Enter`, `Tab`, `ArrowDown`, `Control+A`, `Shift+Tab`, `a`,
//! `F5`) → DevTools `Input.dispatchKeyEvent` fields. Browser accelerators (Ctrl+W, Ctrl+T, …) never
//! see DevTools key events, so an agent can't use them to control the browser itself.

/// `Input.dispatchKeyEvent` modifier bits.
pub const MOD_ALT: i32 = 1;
pub const MOD_CTRL: i32 = 2;
pub const MOD_META: i32 = 4;
pub const MOD_SHIFT: i32 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPress {
    /// DOM `key` value.
    pub key: String,
    /// DOM `code` value.
    pub code: String,
    pub windows_virtual_key_code: i32,
    /// Text the key inserts (only without Ctrl/Alt/Meta).
    pub text: Option<String>,
    pub modifiers: i32,
}

fn named(key: &str) -> Option<(&'static str, &'static str, i32, Option<&'static str>)> {
    // (key, code, vk, text)
    Some(match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => ("Enter", "Enter", 13, Some("\r")),
        "tab" => ("Tab", "Tab", 9, None),
        "escape" | "esc" => ("Escape", "Escape", 27, None),
        "backspace" => ("Backspace", "Backspace", 8, None),
        "delete" | "del" => ("Delete", "Delete", 46, None),
        "space" | " " => (" ", "Space", 32, Some(" ")),
        "arrowup" | "up" => ("ArrowUp", "ArrowUp", 38, None),
        "arrowdown" | "down" => ("ArrowDown", "ArrowDown", 40, None),
        "arrowleft" | "left" => ("ArrowLeft", "ArrowLeft", 37, None),
        "arrowright" | "right" => ("ArrowRight", "ArrowRight", 39, None),
        "home" => ("Home", "Home", 36, None),
        "end" => ("End", "End", 35, None),
        "pageup" => ("PageUp", "PageUp", 33, None),
        "pagedown" => ("PageDown", "PageDown", 34, None),
        "insert" => ("Insert", "Insert", 45, None),
        _ => return None,
    })
}

fn modifier(name: &str) -> Option<i32> {
    match name.to_ascii_lowercase().as_str() {
        "control" | "ctrl" => Some(MOD_CTRL),
        "shift" => Some(MOD_SHIFT),
        "alt" | "option" => Some(MOD_ALT),
        "meta" | "cmd" | "command" | "win" | "super" => Some(MOD_META),
        _ => None,
    }
}

/// Parses `key` (a key name or a `+`-separated combo). `Err` explains what's wrong.
pub fn parse(combo: &str) -> Result<KeyPress, String> {
    let combo = combo.trim();
    if combo.is_empty() {
        return Err("empty key".into());
    }
    // "+" alone or a trailing "++" means the plus key.
    let (mods_part, key_part) = if combo == "+" {
        ("", "+")
    } else if let Some(stripped) = combo.strip_suffix("++") {
        (stripped, "+")
    } else {
        match combo.rsplit_once('+') {
            Some((m, k)) => (m, k),
            None => ("", combo),
        }
    };
    let mut modifiers = 0;
    for m in mods_part.split('+').filter(|m| !m.is_empty()) {
        modifiers |= modifier(m).ok_or_else(|| format!("unknown modifier {m:?}"))?;
    }
    let texty = modifiers & (MOD_CTRL | MOD_ALT | MOD_META) == 0;
    if let Some(mods) = modifier(key_part)
        && mods_part.is_empty()
    {
        // A modifier on its own.
        let (key, code, vk) = match mods {
            MOD_CTRL => ("Control", "ControlLeft", 17),
            MOD_SHIFT => ("Shift", "ShiftLeft", 16),
            MOD_ALT => ("Alt", "AltLeft", 18),
            _ => ("Meta", "MetaLeft", 91),
        };
        return Ok(KeyPress { key: key.into(), code: code.into(), windows_virtual_key_code: vk, text: None, modifiers: mods });
    }
    if let Some((key, code, vk, text)) = named(key_part) {
        return Ok(KeyPress { key: key.into(), code: code.into(), windows_virtual_key_code: vk, text: text.filter(|_| texty).map(str::to_string), modifiers });
    }
    let lower = key_part.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|d| d.parse::<i32>().ok()).filter(|n| (1..=24).contains(n)) {
        return Ok(KeyPress { key: format!("F{n}"), code: format!("F{n}"), windows_virtual_key_code: 111 + n, text: None, modifiers });
    }
    let mut chars = key_part.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Err(format!("unknown key {key_part:?} (use a single character or a name like Enter, Tab, ArrowDown, F5)"));
    };
    let shifted = modifiers & MOD_SHIFT != 0;
    let (code, vk) = if c.is_ascii_alphabetic() {
        (format!("Key{}", c.to_ascii_uppercase()), c.to_ascii_uppercase() as i32)
    } else if c.is_ascii_digit() {
        (format!("Digit{c}"), c as i32)
    } else {
        let (code, vk) = match c {
            '-' | '_' => ("Minus", 189),
            '=' | '+' => ("Equal", 187),
            ',' | '<' => ("Comma", 188),
            '.' | '>' => ("Period", 190),
            '/' | '?' => ("Slash", 191),
            ';' | ':' => ("Semicolon", 186),
            '\'' | '"' => ("Quote", 222),
            '[' | '{' => ("BracketLeft", 219),
            ']' | '}' => ("BracketRight", 221),
            '\\' | '|' => ("Backslash", 220),
            '`' | '~' => ("Backquote", 192),
            _ => ("", 0),
        };
        (code.to_string(), vk)
    };
    let key = match (c.is_ascii_alphabetic(), shifted) {
        (true, true) => c.to_ascii_uppercase().to_string(),
        (true, false) => c.to_ascii_lowercase().to_string(),
        _ => c.to_string(),
    };
    let text = texty.then(|| key.clone());
    Ok(KeyPress { key, code, windows_virtual_key_code: vk, text, modifiers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys() {
        let enter = parse("Enter").unwrap();
        assert_eq!((enter.key.as_str(), enter.code.as_str(), enter.windows_virtual_key_code, enter.text.as_deref(), enter.modifiers), ("Enter", "Enter", 13, Some("\r"), 0));
        assert_eq!(parse("arrowdown").unwrap().windows_virtual_key_code, 40);
        assert_eq!(parse("F5").unwrap().windows_virtual_key_code, 116);
        assert_eq!(parse("Escape").unwrap().text, None);
    }

    #[test]
    fn combos() {
        let sel = parse("Control+A").unwrap();
        assert_eq!((sel.key.as_str(), sel.code.as_str(), sel.windows_virtual_key_code, sel.text, sel.modifiers), ("a", "KeyA", 65, None, MOD_CTRL));
        let back = parse("Shift+Tab").unwrap();
        assert_eq!((back.key.as_str(), back.modifiers), ("Tab", MOD_SHIFT));
        let upper = parse("Shift+a").unwrap();
        assert_eq!((upper.key.as_str(), upper.text.as_deref()), ("A", Some("A")));
        let plus = parse("Control++").unwrap();
        assert_eq!((plus.key.as_str(), plus.modifiers), ("+", MOD_CTRL));
        assert_eq!(parse("ctrl+shift+ArrowLeft").unwrap().modifiers, MOD_CTRL | MOD_SHIFT);
        assert_eq!(parse("Shift").unwrap().windows_virtual_key_code, 16);
        assert_eq!(parse("Alt+Enter").unwrap().text, None);
        assert_eq!(parse("가").unwrap().text.as_deref(), Some("가"));
    }

    #[test]
    fn errors() {
        assert!(parse("").is_err());
        assert!(parse("Hyper+A").is_err());
        assert!(parse("NotAKey").is_err());
    }
}
