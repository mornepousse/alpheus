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
    assert_eq!(
        Locale::parse("sr@latin").unwrap().keys("Name"),
        ["Name[sr@latin]", "Name[sr]", "Name"],
        "a modifier with no country still gets its own key, before the plain lang key"
    );
    let vars =
        |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
    assert_eq!(Locale::from_env(vars(&[("LC_ALL", ""), ("LC_MESSAGES", "de_DE.UTF-8"), ("LANG", "fr_FR.UTF-8")])), Locale::parse("de_DE"));
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
        Ok(strings(&["example-viewer", "--icon", "example-icon", "--name", "Field Codes", "--from", path, "--percent", "100%", "--file="]))
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

/// The Desktop Entry spec's quoting is stricter than what GLib and KDE accept; real files
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
    assert_eq!(e.actions, expected, "missing group, missing Exec, invalid Exec and unlisted groups skipped");
    let fr = Locale::parse("fr_FR").unwrap();
    assert_eq!(shown("actions.desktop", &penv(Some(&fr), &[], &any_program)).actions[1].name, "Nouvelle fenêtre privée");
}

/// A hostile or malformed file could list thousands of keywords or actions: both are
/// capped, so one file cannot bloat the catalog kept in memory.
#[test]
fn keywords_and_actions_are_capped() {
    let keywords: String = (0..50).map(|i| format!("k{i};")).collect();
    let text = format!("[Desktop Entry]\nType=Application\nName=Many\nExec=many\nKeywords={keywords}\n");
    let Ok(Verdict::Show(e)) = inline(&text, &plain_env()) else { panic!("expected Show") };
    assert_eq!(e.keywords.len(), 32, "50 declared, capped to 32");
    assert_eq!(e.keywords[0], "k0", "the first ones are kept, not the last ones");
    assert_eq!(e.keywords[31], "k31");

    let actions_list: String = (0..20).map(|i| format!("a{i};")).collect();
    let groups: String = (0..20).map(|i| format!("\n[Desktop Action a{i}]\nName=Action {i}\nExec=many --a{i}\n")).collect();
    let text = format!("[Desktop Entry]\nType=Application\nName=Many\nExec=many\nActions={actions_list}\n{groups}");
    let Ok(Verdict::Show(e)) = inline(&text, &plain_env()) else { panic!("expected Show") };
    assert_eq!(e.actions.len(), 16, "20 valid actions declared, capped to 16");
    assert_eq!(e.actions[0].key, "a0", "the first ones are kept, not the last ones");
    assert_eq!(e.actions[15].key, "a15");
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
fn an_entry_whose_program_is_gone_is_absent() {
    // An app removed without its .desktop file (a store path collected by the GC, an /opt
    // directory deleted): its Exec program no longer exists. Absent, not masked, so a valid
    // entry of the same id in a lower directory still shows.
    let asked = RefCell::new(Vec::new());
    let only = |present: &'static str| {
        let asked = &asked;
        move |p: &str| {
            asked.borrow_mut().push(p.to_string());
            p == present
        }
    };
    let gone = "[Desktop Entry]\nType=Application\nName=Gone\nExec=\"/opt/gone app/bin/gone\" %u\n";
    assert_eq!(inline(gone, &penv(None, &[], &only("other"))), Err("program not found".to_string()));
    assert_eq!(asked.borrow().last().map(String::as_str), Some("/opt/gone app/bin/gone"), "the unquoted program");
    assert!(matches!(inline(gone, &penv(None, &[], &only("/opt/gone app/bin/gone"))), Ok(Verdict::Show(_))));
    let bare = "[Desktop Entry]\nType=Application\nName=Bare\nExec=gone --flag\n";
    assert_eq!(inline(bare, &penv(None, &[], &only("other"))), Err("program not found".to_string()));
    assert!(matches!(inline(bare, &penv(None, &[], &only("gone"))), Ok(Verdict::Show(_))));
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
fn blank_translations_fall_back_to_the_next_candidate() {
    let fr = Locale::parse("fr").unwrap();
    let e = shown("blank-translation.desktop", &penv(Some(&fr), &[], &any_program));
    assert_eq!(e.name, "Files", "an empty Name[fr]= is skipped, not kept as a blank name");
    assert_eq!(e.comment.as_deref(), Some("Manage your files"), "same for Comment[fr]=");
}

#[test]
fn is_true_accepts_1() {
    let mut g: Group = Group::new();
    g.insert("Terminal", "1");
    assert!(is_true(&g, "Terminal"));
    g.insert("Terminal", "true");
    assert!(is_true(&g, "Terminal"));
    g.insert("Terminal", "yes");
    assert!(!is_true(&g, "Terminal"));
}

#[test]
fn escaped_semicolon_is_literal_only_in_lists() {
    assert_eq!(escaped(';', false), None, "outside a list, \\; is not a recognized escape");
    assert_eq!(escaped(';', true), Some(';'));
}

#[test]
fn a_leading_bom_is_stripped() {
    let text = "\u{FEFF}[Desktop Entry]\nType=Application\nName=With BOM\nExec=x\n";
    let Ok(Verdict::Show(e)) = inline(text, &plain_env()) else { panic!("not shown") };
    assert_eq!(e.name, "With BOM");
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
