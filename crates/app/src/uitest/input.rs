//! Keyboard chords as people write them ("Cmd+Shift+N", "Esc", "Alt+[")
//! turned into the modifiers and keys egui-winit would deliver on this
//! platform.

use eframe::egui::{Key, Modifiers};

/// "Cmd" is the platform's command key: ⌘ on macOS, Ctrl elsewhere,
/// filled in the way egui-winit fills `Modifiers`.
pub(crate) fn command() -> Modifiers {
    if cfg!(target_os = "macos") {
        Modifiers {
            mac_cmd: true,
            command: true,
            ..Modifiers::NONE
        }
    } else {
        Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        }
    }
}

fn ctrl() -> Modifiers {
    if cfg!(target_os = "macos") {
        Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        }
    } else {
        command()
    }
}

/// Parse "Cmd+Shift+N" into modifiers and a key.
pub(crate) fn parse_chord(chord: &str) -> Result<(Modifiers, Key), String> {
    let parts: Vec<&str> = if chord.trim() == "+" {
        vec!["+"]
    } else if let Some(head) = chord.strip_suffix("++") {
        head.split('+').filter(|p| !p.is_empty()).chain(["+"]).collect()
    } else {
        chord.split('+').collect()
    };
    let (key, mods) = parts.split_last().ok_or("an empty chord")?;
    let mut m = Modifiers::NONE;
    for part in mods {
        let add = match part.trim().to_ascii_lowercase().as_str() {
            "cmd" | "command" | "meta" | "super" | "⌘" => command(),
            "ctrl" | "control" | "⌃" => ctrl(),
            "alt" | "option" | "opt" | "⌥" => Modifiers::ALT,
            "shift" | "⇧" => Modifiers::SHIFT,
            other => return Err(format!("unknown modifier {other:?} in {chord:?}")),
        };
        m = m.plus(add);
    }
    let key = key_named(key.trim()).ok_or_else(|| format!("unknown key {key:?} in {chord:?}"))?;
    Ok((m, key))
}

/// A key by the name people write: egui's names ("ArrowLeft", "F5"),
/// a single character ("a", "=", "["), or a common alias ("Esc").
pub(crate) fn key_named(name: &str) -> Option<Key> {
    let alias = match name.to_ascii_lowercase().as_str() {
        "esc" => Some(Key::Escape),
        "return" => Some(Key::Enter),
        "del" => Some(Key::Delete),
        "left" => Some(Key::ArrowLeft),
        "right" => Some(Key::ArrowRight),
        "up" => Some(Key::ArrowUp),
        "down" => Some(Key::ArrowDown),
        "pgup" => Some(Key::PageUp),
        "pgdn" => Some(Key::PageDown),
        _ => None,
    };
    if alias.is_some() {
        return alias;
    }
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return key_for_char(c);
    }
    Key::from_name(name).or_else(|| {
        // Case-insensitive egui names ("enter", "pageup").
        Key::ALL
            .iter()
            .copied()
            .find(|k| k.name().eq_ignore_ascii_case(name))
    })
}

/// The key a character is typed with (letters ignore case).
pub(crate) fn key_for_char(c: char) -> Option<Key> {
    Some(match c {
        ' ' => Key::Space,
        '=' => Key::Equals,
        '+' => Key::Plus,
        '-' => Key::Minus,
        '[' => Key::OpenBracket,
        ']' => Key::CloseBracket,
        ';' => Key::Semicolon,
        '\'' => Key::Quote,
        ',' => Key::Comma,
        '.' => Key::Period,
        '/' => Key::Slash,
        '\\' => Key::Backslash,
        '`' => Key::Backtick,
        ':' => Key::Colon,
        '|' => Key::Pipe,
        '?' => Key::Questionmark,
        '\n' => Key::Enter,
        '\t' => Key::Tab,
        c if c.is_ascii_digit() => Key::from_name(&c.to_string())?,
        c if c.is_ascii_alphabetic() => Key::from_name(&c.to_ascii_uppercase().to_string())?,
        _ => return None,
    })
}

/// A key's own character as typed without modifiers other than Shift,
/// which winit reports alongside the key press.
pub(crate) fn typed_text(key: Key, m: Modifiers) -> Option<String> {
    if m.command || m.ctrl || m.mac_cmd {
        return None;
    }
    let s = match key {
        Key::Space => " ".to_string(),
        k => {
            let sym = k.symbol_or_name();
            let mut chars = sym.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_graphic() => {
                    if m.shift {
                        c.to_ascii_uppercase().to_string()
                    } else {
                        c.to_ascii_lowercase().to_string()
                    }
                }
                _ => return None,
            }
        }
    };
    Some(s)
}

/// How a chord reads in a caption: "⇧⌘N" on macOS, "Ctrl+Shift+N" elsewhere.
pub(crate) fn chord_text(m: Modifiers, key: Key) -> String {
    let k = key.symbol_or_name();
    if cfg!(target_os = "macos") {
        let mut s = String::new();
        for (on, sym) in [
            (m.ctrl, "⌃"),
            (m.alt, "⌥"),
            (m.shift, "⇧"),
            (m.mac_cmd || m.command, "⌘"),
        ] {
            if on {
                s.push_str(sym);
            }
        }
        s + k
    } else {
        let mut parts: Vec<&str> = Vec::new();
        for (on, name) in [(m.ctrl || m.command, "Ctrl"), (m.alt, "Alt"), (m.shift, "Shift")] {
            if on {
                parts.push(name);
            }
        }
        parts.push(k);
        parts.join("+")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_parse_with_platform_modifiers() {
        let (m, k) = parse_chord("Cmd+Shift+N").unwrap();
        assert_eq!(k, Key::N);
        assert!(m.command && m.shift && !m.alt);
        assert_eq!(m.mac_cmd, cfg!(target_os = "macos"));
        assert_eq!(parse_chord("Esc").unwrap().1, Key::Escape);
        assert_eq!(parse_chord("Alt+[").unwrap(), (Modifiers::ALT, Key::OpenBracket));
        assert_eq!(parse_chord("Cmd++").unwrap().1, Key::Plus);
        assert_eq!(parse_chord("enter").unwrap().1, Key::Enter);
        assert!(parse_chord("Hyper+X").is_err());
        assert_eq!(key_for_char('7'), Some(Key::Num7));
        assert_eq!(typed_text(Key::A, Modifiers::SHIFT).as_deref(), Some("A"));
        assert_eq!(typed_text(Key::A, command()), None);
        let shown = chord_text(command().plus(Modifiers::SHIFT), Key::N);
        assert_eq!(
            shown,
            if cfg!(target_os = "macos") {
                "⇧⌘N"
            } else {
                "Ctrl+Shift+N"
            }
        );
    }
}
