//! Recording keys: the next key pressed in the window, with Ctrl, Alt or
//! Shift held, becomes a hotkey's keys.
//!
//! The window cannot see the Super key, and keys a hotkey already has
//! reach that hotkey rather than the window; both can be typed under More
//! options, or set in the desktop's shortcut settings.

use weir_protocol::KeyCombo;

/// What a key press means while recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// These keys, written the usual way.
    Keys(String),
    /// Escape: stop recording.
    Cancel,
    /// Keys that cannot be a hotkey, and why.
    Refused(String),
}

/// Weir's name for `key`, when it can be a hotkey's.
fn name(key: egui::Key) -> Option<String> {
    use egui::Key as K;
    let fixed = match key {
        K::ArrowUp => "Up",
        K::ArrowDown => "Down",
        K::ArrowLeft => "Left",
        K::ArrowRight => "Right",
        K::Tab => "Tab",
        K::Backspace => "Backspace",
        K::Enter => "Enter",
        K::Space => "Space",
        K::Insert => "Insert",
        K::Delete => "Delete",
        K::Home => "Home",
        K::End => "End",
        K::PageUp => "PageUp",
        K::PageDown => "PageDown",
        K::Comma => "Comma",
        K::Backslash => "Backslash",
        K::Slash => "Slash",
        K::OpenBracket => "BracketLeft",
        K::CloseBracket => "BracketRight",
        K::Backtick => "Grave",
        K::Minus => "Minus",
        K::Period => "Period",
        K::Equals => "Equal",
        K::Semicolon => "Semicolon",
        K::Quote => "Apostrophe",
        // Letters, digits and F keys are called what egui calls them.
        _ => {
            let n = key.name();
            let letter_or_digit = n.len() == 1 && n.chars().all(|c| c.is_ascii_alphanumeric());
            let f_key = n
                .strip_prefix('F')
                .and_then(|d| d.parse::<u32>().ok())
                .is_some_and(|d| (1..=24).contains(&d));
            return (letter_or_digit || f_key).then(|| n.to_string());
        }
    };
    Some(fixed.to_string())
}

/// What pressing `key` with `mods` means while recording.
pub fn read(key: egui::Key, mods: egui::Modifiers) -> Recorded {
    if key == egui::Key::Escape && !(mods.ctrl || mods.alt || mods.shift) {
        return Recorded::Cancel;
    }
    let Some(key) = name(key) else {
        return Recorded::Refused("Weir cannot use that key for a hotkey.".into());
    };
    let mut parts: Vec<&str> = Vec::new();
    if mods.ctrl {
        parts.push("Ctrl");
    }
    if mods.alt {
        parts.push("Alt");
    }
    if mods.shift {
        parts.push("Shift");
    }
    parts.push(&key);
    match KeyCombo::parse(&parts.join("+")) {
        Ok(combo) => Recorded::Keys(combo.to_string()),
        Err(why) => Recorded::Refused(capitalized(&why)),
    }
}

/// `text` as a sentence: its first letter capitalized, and a full stop.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

/// The first key pressed in this window since the last frame, taking every
/// key and text event so that nothing else in the window acts on them.
pub fn take(ctx: &egui::Context) -> Option<Recorded> {
    ctx.input_mut(|i| {
        let mut got = None;
        i.events.retain(|e| match e {
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } => {
                got.get_or_insert_with(|| read(*key, *modifiers));
                false
            }
            egui::Event::Key { .. } | egui::Event::Text(_) => false,
            _ => true,
        });
        got
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};

    const CTRL_ALT: Modifiers = Modifiers {
        alt: true,
        ctrl: true,
        shift: false,
        mac_cmd: false,
        command: true,
    };

    #[test]
    fn keys_pressed_become_combinations() {
        assert_eq!(read(Key::M, CTRL_ALT), Recorded::Keys("Ctrl+Alt+M".into()));
        assert_eq!(read(Key::F9, Modifiers::NONE), Recorded::Keys("F9".into()));
        assert_eq!(
            read(Key::ArrowDown, CTRL_ALT),
            Recorded::Keys("Ctrl+Alt+Down".into())
        );
        assert_eq!(
            read(Key::Num1, Modifiers::SHIFT | Modifiers::ALT),
            Recorded::Keys("Alt+Shift+1".into())
        );
        assert_eq!(read(Key::Escape, Modifiers::NONE), Recorded::Cancel);
        assert!(
            matches!(read(Key::M, Modifiers::NONE), Recorded::Refused(why) if why.ends_with('.'))
        );
        assert!(matches!(
            read(Key::F30, Modifiers::NONE),
            Recorded::Refused(_)
        ));
    }
}
