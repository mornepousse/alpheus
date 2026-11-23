//! The keys the daemon's keyboard surfaces understand.
//!
//! The launcher's box, the notepad, a tray menu, the calendar and every panel read their keys from
//! here, as xkbcommon already resolved them: a keysym with the layout applied, so Dvorak's letters
//! are Dvorak's, and the text the key produces. `Key` is the closed set of what any of them can
//! answer to, which is why a new key is added here once and not in five places. Pure: no Wayland,
//! and the tests feed keysyms directly.
//!
//! Never:
//! - read a scancode or a raw keycode: the layout the user chose must decide what a key means;
//! - decide what a key does: that belongs to each box's own model (`launcher::model`,
//!   `notes::keys`, the popups), which is what lets the same key mean different things;
//! - repeat a key that must act once: the caller tells a repeat from a press
//!   (`the_keyboards_own_repeat_is_told_from_a_press`).
//!
//! See: docs/features/launcher.md, docs/features/notes.md and docs/features/quick-settings.md (the
//! keys a reader sees). The control center has its own keymap, with the Dvorak navigation keys of
//! `config.toml` (docs/configuration.md, `[keys]`).

use xkeysym::{Keysym, key};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Backspace,
    CtrlU,
    CtrlW,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    CtrlN,
    CtrlP,
    Enter,
    CtrlEnter,
    Escape,
    Tab,
    BackTab,
    /// The launcher ignores them (model.rs); a tray menu enters a submenu (→) or goes back a level (←).
    Left,
    Right,
    /// Removes the selected entry in the notifications panel; the launcher ignores it.
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub logo: bool,
}

/// xkb keysym (already layout-resolved by xkbcommon) + its text → Key.
/// Ctrl+n/p/u/w/Enter by keysym; Shift+Tab or ISO_Left_Tab → BackTab;
/// KP_Enter = Enter; printable text without Ctrl/Alt/Logo → Char (control
/// characters never); anything else → None.
///
/// The Ctrl shortcuts match the *Latin* keysyms (`key::n`/`key::N`, …) deliberately, the same
/// way the Dvorak case below does: Ctrl+letter is conventionally bound to the Latin letter's
/// keysym on Linux regardless of the active layout (xkbcommon's own "Ctrl falls back to the
/// ASCII keysym" behaviour). A layout that does not follow this convention would miss these
/// shortcuts; nothing here can detect that without more context than a pure function has.
pub fn translate(sym: Keysym, utf8: Option<&str>, mods: Mods) -> Option<Key> {
    let raw = sym.raw();
    if mods.ctrl && !mods.alt && !mods.logo {
        let k = match raw {
            key::n | key::N => Some(Key::CtrlN),
            key::p | key::P => Some(Key::CtrlP),
            key::u | key::U => Some(Key::CtrlU),
            key::w | key::W => Some(Key::CtrlW),
            key::Return | key::KP_Enter => Some(Key::CtrlEnter),
            _ => None,
        };
        if k.is_some() {
            return k;
        }
    }
    let k = match raw {
        key::Return | key::KP_Enter => Some(Key::Enter),
        key::BackSpace => Some(Key::Backspace),
        key::Escape => Some(Key::Escape),
        key::ISO_Left_Tab => Some(Key::BackTab),
        key::Tab if mods.shift => Some(Key::BackTab),
        key::Tab => Some(Key::Tab),
        key::Up | key::KP_Up => Some(Key::Up),
        key::Down | key::KP_Down => Some(Key::Down),
        key::Page_Up | key::KP_Page_Up => Some(Key::PageUp),
        key::Page_Down | key::KP_Page_Down => Some(Key::PageDown),
        key::Home | key::KP_Home => Some(Key::Home),
        key::End | key::KP_End => Some(Key::End),
        key::Left | key::KP_Left => Some(Key::Left),
        key::Right | key::KP_Right => Some(Key::Right),
        key::Delete | key::KP_Delete => Some(Key::Delete),
        _ => None,
    };
    if k.is_some() {
        return k;
    }
    if mods.ctrl || mods.alt || mods.logo {
        return None;
    }
    let mut chars = utf8?.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => Some(Key::Char(c)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, shift: false, alt: false, logo: false };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };
    const SHIFT: Mods = Mods { shift: true, ..NONE };

    fn sym(raw: u32) -> Keysym {
        Keysym::new(raw)
    }

    #[test]
    fn navigation_and_editing_keys() {
        let cases = [
            (key::Return, Key::Enter),
            (key::KP_Enter, Key::Enter),
            (key::BackSpace, Key::Backspace),
            (key::Escape, Key::Escape),
            (key::Tab, Key::Tab),
            (key::ISO_Left_Tab, Key::BackTab),
            (key::Up, Key::Up),
            (key::KP_Down, Key::Down),
            (key::Page_Up, Key::PageUp),
            (key::Page_Down, Key::PageDown),
            (key::Home, Key::Home),
            (key::End, Key::End),
            (key::Left, Key::Left),
            (key::Right, Key::Right),
            (key::Delete, Key::Delete),
            (key::KP_Delete, Key::Delete),
        ];
        for (raw, k) in cases {
            assert_eq!(translate(sym(raw), None, NONE), Some(k), "{:?}", sym(raw));
        }
        assert_eq!(translate(sym(key::Tab), None, SHIFT), Some(Key::BackTab), "Shift+Tab");
        assert_eq!(translate(sym(key::Return), Some("\r"), NONE), Some(Key::Enter), "its text \\r is not a character");
    }

    #[test]
    fn ctrl_shortcuts_follow_the_keysym_whatever_the_layout() {
        // Dvorak: xkbcommon already gives the keysym printed on the key (the
        // physical key differs from QWERTY's, the keysym does not), and Ctrl
        // makes the text a control character: only the keysym counts.
        for (c, k) in [('n', Key::CtrlN), ('p', Key::CtrlP), ('u', Key::CtrlU), ('w', Key::CtrlW)] {
            let control = char::from_u32(c as u32 - 0x60).unwrap().to_string();
            assert_eq!(translate(Keysym::from_char(c), Some(&control), CTRL), Some(k), "Ctrl+{c}");
            assert_eq!(translate(Keysym::from_char(c.to_ascii_uppercase()), None, Mods { shift: true, ..CTRL }), Some(k), "Ctrl+Shift+{c}");
        }
        assert_eq!(translate(sym(key::Return), None, CTRL), Some(Key::CtrlEnter));
        assert_eq!(translate(sym(key::KP_Enter), None, CTRL), Some(Key::CtrlEnter));
        assert_eq!(translate(Keysym::from_char('x'), Some("\u{18}"), CTRL), None, "an unbound Ctrl+key does nothing");
        assert_eq!(translate(sym(key::space), Some(" "), CTRL), None, "Ctrl+Space: unbound (the preview toggle it drove is gone)");
        assert_eq!(translate(sym(key::Down), None, CTRL), Some(Key::Down), "Ctrl does not block navigation");
    }

    #[test]
    fn text_becomes_characters_never_control_ones() {
        assert_eq!(translate(Keysym::from_char('a'), Some("a"), NONE), Some(Key::Char('a')));
        assert_eq!(translate(Keysym::from_char('A'), Some("A"), SHIFT), Some(Key::Char('A')), "Shift is text");
        assert_eq!(translate(Keysym::from_char('é'), Some("é"), NONE), Some(Key::Char('é')));
        assert_eq!(translate(Keysym::from_char('>'), Some(">"), SHIFT), Some(Key::Char('>')));
        assert_eq!(translate(sym(key::space), Some(" "), NONE), Some(Key::Char(' ')));
        assert_eq!(translate(sym(0x31), Some("1"), NONE), Some(Key::Char('1')));
        assert_eq!(translate(sym(0xffb1), Some("1"), NONE), Some(Key::Char('1')), "KP_1 with NumLock: its text");
        assert_eq!(translate(sym(0xffff), Some("\u{7f}"), NONE), Some(Key::Delete), "Delete: its text \\x7f is not a character");
        assert_eq!(translate(Keysym::from_char('a'), Some("\u{7f}"), NONE), None, "\\x7f: a control character");
        assert_eq!(translate(Keysym::from_char('a'), Some("\u{1b}"), NONE), None);
        assert_eq!(translate(Keysym::from_char('a'), None, NONE), None, "no text: nothing");
        assert_eq!(translate(Keysym::from_char('a'), Some("ab"), NONE), None, "several characters at once: ignored");
        assert_eq!(translate(sym(0xffe1), None, SHIFT), None, "a modifier alone");
    }

    #[test]
    fn alt_and_logo_never_type() {
        assert_eq!(translate(Keysym::from_char('a'), Some("a"), Mods { alt: true, ..NONE }), None);
        assert_eq!(translate(Keysym::from_char('a'), Some("a"), Mods { logo: true, ..NONE }), None);
        assert_eq!(translate(Keysym::from_char('n'), Some("n"), Mods { ctrl: true, alt: true, ..NONE }), None, "Ctrl+Alt+n is not Ctrl+n");
    }
}
