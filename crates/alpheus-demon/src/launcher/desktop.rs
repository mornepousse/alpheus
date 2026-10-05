//! `.desktop` files (XDG Desktop Entry Specification 1.5), the parts the
//! launcher uses: the `[Desktop Entry]` group and its `[Desktop Action x]`
//! groups, value escapes (`\s \n \t \r \\`, `\;` in lists), localized keys
//! (`lang_COUNTRY@MODIFIER` lookup order), `Hidden`, `NoDisplay`,
//! `OnlyShowIn`/`NotShowIn`, `TryExec`, and the `Exec` quoting rules and
//! field codes. Hand-written (≈ 400 lines) rather than the
//! `freedesktop-desktop-entry` crate: see the plan's dependency decisions.
//!
//! Lenient where real files are sloppy (unknown escapes kept, booleans
//! "true"/"1", desktop names compared case-insensitively), strict where a
//! mistake would launch the wrong thing (Exec quoting, unknown field codes).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locale {
    pub lang: String,
    pub country: Option<String>,
    pub modifier: Option<String>,
}

impl Locale {
    /// "fr_FR.UTF-8@euro" → fr, FR, euro (encoding dropped); "C"/"POSIX"/"" → None.
    pub fn parse(s: &str) -> Option<Locale> {
        let (rest, modifier) = match s.split_once('@') {
            Some((r, m)) => (r, Some(m)),
            None => (s, None),
        };
        let rest = rest.split_once('.').map_or(rest, |(r, _)| r);
        let (lang, country) = match rest.split_once('_') {
            Some((l, c)) => (l, Some(c)),
            None => (rest, None),
        };
        if lang.is_empty() || lang == "C" || lang == "POSIX" {
            return None;
        }
        let part = |x: Option<&str>| x.filter(|v| !v.is_empty()).map(str::to_string);
        Some(Locale { lang: lang.to_string(), country: part(country), modifier: part(modifier) })
    }

    /// LC_ALL, then LC_MESSAGES, then LANG (first non-empty).
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Option<Locale> {
        ["LC_ALL", "LC_MESSAGES", "LANG"].into_iter().filter_map(&var).find(|v| !v.is_empty()).and_then(|v| Locale::parse(&v))
    }

    /// Keys to try for `key`, in the spec's order: key[lang_COUNTRY@MOD], key[lang_COUNTRY],
    /// key[lang@MOD], key[lang], then key.
    pub fn keys(&self, key: &str) -> Vec<String> {
        let l = &self.lang;
        let mut keys = Vec::with_capacity(5);
        if let (Some(c), Some(m)) = (&self.country, &self.modifier) {
            keys.push(format!("{key}[{l}_{c}@{m}]"));
        }
        if let Some(c) = &self.country {
            keys.push(format!("{key}[{l}_{c}]"));
        }
        if let Some(m) = &self.modifier {
            keys.push(format!("{key}[{l}@{m}]"));
        }
        keys.push(format!("{key}[{l}]"));
        keys.push(key.to_string());
        keys
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAction {
    pub key: String,
    pub name: String,
    pub exec: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    /// e.g. "org.mozilla.firefox.desktop"
    pub id: String,
    pub path: PathBuf,
    /// Localized.
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    /// Localized list.
    pub keywords: Vec<String>,
    pub icon: Option<String>,
    /// Value-unescaped; field codes still present.
    pub exec: String,
    pub terminal: bool,
    pub startup_wm_class: Option<String>,
    /// In `Actions=` order, only those with a group, a Name and a valid Exec.
    pub actions: Vec<DesktopAction>,
}

pub struct ParseEnv<'a> {
    pub locale: Option<&'a Locale>,
    /// XDG_CURRENT_DESKTOP split on ':'; empty → OnlyShowIn/NotShowIn ignored.
    pub desktops: &'a [String],
    /// TryExec resolution (absolute path: executable file; bare name: on $PATH).
    pub has_program: &'a dyn Fn(&str) -> bool,
}

// One short-lived value per file read: the size difference with `Mask` does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Show(DesktopEntry),
    /// A valid file that must not be shown; it still claims its id (masks lower dirs).
    /// Reasons: "NoDisplay", "Hidden", "OnlyShowIn", "NotShowIn", "TryExec", "not an application".
    Mask(&'static str),
}

/// A group's keys, first occurrence wins; raw values (not yet unescaped).
type Group<'t> = HashMap<&'t str, &'t str>;

/// The `[Desktop Entry]` group (which must come first) and the `[Desktop
/// Action x]` groups; any other group is ignored, as is a repeated one.
fn groups(text: &str) -> Result<(Group<'_>, HashMap<&str, Group<'_>>), String> {
    enum In<'t> {
        Main,
        Action(&'t str),
        Ignored,
    }
    let mut main: Option<Group> = None;
    let mut actions: HashMap<&str, Group> = HashMap::new();
    let mut current = In::Ignored;
    for (n, raw) in text.split('\n').enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let name =
                line.strip_prefix('[').and_then(|l| l.strip_suffix(']')).ok_or_else(|| format!("line {}: bad group header", n + 1))?;
            current = if main.is_none() {
                if name != "Desktop Entry" {
                    return Err(format!("first group is [{name}], not [Desktop Entry]"));
                }
                main = Some(Group::new());
                In::Main
            } else {
                match name.strip_prefix("Desktop Action ") {
                    Some(key) if !actions.contains_key(key) => {
                        actions.insert(key, Group::new());
                        In::Action(key)
                    }
                    _ => In::Ignored,
                }
            };
            continue;
        }
        let (key, value) = line.split_once('=').ok_or_else(|| format!("line {}: not a key=value line", n + 1))?;
        let (key, value) = (key.trim_end(), value.trim_start());
        if key.is_empty() {
            return Err(format!("line {}: empty key", n + 1));
        }
        let group = match (&current, main.as_mut()) {
            (_, None) => return Err(format!("line {}: key before the [Desktop Entry] group", n + 1)),
            (In::Main, Some(m)) => m,
            (In::Action(k), Some(_)) => actions.get_mut(k).expect("inserted with its header"),
            (In::Ignored, Some(_)) => continue,
        };
        group.entry(key).or_insert(value);
    }
    main.map(|m| (m, actions)).ok_or_else(|| "no [Desktop Entry] group".to_string())
}

/// `\s \n \t \r \\` in any string; `\;` too in a list item. Unknown escapes are kept as written.
fn escaped(c: char, in_list: bool) -> Option<char> {
    match c {
        's' => Some(' '),
        'n' => Some('\n'),
        't' => Some('\t'),
        'r' => Some('\r'),
        '\\' => Some('\\'),
        ';' if in_list => Some(';'),
        _ => None,
    }
}

fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(e) => match escaped(e, false) {
                Some(x) => out.push(x),
                None => {
                    out.push('\\');
                    out.push(e);
                }
            },
            None => out.push('\\'),
        }
    }
    out
}

/// A `;`-separated list (`\;` is a literal semicolon); empty items dropped.
fn list(raw: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut item = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            ';' => items.push(std::mem::take(&mut item)),
            '\\' => match chars.next() {
                Some(e) => match escaped(e, true) {
                    Some(x) => item.push(x),
                    None => {
                        item.push('\\');
                        item.push(e);
                    }
                },
                None => item.push('\\'),
            },
            c => item.push(c),
        }
    }
    items.push(item);
    items.retain(|i| !i.is_empty());
    items
}

fn localized<'t>(g: &Group<'t>, locale: Option<&Locale>, key: &str) -> Option<&'t str> {
    match locale {
        Some(l) => l.keys(key).iter().find_map(|k| g.get(k.as_str()).copied()),
        None => g.get(key).copied(),
    }
}

/// A non-blank string value, unescaped.
fn string(g: &Group, key: &str) -> Option<String> {
    g.get(key).map(|v| unescape(v)).filter(|v| !v.trim().is_empty())
}

fn is_true(g: &Group, key: &str) -> bool {
    matches!(g.get(key).copied(), Some("true" | "1"))
}

fn listed(names: &[String], desktops: &[String]) -> bool {
    names.iter().any(|n| desktops.iter().any(|d| d.eq_ignore_ascii_case(n)))
}

/// Err(reason) for an invalid file (no [Desktop Entry] group first, missing Type/Name,
/// missing or invalid Exec for an application, file not UTF-8): treated as absent by the scan.
/// `Hidden=true` and `NoDisplay=true` mask before any other check: the usual minimal user
/// override ("[Desktop Entry]" + one of them) hides the system entry instead of counting as
/// a broken file.
pub fn parse(id: &str, path: &Path, text: &str, env: &ParseEnv) -> Result<Verdict, String> {
    let (main, action_groups) = groups(text)?;
    if is_true(&main, "Hidden") {
        return Ok(Verdict::Mask("Hidden"));
    }
    if is_true(&main, "NoDisplay") {
        return Ok(Verdict::Mask("NoDisplay"));
    }
    match main.get("Type").copied() {
        None => return Err("missing Type".into()),
        Some("Application") => {}
        Some(_) => return Ok(Verdict::Mask("not an application")),
    }
    let name =
        localized(&main, env.locale, "Name").map(unescape).filter(|n| !n.trim().is_empty()).ok_or_else(|| "missing Name".to_string())?;
    let exec = string(&main, "Exec").ok_or_else(|| "missing Exec".to_string())?;
    let icon = string(&main, "Icon");
    let ctx = ExecContext { name: &name, icon: icon.as_deref(), path };
    exec_argv(&exec, &ctx).map_err(|BadExec(m)| format!("invalid Exec: {m}"))?;
    if !env.desktops.is_empty() {
        if main.get("OnlyShowIn").is_some_and(|only| !listed(&list(only), env.desktops)) {
            return Ok(Verdict::Mask("OnlyShowIn"));
        }
        if main.get("NotShowIn").is_some_and(|not| listed(&list(not), env.desktops)) {
            return Ok(Verdict::Mask("NotShowIn"));
        }
    }
    if string(&main, "TryExec").is_some_and(|program| !(env.has_program)(&program)) {
        return Ok(Verdict::Mask("TryExec"));
    }
    let mut actions: Vec<DesktopAction> = Vec::new();
    for key in main.get("Actions").map(|a| list(a)).unwrap_or_default() {
        if actions.iter().any(|a| a.key == key) {
            continue;
        }
        let Some(group) = action_groups.get(key.as_str()) else { continue };
        let Some(action_name) = localized(group, env.locale, "Name").map(unescape).filter(|n| !n.trim().is_empty()) else { continue };
        let Some(action_exec) = string(group, "Exec") else { continue };
        if exec_argv(&action_exec, &ctx).is_err() {
            continue;
        }
        actions.push(DesktopAction { key, name: action_name, exec: action_exec });
    }
    Ok(Verdict::Show(DesktopEntry {
        id: id.to_string(),
        path: path.to_path_buf(),
        generic_name: localized(&main, env.locale, "GenericName").map(unescape).filter(|s| !s.trim().is_empty()),
        comment: localized(&main, env.locale, "Comment").map(unescape).filter(|s| !s.trim().is_empty()),
        keywords: localized(&main, env.locale, "Keywords").map(list).unwrap_or_default(),
        terminal: is_true(&main, "Terminal"),
        startup_wm_class: string(&main, "StartupWMClass"),
        name,
        icon,
        exec,
        actions,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadExec(pub String);

pub struct ExecContext<'a> {
    pub name: &'a str,
    pub icon: Option<&'a str>,
    pub path: &'a Path,
}

/// Exec quoting as GLib/KDE do it (g_shell_parse_argv): double quotes (`\"` `` \` `` `\$` `\\`
/// inside them), single quotes (literal), a backslash outside quotes escapes the next character;
/// then field codes, outside quotes and inside double quotes alike:
/// %f %F %u %U and deprecated %d %D %n %N %v %m → removed (no file passed);
/// %i → "--icon" + icon when icon is Some, removed otherwise; %c → name; %k → path; %% → "%";
/// any other %x → Err. An argument that was only removed codes (quoted or not) disappears.
/// The spec forbids field codes inside quotes; real files have them and GLib expands them
/// there too. `%i` must be an unquoted argument of its own.
pub fn exec_argv(exec: &str, ctx: &ExecContext) -> Result<Vec<String>, BadExec> {
    let bad = |m: &str| Err(BadExec(m.to_string()));
    let mut argv = Vec::new();
    let mut chars = exec.chars().peekable();
    loop {
        while chars.next_if(|c| *c == ' ' || *c == '\t').is_some() {}
        if chars.peek().is_none() {
            break;
        }
        let mut arg = String::new();
        let (mut quoted, mut code, mut icon) = (false, false, false);
        let mut quote: Option<char> = None;
        loop {
            let Some(c) = chars.next() else {
                if quote.is_some() {
                    return bad("unterminated quote");
                }
                break;
            };
            match (quote, c) {
                (Some('\''), '\'') | (Some('"'), '"') => quote = None,
                // Single quotes: everything literal, field codes included.
                (Some('\''), _) => arg.push(c),
                (Some(_), '\\') => match chars.next() {
                    Some(e @ ('"' | '`' | '$' | '\\')) => arg.push(e),
                    Some(other) => {
                        arg.push('\\');
                        arg.push(other);
                    }
                    None => return bad("unterminated quote"),
                },
                (None, ' ' | '\t') => break,
                (None, '"' | '\'') => {
                    quote = Some(c);
                    quoted = true;
                }
                (None, '\\') => match chars.next() {
                    Some(e) => arg.push(e),
                    None => return bad("lone \\ at the end"),
                },
                // Unquoted or inside double quotes.
                (_, '%') => {
                    code = true;
                    match chars.next() {
                        Some('%') => arg.push('%'),
                        Some('f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm') => {}
                        Some('c') => arg.push_str(ctx.name),
                        Some('k') => arg.push_str(&ctx.path.to_string_lossy()),
                        Some('i') => icon = true,
                        Some(x) => return Err(BadExec(format!("unknown field code %{x}"))),
                        None => return bad("lone % at the end"),
                    }
                }
                _ => arg.push(c),
            }
        }
        if icon {
            if !arg.is_empty() || quoted {
                return bad("%i must be an argument of its own");
            }
            if let Some(i) = ctx.icon {
                argv.push("--icon".to_string());
                argv.push(i.to_string());
            }
            continue;
        }
        // `"%f"` disappears like `%f`; an explicit `""` (no code) stays an empty argument.
        if arg.is_empty() && code {
            continue;
        }
        argv.push(arg);
    }
    if argv.first().is_none_or(|p| p.is_empty()) {
        return bad("no program");
    }
    Ok(argv)
}

/// "<applications dir>/kde/foo.desktop" → "kde-foo.desktop"; None if not a .desktop under it.
pub fn desktop_id(applications_dir: &Path, file: &Path) -> Option<String> {
    let relative = file.strip_prefix(applications_dir).ok()?;
    let mut parts = Vec::new();
    for c in relative.components() {
        match c {
            Component::Normal(p) => parts.push(p.to_str()?),
            _ => return None,
        }
    }
    let id = parts.join("-");
    (id.len() > ".desktop".len() && id.ends_with(".desktop")).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/desktop");

    fn no_program(_: &str) -> bool {
        false
    }

    fn any_program(_: &str) -> bool {
        true
    }

    fn penv<'a>(locale: Option<&'a Locale>, desktops: &'a [String], has_program: &'a dyn Fn(&str) -> bool) -> ParseEnv<'a> {
        ParseEnv { locale, desktops, has_program }
    }

    fn plain_env() -> ParseEnv<'static> {
        penv(None, &[], &any_program)
    }

    fn verdict(name: &str, env: &ParseEnv) -> Result<Verdict, String> {
        let path = Path::new(FIXTURES).join(name);
        let text = std::fs::read_to_string(&path).unwrap();
        parse(name, &path, &text, env)
    }

    fn shown(name: &str, env: &ParseEnv) -> DesktopEntry {
        match verdict(name, env) {
            Ok(Verdict::Show(e)) => e,
            other => panic!("{name}: {other:?}"),
        }
    }

    fn inline(text: &str, env: &ParseEnv) -> Result<Verdict, String> {
        parse("inline.desktop", Path::new("/x/inline.desktop"), text, env)
    }

    fn ctx<'a>(name: &'a str, icon: Option<&'a str>) -> ExecContext<'a> {
        ExecContext { name, icon, path: Path::new("/apps/example.desktop") }
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_plain_entry_is_read() {
        let expected = DesktopEntry {
            id: "plain.desktop".into(),
            path: Path::new(FIXTURES).join("plain.desktop"),
            name: "Example Editor".into(),
            generic_name: Some("Text editor".into()),
            comment: Some("Edit plain text files".into()),
            keywords: strings(&["text", "notes"]),
            icon: Some("example-editor".into()),
            exec: "example-editor %F".into(),
            terminal: false,
            startup_wm_class: Some("ExampleEditor".into()),
            actions: vec![],
        };
        assert_eq!(shown("plain.desktop", &plain_env()), expected);
    }

    #[test]
    fn localized_keys_follow_the_locale_order() {
        let name = |l: Option<&str>| {
            let locale = l.and_then(Locale::parse);
            shown("localized.desktop", &penv(locale.as_ref(), &[], &any_program)).name
        };
        assert_eq!(name(Some("fr_FR.UTF-8")), "Fichiers (France)");
        assert_eq!(name(Some("fr_CA.UTF-8")), "Fichiers", "no fr_CA: falls back to fr");
        assert_eq!(name(Some("fr")), "Fichiers");
        assert_eq!(name(Some("de_AT@euro")), "Dateien");
        assert_eq!(name(Some("it_IT")), "Files");
        assert_eq!(name(None), "Files", "and the later repeated Name= is ignored");
        let fr = Locale::parse("fr_BE").unwrap();
        let e = shown("localized.desktop", &penv(Some(&fr), &[], &any_program));
        assert_eq!(e.generic_name.as_deref(), Some("Gestionnaire de fichiers"));
        assert_eq!(e.keywords, strings(&["dossier", "gestionnaire;fichiers", "explorateur de fichiers"]));
        assert_eq!(shown("localized.desktop", &plain_env()).keywords, strings(&["folder", "manager"]));
    }

    #[test]
    fn value_escapes_are_decoded() {
        let e = shown("localized.desktop", &plain_env());
        assert_eq!(e.comment.as_deref(), Some("Line one\nLine two\twith tab\\backslash and\rreturn"));
    }

    #[test]
    fn locale_parsing_and_lookup_order() {
        let fr = Locale { lang: "fr".into(), country: Some("FR".into()), modifier: Some("euro".into()) };
        assert_eq!(Locale::parse("fr_FR.UTF-8@euro"), Some(fr.clone()));
        assert_eq!(Locale::parse("sr@latin"), Some(Locale { lang: "sr".into(), country: None, modifier: Some("latin".into()) }));
        for none in ["C", "POSIX", "", "C.UTF-8"] {
            assert_eq!(Locale::parse(none), None, "{none:?}");
        }
        assert_eq!(fr.keys("Name"), ["Name[fr_FR@euro]", "Name[fr_FR]", "Name[fr@euro]", "Name[fr]", "Name"]);
        assert_eq!(Locale::parse("fr").unwrap().keys("Name"), ["Name[fr]", "Name"]);
        let vars =
            |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        assert_eq!(
            Locale::from_env(vars(&[("LC_ALL", ""), ("LC_MESSAGES", "de_DE.UTF-8"), ("LANG", "fr_FR.UTF-8")])),
            Locale::parse("de_DE")
        );
        assert_eq!(Locale::from_env(vars(&[("LANG", "fr_FR.UTF-8")])), Locale::parse("fr_FR"));
        assert_eq!(Locale::from_env(vars(&[("LC_ALL", "C"), ("LANG", "fr_FR.UTF-8")])), None, "LC_ALL=C wins");
        assert_eq!(Locale::from_env(vars(&[])), None);
    }

    #[test]
    fn exec_quoting_follows_the_spec() {
        let e = shown("quoted-exec.desktop", &plain_env());
        assert_eq!(
            e.exec, r#""/opt/Example App/bin/example" --title "Hello \"World\"" --price "\$5" --path "C:\\tmp" %u"#,
            "string escapes decoded first, quoting left to exec_argv"
        );
        let argv = strings(&["/opt/Example App/bin/example", "--title", "Hello \"World\"", "--price", "$5", "--path", r"C:\tmp"]);
        assert_eq!(exec_argv(&e.exec, &ctx("Quoted", None)), Ok(argv));
    }

    #[test]
    fn field_codes_expand_or_disappear() {
        let e = shown("field-codes.desktop", &plain_env());
        let path = e.path.to_str().unwrap();
        let with_icon = ExecContext { name: &e.name, icon: e.icon.as_deref(), path: &e.path };
        assert_eq!(
            exec_argv(&e.exec, &with_icon),
            Ok(strings(&[
                "example-viewer",
                "--icon",
                "example-icon",
                "--name",
                "Field Codes",
                "--from",
                path,
                "--percent",
                "100%",
                "--file="
            ]))
        );
        let without = ExecContext { icon: None, ..with_icon };
        assert_eq!(
            exec_argv(&e.exec, &without),
            Ok(strings(&["example-viewer", "--name", "Field Codes", "--from", path, "--percent", "100%", "--file="]))
        );
    }

    #[test]
    fn bad_exec_values_are_refused() {
        let c = ctx("X", Some("x"));
        for (exec, needle) in [
            ("example %z", "%z"),
            ("example \"unterminated", "unterminated"),
            ("example 100%", "lone %"),
            ("", "no program"),
            ("   ", "no program"),
            ("%f", "no program"),
            ("example --icon=%i", "%i"),
        ] {
            let BadExec(m) = exec_argv(exec, &c).unwrap_err();
            assert!(m.contains(needle), "{exec:?}: {m}");
        }
        let m = verdict("broken-bad-exec.desktop", &plain_env()).unwrap_err();
        assert!(m.contains("%z"), "{m}");
    }

    #[test]
    fn exec_keeps_quoted_empty_arguments_and_splits_on_tabs() {
        let c = ctx("X", None);
        assert_eq!(exec_argv("prog \"\"\t--flag  last", &c), Ok(strings(&["prog", "", "--flag", "last"])));
        assert_eq!(exec_argv("prog --opt=\"a b\"", &c), Ok(strings(&["prog", "--opt=a b"])));
        assert_eq!(exec_argv("prog ''", &c), Ok(strings(&["prog", ""])), "an explicit empty argument stays");
    }

    /// The spec's quoting is stricter than what GLib and KDE accept; real files
    /// follow the latter (g_shell_parse_argv), so does the launcher.
    #[test]
    fn exec_follows_glib_quoting_and_field_codes_in_double_quotes() {
        let c = ctx("Field Codes", None);
        assert_eq!(exec_argv("sh -c 'foo bar'", &c), Ok(strings(&["sh", "-c", "foo bar"])));
        assert_eq!(
            exec_argv("sh -c 'date +%H; echo \"$HOME\" \\'", &c),
            Ok(strings(&["sh", "-c", "date +%H; echo \"$HOME\" \\"])),
            "single quotes: literal, field codes and backslashes included"
        );
        assert_eq!(exec_argv("prog \"%u\" \"--name=%c\" \"%%\"", &c), Ok(strings(&["prog", "--name=Field Codes", "%"])));
        assert_eq!(exec_argv("prog it\\'s a\\ b", &c), Ok(strings(&["prog", "it's", "a b"])), "backslash outside quotes");
        assert_eq!(exec_argv("prog 'a'\"b\"c", &c), Ok(strings(&["prog", "abc"])));
        for (exec, needle) in [("prog 'open", "unterminated"), ("prog \"%i\"", "%i"), ("prog \\", "lone \\"), ("prog \"%z\"", "%z")] {
            let BadExec(m) = exec_argv(exec, &c).unwrap_err();
            assert!(m.contains(needle), "{exec:?}: {m}");
        }
    }

    #[test]
    fn actions_follow_the_actions_key_order() {
        let e = shown("actions.desktop", &plain_env());
        let expected = vec![
            DesktopAction { key: "new-window".into(), name: "New window".into(), exec: "example-browser --new-window %u".into() },
            DesktopAction {
                key: "new-private-window".into(),
                name: "New private window".into(),
                exec: "example-browser --private-window %u".into(),
            },
        ];
        assert_eq!(e.actions, expected, "missing group, missing Exec and unlisted groups skipped");
        let fr = Locale::parse("fr_FR").unwrap();
        assert_eq!(shown("actions.desktop", &penv(Some(&fr), &[], &any_program)).actions[1].name, "Nouvelle fenêtre privée");
    }

    #[test]
    fn hidden_and_nodisplay_mask() {
        assert_eq!(verdict("hidden.desktop", &plain_env()), Ok(Verdict::Mask("Hidden")), "Hidden=true masks even a minimal file");
        assert_eq!(verdict("nodisplay.desktop", &plain_env()), Ok(Verdict::Mask("NoDisplay")));
        // The usual way to hide an app: a two-line copy in ~/.local/share/applications.
        assert_eq!(inline("[Desktop Entry]\nNoDisplay=true\n", &plain_env()), Ok(Verdict::Mask("NoDisplay")));
        assert!(inline("[Desktop Entry]\nNoDisplay=false\n", &plain_env()).is_err(), "not hidden: an invalid file");
    }

    #[test]
    fn try_exec_needs_its_program() {
        assert_eq!(verdict("tryexec-missing.desktop", &penv(None, &[], &no_program)), Ok(Verdict::Mask("TryExec")));
        assert!(matches!(verdict("tryexec-missing.desktop", &plain_env()), Ok(Verdict::Show(_))));
        let asked = RefCell::new(Vec::new());
        let spy = |p: &str| {
            asked.borrow_mut().push(p.to_string());
            false
        };
        let _ = verdict("tryexec-missing.desktop", &penv(None, &[], &spy));
        assert_eq!(*asked.borrow(), ["example-not-installed"]);
    }

    #[test]
    fn only_show_in_and_not_show_in() {
        let only = |desktops: &[&str]| verdict("onlyshowin.desktop", &penv(None, &strings(desktops), &any_program));
        assert_eq!(only(&["Hyprland"]), Ok(Verdict::Mask("OnlyShowIn")));
        assert!(matches!(only(&["GNOME"]), Ok(Verdict::Show(_))));
        assert!(matches!(only(&["gnome"]), Ok(Verdict::Show(_))), "desktop names compared case-insensitively");
        assert!(matches!(only(&[]), Ok(Verdict::Show(_))), "no XDG_CURRENT_DESKTOP: ignored");
        let not = "[Desktop Entry]\nType=Application\nName=Not here\nExec=x\nNotShowIn=Hyprland;\n";
        let with = |desktops: &[&str]| inline(not, &penv(None, &strings(desktops), &any_program));
        assert_eq!(with(&["wlroots", "Hyprland"]), Ok(Verdict::Mask("NotShowIn")));
        assert!(matches!(with(&["niri"]), Ok(Verdict::Show(_))));
        assert!(matches!(with(&[]), Ok(Verdict::Show(_))));
    }

    #[test]
    fn terminal_flag() {
        assert!(shown("terminal.desktop", &plain_env()).terminal);
        assert!(!shown("plain.desktop", &plain_env()).terminal);
    }

    #[test]
    fn a_link_is_not_an_application() {
        assert_eq!(verdict("link.desktop", &plain_env()), Ok(Verdict::Mask("not an application")));
    }

    #[test]
    fn broken_files_are_invalid() {
        let m = verdict("broken-no-group.desktop", &plain_env()).unwrap_err();
        assert!(m.contains("before the [Desktop Entry] group"), "{m}");
        for (text, needle) in [
            ("", "no [Desktop Entry]"),
            ("# only a comment\n", "no [Desktop Entry]"),
            ("[Desktop Action new]\nName=x\n[Desktop Entry]\nType=Application\nName=x\nExec=x\n", "first group"),
            ("[Desktop Entry]\nName=x\nExec=x\n", "missing Type"),
            ("[Desktop Entry]\nType=Application\nExec=x\n", "missing Name"),
            ("[Desktop Entry]\nType=Application\nName=x\n", "missing Exec"),
            ("[Desktop Entry]\nType=Application\nName=x\nExec=x\nthis line has no equals sign\n", "line 5"),
            ("[Desktop Entry\nType=Application\n", "line 1"),
        ] {
            let m = inline(text, &plain_env()).unwrap_err();
            assert!(m.contains(needle), "{text:?}: {m}");
        }
    }

    #[test]
    fn later_groups_are_ignored_except_actions_and_crlf_is_tolerated() {
        let text = "[Desktop Entry]\r\nType=Application\r\nName = Spaced \r\nExec=x\r\n\r\n[X-Vendor Extras]\r\nName=Vendor\r\n[Desktop Entry]\r\nName=Second\r\n";
        let Ok(Verdict::Show(e)) = inline(text, &plain_env()) else { panic!("not shown") };
        assert_eq!((e.name.as_str(), e.exec.as_str()), ("Spaced", "x"));
    }

    #[test]
    fn desktop_ids_join_subdirectories_with_dashes() {
        let apps = Path::new("/share/applications");
        let id = |f: &str| desktop_id(apps, Path::new(f));
        assert_eq!(id("/share/applications/foo.desktop").as_deref(), Some("foo.desktop"));
        assert_eq!(id("/share/applications/kde/foo.desktop").as_deref(), Some("kde-foo.desktop"));
        assert_eq!(id("/share/applications/a/b/c.desktop").as_deref(), Some("a-b-c.desktop"));
        assert_eq!(id("/other/foo.desktop"), None);
        assert_eq!(id("/share/applications/foo.txt"), None);
        assert_eq!(id("/share/applications/.desktop"), None);
    }
}
