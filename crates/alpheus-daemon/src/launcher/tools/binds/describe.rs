//! How a bind reads: its combo in title case (`Super+Shift+A`), a function humanised from a fixed
//! table when no comment describes it (`spawn,firefox` → `Run firefox`; an unknown function shows
//! raw), and the group headers of the comments (`# ── Name ──`).

use super::Dialect;

/// The combo of `mods` (joined by `+`) and `key`: modifiers named the usual way (`SUPER`, `Mod4`
/// → `Super`, `Mod1` → `Alt`, `CTRL`, `Control` → `Ctrl`), `NONE` dropped, others title-cased; a
/// one-letter key upper-cased, a longer one kept as written (`Tab`, `XF86AudioMute`, `code:38`).
pub fn combo(_dialect: Dialect, mods: &str, key: &str) -> String {
    let mut out = String::new();
    for m in mods.split('+').map(str::trim).filter(|m| !m.is_empty()) {
        let name = match m.to_ascii_lowercase().as_str() {
            "none" => continue,
            "super" | "logo" | "mod4" | "win" => "Super".to_string(),
            "shift" => "Shift".to_string(),
            "ctrl" | "control" => "Ctrl".to_string(),
            "alt" | "mod1" => "Alt".to_string(),
            _ => title(m),
        };
        out.push_str(&name);
        out.push('+');
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => out.extend(c.to_uppercase()),
        _ => out.push_str(key),
    }
    out
}

/// `word` with its first letter upper-cased and the rest lower-cased.
fn title(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

/// What a bind does, in words, from its function and arguments; an unknown function reads
/// `function args`.
pub fn humanise(dialect: Dialect, function: &str, args: &str) -> String {
    let phrase = match dialect {
        Dialect::Mango => mango(function, args),
        Dialect::Sway => None,
    };
    phrase.unwrap_or_else(|| if args.is_empty() { function.to_string() } else { format!("{function} {args}") })
}

/// Mango's table. A phrase ending in a blank takes the arguments.
fn mango(function: &str, args: &str) -> Option<String> {
    let phrase = match function {
        "spawn" | "spawn_shell" => "Run ",
        "view" => "Go to desktop ",
        "tag" => "Move window to desktop ",
        "tagsilent" => "Send window to desktop ",
        "toggleview" => "Toggle desktop ",
        "focusdir" => "Focus ",
        "focusstack" => "Focus window ",
        "focusmon" => "Focus monitor ",
        "tagmon" => "Move window to monitor ",
        "exchange_client" => "Swap window ",
        "setlayout" => "Set layout ",
        "resizewin" => "Resize window",
        "movewin" => "Move window",
        "killclient" => "Close window",
        "togglefullscreen" => "Toggle fullscreen",
        "togglemaximizescreen" => "Toggle maximized",
        "togglefloating" => "Toggle floating",
        "toggleoverview" => "Toggle overview",
        "togglegaps" => "Toggle gaps",
        "toggle_scratchpad" => "Toggle scratchpad",
        "minimized" => "Minimize window",
        "restore_minimized" => "Restore minimized window",
        "switch_layout" => "Switch layout",
        "viewtoleft" => "Go to the desktop on the left",
        "viewtoright" => "Go to the desktop on the right",
        "tagtoleft" => "Move window to the desktop on the left",
        "tagtoright" => "Move window to the desktop on the right",
        "reload_config" => "Reload configuration",
        "quit" => "Quit Mango",
        _ => return None,
    };
    Some(match phrase.strip_suffix(' ') {
        Some(head) if args.is_empty() => head.to_string(),
        Some(_) => format!("{phrase}{args}"),
        None => phrase.to_string(),
    })
}

/// A comment's text read as a rule: None when it is not one; `Some(None)` for a bare rule
/// (`──────`); `Some(Some(name))` for a group header, `── Name ──` (a run of at least two of
/// `─━═-=` on each side).
pub(crate) fn rule_name(comment: &str) -> Option<Option<&str>> {
    let is_rule = |c: char| matches!(c, '─' | '━' | '═' | '-' | '=');
    let inner = comment.trim_start_matches(is_rule);
    if comment.chars().take_while(|c| is_rule(*c)).count() < 2 {
        return None;
    }
    if inner.trim().is_empty() {
        return Some(None);
    }
    let name = inner.trim_end_matches(is_rule);
    if inner.chars().rev().take_while(|c| is_rule(*c)).count() < 2 {
        return None;
    }
    Some(Some(name.trim()).filter(|n| !n.is_empty()))
}
