//! How a bind reads: its combo in title case (`Super+Shift+A`), a function humanised from a fixed
//! table when no comment describes it (`spawn,firefox` → `Run firefox`; an unknown function shows
//! raw; Mango's and Sway's tables), and the group headers of the comments (`# ── Name ──`).

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
        Dialect::Sway => sway(function, args),
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

/// Sway's table, over the action's first word and the rest.
fn sway(function: &str, args: &str) -> Option<String> {
    let after = |prefix: &str| args.strip_prefix(prefix).map(str::trim_start);
    let phrase = match function {
        "exec" | "exec_always" if !args.is_empty() => format!("Run {}", after("--no-startup-id ").unwrap_or(args)),
        "workspace" if !args.is_empty() => format!("Go to workspace {}", after("number ").unwrap_or(args)),
        "move" => {
            let to = ["container to workspace ", "window to workspace ", "to workspace "].iter().find_map(|p| after(p));
            match to {
                Some(ws) => format!("Move window to workspace {}", ws.strip_prefix("number ").unwrap_or(ws).trim_start()),
                None if args == "scratchpad" => "Move window to the scratchpad".to_string(),
                None if !args.is_empty() && !args.contains(char::is_whitespace) => format!("Move window {args}"),
                None => return None,
            }
        }
        "scratchpad" if args == "show" => "Show the scratchpad".to_string(),
        "kill" if args.is_empty() => "Close window".to_string(),
        "focus" if !args.is_empty() => format!("Focus {args}"),
        "fullscreen" if args.is_empty() || args == "toggle" => "Toggle fullscreen".to_string(),
        "floating" if args == "toggle" => "Toggle floating".to_string(),
        "sticky" if args == "toggle" => "Toggle sticky".to_string(),
        "layout" if !args.is_empty() => format!("Set layout {args}"),
        "splith" if args.is_empty() => "Split horizontally".to_string(),
        "splitv" if args.is_empty() => "Split vertically".to_string(),
        "resize" if !args.is_empty() => format!("Resize {args}"),
        "reload" if args.is_empty() => "Reload configuration".to_string(),
        "exit" if args.is_empty() => "Quit Sway".to_string(),
        "mode" if !args.is_empty() => format!("Enter mode {}", args.trim_matches('"')),
        _ => return None,
    };
    Some(phrase)
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
