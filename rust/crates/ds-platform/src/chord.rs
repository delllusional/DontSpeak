//! Key-chord parsing for Claude Code's `voice:pushToTalk` keybinding grammar.
//!
//! Pure, OS-independent string parsing: a `"+"`-separated keybinding string
//! (`"ctrl+g"`, `"space"`, `"meta+k"`, …) becomes a [`KeyChord`] (modifier flags
//! + a [`KeyBase`]). Each [`crate::KeyInjector`] maps the base to its OS keycode.

/// The base (non-modifier) key of a [`KeyChord`]. Only the keys DontSpeak can reliably
/// synthesize as one discrete terminal keypress are modeled; anything else parses to
/// `Unsupported` so the caller WARNS + skips rather than emitting the wrong key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyBase {
    Space,
    Enter,
    Tab,
    Escape,
    /// A letter `a`–`z` (lowercased).
    Letter(char),
    /// A key we don't synthesize yet (function keys, arrows, punctuation, …) — kept as
    /// the original token for the warning/UI.
    Unsupported(String),
}

/// A keystroke to synthesize: a base key plus modifier flags, parsed from a Claude Code
/// keybinding string (e.g. `"ctrl+g"`, `"space"`, `"meta+k"`). Platform-agnostic — each
/// [`crate::KeyInjector`] maps the base to its OS keycode. The `claude_code` STT engine reads
/// Claude Code's `voice:pushToTalk` binding into one of these and taps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub cmd: bool,
    pub base: KeyBase,
}

impl Default for KeyChord {
    /// Claude Code's default `voice:pushToTalk` key — bare `Space`.
    fn default() -> Self {
        KeyChord {
            ctrl: false,
            shift: false,
            alt: false,
            cmd: false,
            base: KeyBase::Space,
        }
    }
}

impl KeyChord {
    /// Parse a Claude Code keybinding string (`"ctrl+g"`, `"space"`, `"meta+k"`, …). The
    /// LAST `+`-separated token is the base key; the rest are modifiers (matching Claude
    /// Code's grammar: `ctrl|control`, `shift`, `alt|opt|option|meta`, `cmd|command|super|win`).
    /// An unknown modifier or base yields `KeyBase::Unsupported` so callers can warn.
    pub fn parse(s: &str) -> Self {
        let parts: Vec<&str> = s
            .split('+')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        let mut chord = KeyChord {
            ctrl: false,
            shift: false,
            alt: false,
            cmd: false,
            base: KeyBase::Unsupported(s.to_string()),
        };
        let Some((base, mods)) = parts.split_last() else {
            return chord;
        };
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => chord.ctrl = true,
                "shift" => chord.shift = true,
                "alt" | "opt" | "option" | "meta" => chord.alt = true,
                "cmd" | "command" | "super" | "win" => chord.cmd = true,
                // Unknown modifier ⇒ give up (leave base Unsupported).
                _ => {
                    return KeyChord {
                        base: KeyBase::Unsupported(s.to_string()),
                        ..chord
                    };
                }
            }
        }
        // A standalone uppercase letter implies Shift (Claude Code's rule).
        chord.base = match base.to_ascii_lowercase().as_str() {
            "space" => KeyBase::Space,
            "enter" | "return" => KeyBase::Enter,
            "tab" => KeyBase::Tab,
            "escape" | "esc" => KeyBase::Escape,
            b if b.len() == 1 && b.as_bytes()[0].is_ascii_alphabetic() => {
                if base.len() == 1 && base.as_bytes()[0].is_ascii_uppercase() {
                    chord.shift = true;
                }
                KeyBase::Letter(b.as_bytes()[0] as char)
            }
            _ => KeyBase::Unsupported(s.to_string()),
        };
        chord
    }

    /// Whether this chord can actually be synthesized (a known base key).
    pub fn is_supported(&self) -> bool {
        !matches!(self.base, KeyBase::Unsupported(_))
    }

    /// Human label for logs/UI, e.g. `"Ctrl+G"`, `"Space"`.
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        if self.cmd {
            s.push_str("Cmd+");
        }
        match &self.base {
            KeyBase::Space => s.push_str("Space"),
            KeyBase::Enter => s.push_str("Enter"),
            KeyBase::Tab => s.push_str("Tab"),
            KeyBase::Escape => s.push_str("Esc"),
            KeyBase::Letter(c) => s.push(c.to_ascii_uppercase()),
            KeyBase::Unsupported(t) => s.push_str(t),
        }
        s
    }
}

/// The canonical set of [`KeyBase`] variants every platform's keycode map MUST
/// resolve to a real hardware key — the single source of truth the per-OS parity
/// tests (`vk_for_base` on Windows, `mac_keycode` on macOS, `key_for_base` on Linux)
/// check against. Adding a synthesizable key here makes every platform's test fail
/// until that OS maps it, so a new key can't silently fall through to `Unsupported`
/// on one platform. `Unsupported` is excluded by design — it has no keycode anywhere.
#[cfg(test)]
pub(crate) fn all_supported_bases() -> Vec<KeyBase> {
    let mut v = vec![
        KeyBase::Space,
        KeyBase::Enter,
        KeyBase::Tab,
        KeyBase::Escape,
    ];
    v.extend(('a'..='z').map(KeyBase::Letter));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_supported_bases_is_the_full_set() {
        // 4 named keys + 26 letters, none of them Unsupported.
        let bases = all_supported_bases();
        assert_eq!(bases.len(), 30);
        assert!(!bases.iter().any(|b| matches!(b, KeyBase::Unsupported(_))));
    }

    #[test]
    fn parse_plain_key() {
        let c = KeyChord::parse("space");
        assert_eq!(c.base, KeyBase::Space);
        assert!(!c.ctrl && !c.shift && !c.alt && !c.cmd);
    }

    #[test]
    fn parse_modified_chord_ctrl_letter() {
        let c = KeyChord::parse("ctrl+g");
        assert!(c.ctrl);
        assert!(!c.shift && !c.alt && !c.cmd);
        assert_eq!(c.base, KeyBase::Letter('g'));
    }

    #[test]
    fn parse_multi_modifier_chord_cmd_shift_letter() {
        // `meta` ⇒ alt, `cmd` ⇒ cmd; the base letter is lowercased.
        let c = KeyChord::parse("cmd+shift+x");
        assert!(c.cmd && c.shift);
        assert!(!c.ctrl && !c.alt);
        assert_eq!(c.base, KeyBase::Letter('x'));
    }

    #[test]
    fn standalone_uppercase_letter_implies_shift() {
        let c = KeyChord::parse("G");
        assert!(c.shift);
        assert_eq!(c.base, KeyBase::Letter('g'));
    }

    #[test]
    fn label_round_trips() {
        assert_eq!(KeyChord::parse("ctrl+g").label(), "Ctrl+G");
        assert_eq!(KeyChord::parse("space").label(), "Space");
        assert_eq!(KeyChord::parse("cmd+shift+x").label(), "Shift+Cmd+X");
        assert_eq!(KeyChord::parse("enter").label(), "Enter");
        assert_eq!(KeyChord::parse("esc").label(), "Esc");
    }

    #[test]
    fn is_supported_for_supported_chord() {
        assert!(KeyChord::parse("ctrl+g").is_supported());
        assert!(KeyChord::parse("space").is_supported());
    }

    #[test]
    fn is_supported_false_for_unsupported_chord() {
        // An unknown modifier yields Unsupported.
        assert!(!KeyChord::parse("hyper+g").is_supported());
        // A multi-char / non-modeled base yields Unsupported.
        assert!(!KeyChord::parse("f1").is_supported());
    }

    #[test]
    fn parse_empty_string_is_unsupported_not_default() {
        // An empty string has no base token ⇒ Unsupported (NOT the Space default).
        let c = KeyChord::parse("");
        assert!(!c.is_supported());
        assert_eq!(c.base, KeyBase::Unsupported(String::new()));
    }

    #[test]
    fn default_is_bare_space() {
        let d = KeyChord::default();
        assert_eq!(d.base, KeyBase::Space);
        assert!(!d.ctrl && !d.shift && !d.alt && !d.cmd);
    }
}
