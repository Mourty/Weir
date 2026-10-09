//! Key combinations, such as `Ctrl+Alt+M`: reading them as people write
//! them, and writing them the way the desktop and X11 want them.
//!
//! A combination is any of Ctrl, Alt, Shift and Super, and one key. Keys
//! that type something, or move the cursor, need Ctrl, Alt or Super with
//! them: taken on their own, a hotkey would stop them working everywhere
//! else.

use std::fmt;

/// One key of a combination: its name for people, its name for the
/// desktop (an xkb keysym name), and its X11 keysym.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// As people see it: `M`, `F5`, `Space`, `VolumeUp`.
    pub name: String,
    /// As the desktop's shortcut service wants it: `m`, `F5`, `space`,
    /// `XF86AudioRaiseVolume`.
    pub xdg: String,
    /// The X11 keysym.
    pub keysym: u32,
    /// Whether it may be a hotkey without Ctrl, Alt or Super.
    pub alone: bool,
}

/// Keys with names of their own: the name people see, the keysym name,
/// the keysym, whether it may be used alone, and other names people write.
const NAMED: &[(&str, &str, u32, bool, &[&str])] = &[
    ("Space", "space", 0x20, false, &[]),
    ("Tab", "Tab", 0xff09, false, &[]),
    ("Enter", "Return", 0xff0d, false, &["return"]),
    ("Backspace", "BackSpace", 0xff08, false, &[]),
    ("Escape", "Escape", 0xff1b, false, &["esc"]),
    ("Insert", "Insert", 0xff63, false, &["ins"]),
    ("Delete", "Delete", 0xffff, false, &["del"]),
    ("Home", "Home", 0xff50, false, &[]),
    ("End", "End", 0xff57, false, &[]),
    ("PageUp", "Prior", 0xff55, false, &["pgup", "prior"]),
    ("PageDown", "Next", 0xff56, false, &["pgdn", "pgdown"]),
    ("Up", "Up", 0xff52, false, &["arrowup"]),
    ("Down", "Down", 0xff54, false, &["arrowdown"]),
    ("Left", "Left", 0xff51, false, &["arrowleft"]),
    ("Right", "Right", 0xff53, false, &["arrowright"]),
    ("Pause", "Pause", 0xff13, true, &[]),
    ("Print", "Print", 0xff61, true, &["printscreen"]),
    ("ScrollLock", "Scroll_Lock", 0xff14, true, &[]),
    ("Minus", "minus", 0x2d, false, &["-"]),
    ("Equal", "equal", 0x3d, false, &["="]),
    ("BracketLeft", "bracketleft", 0x5b, false, &["["]),
    ("BracketRight", "bracketright", 0x5d, false, &["]"]),
    ("Semicolon", "semicolon", 0x3b, false, &[";"]),
    ("Apostrophe", "apostrophe", 0x27, false, &["'", "quote"]),
    ("Grave", "grave", 0x60, false, &["`", "backtick"]),
    ("Backslash", "backslash", 0x5c, false, &["\\"]),
    ("Comma", "comma", 0x2c, false, &[","]),
    ("Period", "period", 0x2e, false, &["."]),
    ("Slash", "slash", 0x2f, false, &["/"]),
    ("Num0", "KP_0", 0xffb0, false, &["kp0", "numpad0"]),
    ("Num1", "KP_1", 0xffb1, false, &["kp1", "numpad1"]),
    ("Num2", "KP_2", 0xffb2, false, &["kp2", "numpad2"]),
    ("Num3", "KP_3", 0xffb3, false, &["kp3", "numpad3"]),
    ("Num4", "KP_4", 0xffb4, false, &["kp4", "numpad4"]),
    ("Num5", "KP_5", 0xffb5, false, &["kp5", "numpad5"]),
    ("Num6", "KP_6", 0xffb6, false, &["kp6", "numpad6"]),
    ("Num7", "KP_7", 0xffb7, false, &["kp7", "numpad7"]),
    ("Num8", "KP_8", 0xffb8, false, &["kp8", "numpad8"]),
    ("Num9", "KP_9", 0xffb9, false, &["kp9", "numpad9"]),
    ("NumPlus", "KP_Add", 0xffab, false, &["kpadd", "numpadadd"]),
    (
        "NumMinus",
        "KP_Subtract",
        0xffad,
        false,
        &["kpsubtract", "numpadsubtract"],
    ),
    ("NumMultiply", "KP_Multiply", 0xffaa, false, &["kpmultiply"]),
    ("NumDivide", "KP_Divide", 0xffaf, false, &["kpdivide"]),
    ("NumEnter", "KP_Enter", 0xff8d, false, &["kpenter"]),
    ("NumPeriod", "KP_Decimal", 0xffae, false, &["kpdecimal"]),
    ("Mute", "XF86AudioMute", 0x1008_ff12, true, &["audiomute"]),
    (
        "VolumeDown",
        "XF86AudioLowerVolume",
        0x1008_ff11,
        true,
        &["audiolowervolume"],
    ),
    (
        "VolumeUp",
        "XF86AudioRaiseVolume",
        0x1008_ff13,
        true,
        &["audioraisevolume"],
    ),
    (
        "MicMute",
        "XF86AudioMicMute",
        0x1008_ffb2,
        true,
        &["audiomicmute"],
    ),
    ("Play", "XF86AudioPlay", 0x1008_ff14, true, &["audioplay"]),
    ("Stop", "XF86AudioStop", 0x1008_ff15, true, &["audiostop"]),
    (
        "PreviousTrack",
        "XF86AudioPrev",
        0x1008_ff16,
        true,
        &["audioprev"],
    ),
    (
        "NextTrack",
        "XF86AudioNext",
        0x1008_ff17,
        true,
        &["audionext"],
    ),
];

impl Key {
    /// The key called `name`, as people write it, ignoring case: `m`,
    /// `F5`, `space`, `PageUp`, `-`.
    pub fn from_name(name: &str) -> Option<Key> {
        let t = name.trim();
        let mut chars = t.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_alphabetic() {
                let lower = c.to_ascii_lowercase();
                return Some(Key {
                    name: c.to_ascii_uppercase().to_string(),
                    xdg: lower.to_string(),
                    keysym: lower as u32,
                    alone: false,
                });
            }
            if c.is_ascii_digit() {
                return Some(Key {
                    name: c.to_string(),
                    xdg: c.to_string(),
                    keysym: c as u32,
                    alone: false,
                });
            }
        }
        if let Some(n) = t
            .strip_prefix(['F', 'f'])
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|n| (1..=24).contains(n))
        {
            return Some(Key {
                name: format!("F{n}"),
                xdg: format!("F{n}"),
                keysym: 0xffbe + n - 1,
                alone: true,
            });
        }
        let folded = t.to_ascii_lowercase().replace([' ', '_'], "");
        NAMED
            .iter()
            .find(|(n, _, _, _, also)| {
                n.eq_ignore_ascii_case(&folded) || also.iter().any(|a| *a == folded || *a == t)
            })
            .map(|&(n, xdg, keysym, alone, _)| Key {
                name: n.into(),
                xdg: xdg.into(),
                keysym,
                alone,
            })
    }
}

/// A key and the modifiers held with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCombo {
    /// Ctrl held.
    pub ctrl: bool,
    /// Alt held.
    pub alt: bool,
    /// Shift held.
    pub shift: bool,
    /// Super (the Windows or Meta key) held.
    pub super_key: bool,
    /// The key itself.
    pub key: Key,
}

/// X11 modifier masks.
const X11_SHIFT: u16 = 1;
const X11_CONTROL: u16 = 4;
const X11_ALT: u16 = 8;
const X11_SUPER: u16 = 64;

impl KeyCombo {
    /// Read a combination as people write it: `Ctrl+Alt+M`, `ctrl + shift
    /// + f5`, `Super+Space`. The error says what is wrong, in a sentence.
    pub fn parse(text: &str) -> Result<KeyCombo, String> {
        let mut combo = (false, false, false, false);
        let mut key: Option<Key> = None;
        if text.trim().is_empty() {
            return Err("no keys given".into());
        }
        for part in text.split('+') {
            let p = part.trim();
            match p.to_ascii_lowercase().as_str() {
                "" => return Err(format!("'{text}' has an empty part")),
                "ctrl" | "control" => combo.0 = true,
                "alt" => combo.1 = true,
                "shift" => combo.2 = true,
                "super" | "meta" | "win" | "logo" | "windows" => combo.3 = true,
                _ => {
                    let k =
                        Key::from_name(p).ok_or_else(|| format!("there is no key called '{p}'"))?;
                    if let Some(first) = &key {
                        return Err(format!(
                            "'{text}' has two keys, {} and {}; a hotkey has one key, with Ctrl, \
                             Alt, Shift or Super",
                            first.name, k.name
                        ));
                    }
                    key = Some(k);
                }
            }
        }
        let key =
            key.ok_or_else(|| format!("'{text}' has no key, only Ctrl, Alt, Shift or Super"))?;
        let (ctrl, alt, shift, super_key) = combo;
        if !(ctrl || alt || super_key || key.alone) {
            return Err(format!(
                "{} on its own would stop it working anywhere else: hold Ctrl, Alt or Super \
                 with it",
                if shift {
                    format!("Shift+{}", key.name)
                } else {
                    key.name.clone()
                }
            ));
        }
        Ok(KeyCombo {
            ctrl,
            alt,
            shift,
            super_key,
            key,
        })
    }

    /// The combination as the desktop's shortcut service wants it suggested,
    /// such as `CTRL+ALT+m`.
    pub fn to_xdg(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.ctrl {
            parts.push("CTRL");
        }
        if self.alt {
            parts.push("ALT");
        }
        if self.shift {
            parts.push("SHIFT");
        }
        if self.super_key {
            parts.push("LOGO");
        }
        parts.push(&self.key.xdg);
        parts.join("+")
    }

    /// The X11 modifier mask the combination needs.
    pub fn x11_modifiers(&self) -> u16 {
        let mut m = 0;
        if self.shift {
            m |= X11_SHIFT;
        }
        if self.ctrl {
            m |= X11_CONTROL;
        }
        if self.alt {
            m |= X11_ALT;
        }
        if self.super_key {
            m |= X11_SUPER;
        }
        m
    }
}

/// `Ctrl+Alt+Shift+Super+Key`, the modifiers always in that order.
impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (held, name) in [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.super_key, "Super"),
        ] {
            if held {
                write!(f, "{name}+")?;
            }
        }
        write!(f, "{}", self.key.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinations_read_however_they_are_written() {
        let k = KeyCombo::parse(" ctrl + alt + m ").unwrap();
        assert_eq!(k.to_string(), "Ctrl+Alt+M");
        assert_eq!(k.to_xdg(), "CTRL+ALT+m");
        assert_eq!(k.key.keysym, 0x6d);
        assert_eq!(k.x11_modifiers(), 4 | 8);
        let k = KeyCombo::parse("Super+Shift+pgup").unwrap();
        assert_eq!(k.to_string(), "Shift+Super+PageUp");
        assert_eq!(k.to_xdg(), "SHIFT+LOGO+Prior");
        assert_eq!(
            KeyCombo::parse("ctrl+alt+-").unwrap().to_string(),
            "Ctrl+Alt+Minus"
        );
        assert_eq!(KeyCombo::parse("Ctrl+F12").unwrap().key.keysym, 0xffc9);
    }

    #[test]
    fn some_keys_may_be_used_alone() {
        assert_eq!(KeyCombo::parse("f13").unwrap().to_string(), "F13");
        assert_eq!(
            KeyCombo::parse("MicMute").unwrap().to_xdg(),
            "XF86AudioMicMute"
        );
        assert_eq!(KeyCombo::parse("Pause").unwrap().key.keysym, 0xff13);
    }

    #[test]
    fn mistakes_are_explained() {
        let e = KeyCombo::parse("M").unwrap_err();
        assert!(e.contains("Ctrl, Alt or Super"), "{e}");
        assert!(KeyCombo::parse("Shift+Space")
            .unwrap_err()
            .contains("Shift+Space"));
        assert!(KeyCombo::parse("Ctrl+Alt").unwrap_err().contains("no key"));
        assert!(KeyCombo::parse("Ctrl+A+B")
            .unwrap_err()
            .contains("two keys"));
        assert!(KeyCombo::parse("Ctrl+Banana")
            .unwrap_err()
            .contains("Banana"));
        assert!(KeyCombo::parse("Ctrl++A").unwrap_err().contains("empty"));
        assert!(KeyCombo::parse("").is_err());
    }
}
