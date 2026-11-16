//! Keys the pad understands, from what xkbcommon already resolved (a keysym, layout applied, and its
//! text). Pure: tests feed keysyms directly. The held-key repeat is the caller's business.

use crate::keys::Mods;
use xkeysym::{Keysym, key};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    DocStart,
    DocEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKey {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Delete,
    WordBackspace,
    WordDelete,
    Move { to: Motion, extend: bool },
    SelectAll,
    Copy,
    Cut,
    Paste,
    Escape,
}

/// keysym + text → pad key. Ctrl+a/c/x/v by the Latin keysym (as `keys::translate` does for the
/// launcher: Ctrl+letter follows the Latin letter whatever the layout); Ctrl+←/→ word moves,
/// Ctrl+Home/End the whole text, Ctrl+Backspace/Delete word deletes; Shift on a move extends the
/// selection; keypad keys like their main keys; Return and KP_Enter are Enter, Shift or not;
/// printable text without Ctrl, Alt or Logo is a character; anything else `None`.
pub fn translate(sym: Keysym, utf8: Option<&str>, mods: Mods) -> Option<NoteKey> {
    if mods.alt || mods.logo {
        return None;
    }
    let raw = sym.raw();
    let extend = mods.shift;
    let mv = |to| Some(NoteKey::Move { to, extend });
    if mods.ctrl {
        return match raw {
            key::a | key::A => Some(NoteKey::SelectAll),
            key::c | key::C => Some(NoteKey::Copy),
            key::x | key::X => Some(NoteKey::Cut),
            key::v | key::V => Some(NoteKey::Paste),
            key::Left | key::KP_Left => mv(Motion::WordLeft),
            key::Right | key::KP_Right => mv(Motion::WordRight),
            key::Home | key::KP_Home => mv(Motion::DocStart),
            key::End | key::KP_End => mv(Motion::DocEnd),
            key::BackSpace => Some(NoteKey::WordBackspace),
            key::Delete | key::KP_Delete => Some(NoteKey::WordDelete),
            _ => None,
        };
    }
    match raw {
        key::Return | key::KP_Enter => Some(NoteKey::Enter),
        key::Tab if !mods.shift => Some(NoteKey::Tab),
        key::BackSpace => Some(NoteKey::Backspace),
        key::Delete | key::KP_Delete => Some(NoteKey::Delete),
        key::Escape => Some(NoteKey::Escape),
        key::Left | key::KP_Left => mv(Motion::Left),
        key::Right | key::KP_Right => mv(Motion::Right),
        key::Up | key::KP_Up => mv(Motion::Up),
        key::Down | key::KP_Down => mv(Motion::Down),
        key::Home | key::KP_Home => mv(Motion::Home),
        key::End | key::KP_End => mv(Motion::End),
        key::Page_Up | key::KP_Page_Up => mv(Motion::PageUp),
        key::Page_Down | key::KP_Page_Down => mv(Motion::PageDown),
        _ => {
            let mut chars = utf8?.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if !c.is_control() => Some(NoteKey::Char(c)),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, shift: false, alt: false, logo: false };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };
    const SHIFT: Mods = Mods { shift: true, ..NONE };
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, ..NONE };
    const ALT: Mods = Mods { alt: true, ..NONE };

    fn sym(raw: u32) -> Keysym {
        Keysym::new(raw)
    }

    #[test]
    fn ctrl_shortcuts_by_latin_keysym() {
        assert_eq!(translate(Keysym::from_char('a'), Some("\u{1}"), CTRL), Some(NoteKey::SelectAll));
        assert_eq!(translate(Keysym::from_char('C'), None, CTRL_SHIFT), Some(NoteKey::Copy));
        assert_eq!(translate(Keysym::from_char('x'), None, CTRL), Some(NoteKey::Cut));
        assert_eq!(translate(Keysym::from_char('v'), None, CTRL), Some(NoteKey::Paste));
        assert_eq!(translate(Keysym::from_char('z'), Some("\u{1a}"), CTRL), None);
    }

    #[test]
    fn moves_extend_with_shift() {
        assert_eq!(translate(sym(key::Left), None, SHIFT), Some(NoteKey::Move { to: Motion::Left, extend: true }));
        assert_eq!(translate(sym(key::Right), None, CTRL_SHIFT), Some(NoteKey::Move { to: Motion::WordRight, extend: true }));
        assert_eq!(translate(sym(key::End), None, CTRL), Some(NoteKey::Move { to: Motion::DocEnd, extend: false }));
        assert_eq!(translate(sym(key::KP_Up), None, NONE), Some(NoteKey::Move { to: Motion::Up, extend: false }));
        assert_eq!(translate(sym(key::Page_Down), None, SHIFT), Some(NoteKey::Move { to: Motion::PageDown, extend: true }));
        assert_eq!(translate(sym(key::Home), None, NONE), Some(NoteKey::Move { to: Motion::Home, extend: false }));
    }

    #[test]
    fn text_and_edits() {
        assert_eq!(translate(Keysym::from_char('é'), Some("é"), NONE), Some(NoteKey::Char('é')));
        assert_eq!(translate(Keysym::from_char('A'), Some("A"), SHIFT), Some(NoteKey::Char('A')));
        assert_eq!(translate(sym(key::Return), Some("\r"), NONE), Some(NoteKey::Enter));
        assert_eq!(translate(sym(key::Return), None, SHIFT), Some(NoteKey::Enter));
        assert_eq!(translate(sym(key::KP_Enter), None, NONE), Some(NoteKey::Enter));
        assert_eq!(translate(sym(key::Tab), Some("\t"), NONE), Some(NoteKey::Tab));
        assert_eq!(translate(sym(key::BackSpace), None, NONE), Some(NoteKey::Backspace));
        assert_eq!(translate(sym(key::BackSpace), None, CTRL), Some(NoteKey::WordBackspace));
        assert_eq!(translate(sym(key::Delete), None, NONE), Some(NoteKey::Delete));
        assert_eq!(translate(sym(key::Delete), None, CTRL), Some(NoteKey::WordDelete));
        assert_eq!(translate(sym(key::Escape), None, NONE), Some(NoteKey::Escape));
        assert_eq!(translate(Keysym::from_char('a'), Some("a"), ALT), None);
        assert_eq!(translate(Keysym::from_char('a'), Some("a"), Mods { logo: true, ..NONE }), None);
        assert_eq!(translate(sym(key::F5), None, NONE), None);
    }
}
