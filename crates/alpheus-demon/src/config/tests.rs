use super::*;
use alpheus_commun::temp_path::TempPath;

fn file(name: &str, content: &str) -> TempPath {
    let p = TempPath::dir(&format!("bar-{name}")).below("bar.toml");
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn absent_defaults_without_message() {
    let (c, w) = BarConfig::load(Path::new("/nonexistent/bar.toml"));
    assert_eq!((c, w), (BarConfig::default(), None));
    let d = BarConfig::default();
    assert_eq!((d.modules.first(), d.modules.last()), (Some(&ModuleId::Launcher), Some(&ModuleId::Power)));
    assert_eq!((d.height, d.font.size, d.font.fallback.as_slice()), (22, 14.0, ["DejaVu Sans Mono".to_string()].as_slice()));
    assert_eq!(d.workspaces.command, ["wm-set", "workspace", "{workspace}", "{output}"]);
}

#[test]
fn launcher_module_first_by_default_and_removable() {
    assert_eq!(ModuleId::ALL[0], ModuleId::Launcher);
    assert_eq!(ModuleId::Launcher.name(), "launcher");
    let c = BarConfig::from_text("modules = [\"clock\", \"launcher\"]").unwrap();
    assert_eq!(c.modules, vec![ModuleId::Clock, ModuleId::Launcher], "named \"launcher\" in bar.toml, placed anywhere");
    let c = BarConfig::from_text("modules = [\"workspaces\", \"clock\"]").unwrap();
    assert!(!c.modules.contains(&ModuleId::Launcher), "removable");
    for m in ModuleId::ALL {
        let parsed = BarConfig::from_text(&format!("modules = [\"{}\"]", m.name())).unwrap();
        assert_eq!(parsed.modules, vec![m], "name() is the bar.toml name of {m:?}");
    }
}

/// Power spec § 1: `power` parses, is the last of `ALL`, and the default order ends with the
/// clock then the power button.
#[test]
fn the_power_module_parses_and_ends_the_default_order() {
    assert_eq!(ModuleId::Power.name(), "power");
    assert_eq!(ModuleId::ALL.last(), Some(&ModuleId::Power));
    let c = BarConfig::from_text("modules = [\"clock\", \"power\"]").unwrap();
    assert_eq!(c.modules, vec![ModuleId::Clock, ModuleId::Power]);
    let d = BarConfig::default();
    assert_eq!(d.modules[d.modules.len() - 2..], [ModuleId::Clock, ModuleId::Power]);
}

#[test]
fn settings_read() {
    let text = "modules = [\"clock\", \"workspaces\"]\nheight = 28\n[font]\nfamily = \"Fira Code\"\nfallback = [\"Noto Color Emoji\"]\n[clock]\nformat = \"%H:%M:%S\"\n[workspaces]\ncommand = [\"my-tool\", \"{workspace}\"]\n";
    let (c, w) = BarConfig::load(&file("ok", text));
    assert_eq!(w, None);
    assert_eq!(c.modules, vec![ModuleId::Clock, ModuleId::Workspaces]);
    assert_eq!((c.height, c.font.family.as_str(), c.font.size), (28, "Fira Code", 14.0));
    assert_eq!((c.clock.format.as_str(), c.clock.date.as_str()), ("%H:%M:%S", "%a %e %b"));
    assert_eq!(c.font.fallback, ["Noto Color Emoji"]);
    assert_eq!(c.workspaces.command, ["my-tool", "{workspace}"]);
}

#[test]
fn unknown_module_defaults_and_message() {
    let (c, w) = BarConfig::load(&file("unknown", "modules = [\"workspaces\", \"weather\"]\n"));
    assert_eq!(c, BarConfig::default());
    let w = w.unwrap();
    assert!(w.contains("bar.toml invalid") && w.contains("weather"), "{w}");
}

#[test]
fn refused_with_reason() {
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    assert!(err("modules = [\"clock\", \"clock\"]").contains("duplicate"));
    assert!(err("[clock]\nformat = \"%Q\"").contains("clock.format"));
    assert!(err("[clock]\ndate = \"%\"").contains("clock.date"));
    assert!(err("[clock]\nformat = \"\"").contains("empty"));
    assert!(err("height = 4").contains("height"));
    assert!(err("[font]\nsize = 200.0").contains("font.size"));
    assert!(err("[font]\nfamily = \" \"").contains("font.family"));
    assert!(err("[workspaces]\ncommand = []").contains("workspaces.command"));
    assert!(BarConfig::from_text("size = 3").is_err(), "unknown key refused");
    assert_eq!(BarConfig::from_text("modules = []").unwrap().modules, vec![]);
}

#[test]
fn actions_and_resources_defaults_and_settings() {
    let d = BarConfig::default();
    assert_eq!(d.actions.open_tab, ["alpheus-toggle", "{tab}"]);
    assert_eq!(d.actions.resources, ["foot", "-e", "btop"]);
    assert_eq!((d.resources.interval, d.resources.graph, d.resources.sensor.as_str()), (2, 20, ""));
    let text = "[actions]\nopen_tab = [\"my-panel\", \"--tab\", \"{tab}\"]\nresources = [\"foot\", \"htop\"]\n[resources]\ninterval = 5\ngraph = 0\nsensor = \"k10temp\"\n";
    let c = BarConfig::from_text(text).unwrap();
    assert_eq!(c.actions.open_tab, ["my-panel", "--tab", "{tab}"]);
    assert_eq!(c.actions.resources, ["foot", "htop"]);
    assert_eq!((c.resources.interval, c.resources.graph, c.resources.sensor.as_str()), (5, 0, "k10temp"));
}

#[test]
fn resources_show_defaults_and_validation() {
    use crate::config::ResourceItem::*;
    let d = BarConfig::default();
    assert_eq!(d.resources.show, [Cpu, Memory, Temperature]);
    assert_eq!(d.resources.gpu_sensor, "");
    assert_eq!(BarConfig::from_text("[resources]\nshow = [\"gpu\", \"net\"]\n").unwrap().resources.show, [Gpu, Net]);
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    assert!(err("[resources]\nshow = [\"cpu\", \"cpu\"]").contains("resources.show: duplicate \"cpu\""));
    assert!(err("[resources]\nshow = [\"fan\"]").contains("unknown variant"));
    let e = err("[resources]\nshow = []");
    assert!(e.contains("resources.show empty") && e.contains("remove \"resources\" from modules instead"), "{e}");
}

#[test]
fn actions_and_resources_refused_with_reason() {
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    assert!(err("[resources]\ninterval = 0").contains("resources.interval"));
    assert!(err("[resources]\ninterval = 61").contains("resources.interval"));
    assert!(err("[resources]\ngraph = 61").contains("resources.graph"));
    assert!(err("[actions]\nopen_tab = []").contains("actions.open_tab"));
    assert!(err("[actions]\nresources = [\" \"]").contains("actions.resources"));
    assert!(err("[resources]\nperiod = 2").contains("period"), "unknown key refused");
    let c = BarConfig::from_text("[resources]\ninterval = 60\ngraph = 60").unwrap();
    assert_eq!((c.resources.interval, c.resources.graph), (60, GRAPH_MAX), "bounds included");
}

#[test]
fn battery_defaults_and_settings() {
    let d = BarConfig::default();
    assert_eq!((d.battery.peripherals, d.battery.coarse), (BatteryVisibility::Always, BatteryVisibility::Low));
    let text = "[battery]\nperipherals = \"low\"\ncoarse = \"never\"\n";
    let c = BarConfig::from_text(text).unwrap();
    assert_eq!((c.battery.peripherals, c.battery.coarse), (BatteryVisibility::Low, BatteryVisibility::Never));
    let text = "[battery]\nperipherals = \"never\"\ncoarse = \"always\"\n";
    let c = BarConfig::from_text(text).unwrap();
    assert_eq!((c.battery.peripherals, c.battery.coarse), (BatteryVisibility::Never, BatteryVisibility::Always));
}

#[test]
fn battery_refused_with_reason() {
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    assert!(err("[battery]\nperipherals = \"sometimes\"").contains("sometimes"));
    assert!(err("[battery]\ncoarse = \"soon\"").contains("soon"));
    assert!(err("[battery]\nshown = true").contains("shown"), "unknown key refused");
}

#[test]
fn tray_and_clock_keys_parse_with_defaults() {
    let d = BarConfig::default();
    assert_eq!(d.tray, TrayConfig { passive: Passive::Dim, tooltips: true, icons: BTreeMap::new() });
    assert_eq!((d.clock.calendar, d.clock.week_numbers), (true, false), "the calendar on, week numbers off");
    assert_eq!(BarConfig::from_text("height = 22").unwrap().tray, TrayConfig::default(), "absent: defaults");
    let text = "[tray]\npassive = \"hide\"\ntooltips = false\n[tray.icons]\nkeepassxc = \"\u{f030b}\"\n\
                    [clock]\ncalendar = false\nweek_numbers = true\n";
    let c = BarConfig::from_text(text).unwrap();
    assert_eq!((c.tray.passive, c.tray.tooltips), (Passive::Hide, false));
    assert_eq!(c.tray.icons.get("keepassxc").map(String::as_str), Some("\u{f030b}"));
    assert_eq!((c.clock.calendar, c.clock.week_numbers), (false, true));
    assert_eq!((c.clock.format.as_str(), c.clock.date.as_str()), ("%H:%M", "%a %e %b"), "the other clock keys keep their defaults");
    assert_eq!(BarConfig::from_text("[tray]\npassive = \"show\"").unwrap().tray.passive, Passive::Show);
    assert_eq!(BarConfig::from_text("[tray]\npassive = \"dim\"").unwrap().tray.passive, Passive::Dim);
}

#[test]
fn tray_icons_keys_are_lowercased() {
    let c = BarConfig::from_text("[tray.icons]\nDiscord = \"\u{f066f}\"\n\"KeePassXC\" = \"\u{f030b}\"\n").unwrap();
    let keys: Vec<&str> = c.tray.icons.keys().map(String::as_str).collect();
    assert_eq!(keys, ["discord", "keepassxc"]);
}

#[test]
fn invalid_passive_mode_is_an_error() {
    let e = BarConfig::from_text("[tray]\npassive = \"blink\"").unwrap_err();
    assert!(e.contains("blink"), "{e}");
    let (c, w) = BarConfig::load(&file("passive", "[tray]\npassive = \"blink\"\n"));
    assert_eq!(c, BarConfig::default(), "defaults, as for any invalid bar.toml");
    assert!(w.is_some_and(|w| w.contains("bar.toml invalid")));
}

#[test]
fn an_icon_wider_than_two_cells_or_empty_is_an_error() {
    let err = |t: &str| BarConfig::from_text(&format!("[tray.icons]\n{t}\n")).unwrap_err();
    // The key is escaped ({key:?}, not {key}): "tray.icons.\"foo\"", not "tray.icons.foo".
    assert!(err("foo = \"abc\"").contains("tray.icons.\"foo\""), "three cells");
    assert!(err("foo = \"日a\"").contains("tray.icons.\"foo\""), "a wide character and one more: three cells");
    assert!(err("foo = \"\"").contains("tray.icons.\"foo\""), "empty");
    assert!(err("foo = \"\u{301}\"").contains("tray.icons.\"foo\""), "a lone combining mark: no cell");
    assert!(err("foo = \"a\\u0007\"").contains("tray.icons.\"foo\""), "a control character");
    assert!(err("\"\" = \"x\"").contains("empty key"), "an empty key");
    assert!(err("\" \" = \"x\"").contains("empty key"), "a blank key");
    assert!(err("\" discord\" = \"x\"").contains("blanks around the key"), "a key with blanks around it");
    assert!(err("\"discord \" = \"x\"").contains("blanks around the key"), "trailing blanks too");
    for ok in ["foo = \"x\"", "foo = \"ab\"", "foo = \"日\"", "foo = \"\u{f066f}\""] {
        assert!(BarConfig::from_text(&format!("[tray.icons]\n{ok}\n")).is_ok(), "{ok}: one or two cells");
    }
}

#[test]
fn unknown_tray_key_is_an_error() {
    let e = BarConfig::from_text("[tray]\ncolour = \"red\"").unwrap_err();
    assert!(e.contains("colour"), "{e}");
    assert!(BarConfig::from_text("[tray.icons]\nanything = \"x\"").is_ok(), "[tray.icons] takes any key");
}

#[test]
fn too_many_icons_is_an_error() {
    let icons = |n: usize| format!("[tray.icons]\n{}", (0..n).map(|i| format!("app{i} = \"x\"\n")).collect::<String>());
    assert_eq!(BarConfig::from_text(&icons(TRAY_ICONS_MAX)).unwrap().tray.icons.len(), TRAY_ICONS_MAX);
    let e = BarConfig::from_text(&icons(TRAY_ICONS_MAX + 1)).unwrap_err();
    assert!(e.contains("tray.icons") && e.contains("128"), "{e}");
}

#[test]
fn tray_icon_keys_differing_only_by_case_are_an_error() {
    let e = BarConfig::from_text("[tray.icons]\nDiscord = \"x\"\ndiscord = \"y\"\n").unwrap_err();
    assert!(e.contains("tray.icons.") && e.contains("another key differs from it only by case"), "{e}");
    let (c, w) = BarConfig::load(&file("icon-case", "[tray.icons]\nSteam = \"x\"\nSTEAM = \"y\"\n"));
    assert_eq!(c, BarConfig::default(), "defaults, never one of the two glyphs picked silently");
    assert!(w.is_some_and(|w| w.contains("bar.toml invalid")));
    assert!(BarConfig::from_text("[tray.icons]\ndiscord = \"x\"\nsteam = \"y\"\n").is_ok(), "distinct keys");
}

#[test]
fn tray_icons_error_messages_escape_the_key() {
    // A literal quote stands in for a TOML-escaped control character
    // (ESC, a newline…): `{key:?}` must escape it, `{key}` would print
    // it raw and could inject the character straight into the log line.
    let glyph_err = BarConfig::from_text("[tray.icons]\n'fo\"o' = \"abc\"\n").unwrap_err();
    assert!(glyph_err.contains("fo\\\"o"), "{glyph_err}");
    let case_err = BarConfig::from_text("[tray.icons]\n'Fo\"o' = \"x\"\n'fo\"o' = \"y\"\n").unwrap_err();
    assert!(case_err.contains("fo\\\"o"), "{case_err}");
}

#[test]
fn media_defaults_and_settings() {
    let d = BarConfig::default();
    assert_eq!(d.media, MediaConfig { title_max: 30, ignore: vec!["kdeconnect".to_string()] });
    assert!(d.modules.contains(&ModuleId::Media), "on by default");
    assert_eq!(BarConfig::from_text("height = 22").unwrap().media, MediaConfig::default(), "absent: defaults");
    let c = BarConfig::from_text("[media]\ntitle_max = 50\nignore = [\"firefox\", \"chromium\"]\n").unwrap();
    assert_eq!((c.media.title_max, c.media.ignore), (50, vec!["firefox".to_string(), "chromium".to_string()]));
    let c = BarConfig::from_text("[media]\nignore = []\n").unwrap();
    assert_eq!((c.media.title_max, c.media.ignore), (30, Vec::<String>::new()), "[] ignores only playerctld; title_max kept");
    for bound in [TITLE_MAX_MIN, TITLE_MAX_MAX] {
        let c = BarConfig::from_text(&format!("[media]\ntitle_max = {bound}\n")).unwrap();
        assert_eq!(c.media.title_max, bound, "bounds included");
    }
}

#[test]
fn media_refused_with_reason() {
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    assert!(err("[media]\ntitle_max = 7").contains("media.title_max 7 out of range 8 to 80"), "{}", err("[media]\ntitle_max = 7"));
    assert!(err("[media]\ntitle_max = 81").contains("media.title_max 81"));
    assert!(BarConfig::from_text("[media]\ntitle_max = -1").is_err(), "not a u16");
    assert!(BarConfig::from_text("[media]\ntitle_max = 70000").is_err(), "not a u16");
    assert!(BarConfig::from_text("[media]\nignore = \"kdeconnect\"").is_err(), "a list, not a string");
    assert!(err("[media]\nscroll = true").contains("scroll"), "unknown key refused");
    let (c, w) = BarConfig::load(&file("media", "[media]\ntitle_max = 5\n"));
    assert_eq!(c, BarConfig::default(), "defaults, as for any invalid bar.toml");
    assert!(w.is_some_and(|w| w.contains("bar.toml invalid") && w.contains("media.title_max")));
}

#[test]
fn the_privacy_module_sits_after_media_and_parses() {
    let at = |m| ModuleId::ALL.iter().position(|x| *x == m).unwrap();
    assert_eq!(ModuleId::ALL.len(), 18);
    assert_eq!(at(ModuleId::Privacy), at(ModuleId::Media) + 1, "right after media");
    assert_eq!(ModuleId::Privacy.name(), "privacy");
    assert!(BarConfig::default().modules.contains(&ModuleId::Privacy), "on by default");
    let c = BarConfig::from_text("modules = [\"privacy\", \"clock\"]").unwrap();
    assert_eq!(c.modules, vec![ModuleId::Privacy, ModuleId::Clock]);
}

#[test]
fn privacy_defaults_and_settings() {
    assert_eq!(BarConfig::default().privacy, PrivacyConfig::default());
    assert_eq!(BarConfig::default().privacy.dev_dir, PathBuf::from("/dev"));
    assert_eq!(BarConfig::default().privacy.proc_dir, PathBuf::from("/proc"));
    assert_eq!(BarConfig::from_text("height = 22").unwrap().privacy, PrivacyConfig::default(), "absent: defaults");
    let c = BarConfig::from_text("[privacy]\nignore = [\"obs\"]\n").unwrap();
    assert_eq!(c.privacy.ignore, vec!["obs".to_string()]);
}

#[test]
fn privacy_roots_default_and_parse() {
    let c = BarConfig::from_text("[privacy]\ndev_dir = \"/tmp/x\"\nproc_dir = \"/tmp/y\"\n").unwrap();
    assert_eq!(c.privacy.dev_dir, PathBuf::from("/tmp/x"));
    assert_eq!(c.privacy.proc_dir, PathBuf::from("/tmp/y"));
    let c = BarConfig::from_text("[privacy]\nignore = [\"obs\"]\n").unwrap();
    assert_eq!((c.privacy.dev_dir, c.privacy.proc_dir), (PathBuf::from("/dev"), PathBuf::from("/proc")));
}

#[test]
fn privacy_roots_must_be_absolute() {
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    assert!(err("[privacy]\ndev_dir = \"dev\"").contains("privacy.dev_dir must be an absolute path"));
    assert!(err("[privacy]\nproc_dir = \"proc\"").contains("privacy.proc_dir must be an absolute path"));
}

#[test]
fn privacy_refused_with_reason() {
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    assert!(err("[privacy]\nother = 1").contains("other"), "unknown key refused");
    assert!(BarConfig::from_text("[privacy]\nignore = \"obs\"").is_err(), "a list, not a string");
    let (c, w) = BarConfig::load(&file("privacy", "[privacy]\nother = 1\n"));
    assert_eq!(c, BarConfig::default(), "defaults, as for any invalid bar.toml");
    assert!(w.is_some_and(|w| w.contains("bar.toml invalid") && w.contains("other")));
}

#[test]
fn notifications_defaults_and_settings() {
    let d = BarConfig::default();
    assert_eq!(
        d.notifications,
        NotificationsConfig {
            timeout: 5,
            timeout_min: 2,
            timeout_max: 20,
            max_visible: 4,
            width: 40,
            history: 50,
            dnd_while_sharing: false,
            rules: Vec::new()
        }
    );
    assert!(d.modules.contains(&ModuleId::Notifications), "on by default");
    assert_eq!(BarConfig::from_text("height = 22").unwrap().notifications, NotificationsConfig::default(), "absent: defaults");
    let c = BarConfig::from_text("[notifications]\ntimeout = 8\ntimeout_max = 30\nmax_visible = 2\nwidth = 60\nhistory = 0\n").unwrap();
    assert_eq!(
        c.notifications,
        NotificationsConfig {
            timeout: 8,
            timeout_min: 2,
            timeout_max: 30,
            max_visible: 2,
            width: 60,
            history: 0,
            dnd_while_sharing: false,
            rules: Vec::new()
        }
    );
    for t in [
        "[notifications]\ntimeout_min = 1\ntimeout = 1\ntimeout_max = 1",
        "[notifications]\ntimeout_max = 600",
        "[notifications]\nmax_visible = 1",
        "[notifications]\nmax_visible = 8",
        "[notifications]\nwidth = 24",
        "[notifications]\nwidth = 80",
        "[notifications]\nhistory = 200",
    ] {
        assert!(BarConfig::from_text(t).is_ok(), "{t}: bounds included");
    }
}

#[test]
fn notifications_refused_with_reason() {
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    let order = "notifications: need 1 ≤ timeout_min ≤ timeout ≤ timeout_max ≤ 600 s";
    for (t, got) in [
        ("[notifications]\ntimeout_min = 0", "(got 0, 5, 20)"),
        ("[notifications]\ntimeout = 1", "(got 2, 1, 20)"),
        ("[notifications]\ntimeout = 30", "(got 2, 30, 20)"),
        ("[notifications]\ntimeout_max = 601", "(got 2, 5, 601)"),
    ] {
        let e = err(t);
        assert!(e.contains(order) && e.contains(got), "{t}: {e}");
    }
    for (t, want) in [
        ("[notifications]\nmax_visible = 0", "notifications.max_visible 0 out of range 1 to 8"),
        ("[notifications]\nmax_visible = 9", "notifications.max_visible 9"),
        ("[notifications]\nwidth = 23", "notifications.width 23 out of range 24 to 80 cells"),
        ("[notifications]\nwidth = 81", "notifications.width 81"),
        ("[notifications]\nhistory = 201", "notifications.history 201 out of range 0 to 200"),
    ] {
        let e = err(t);
        assert!(e.contains(want), "{t}: {e}");
    }
    assert!(BarConfig::from_text("[notifications]\nhistory = -1").is_err(), "not a u16");
    assert!(BarConfig::from_text("[notifications]\nmax_visible = 256").is_err(), "not a u8");
    assert!(err("[notifications]\nsound = true").contains("sound"), "unknown key refused");
    let (c, w) = BarConfig::load(&file("notifications", "[notifications]\nwidth = 5\n"));
    assert_eq!(c, BarConfig::default(), "defaults, as for any invalid bar.toml");
    assert!(w.is_some_and(|w| w.contains("bar.toml invalid") && w.contains("notifications.width")));
}

#[test]
fn tray_icon_key_with_a_control_character_is_an_error() {
    let e = BarConfig::from_text("[tray.icons]\n\"a\\u0007b\" = \"x\"\n").unwrap_err();
    assert!(e.contains("tray.icons.") && e.contains("control character"), "{e}");
    assert!(BarConfig::from_text("[tray.icons]\ndiscord = \"x\"\n").is_ok(), "an ordinary key still passes");
}

#[test]
fn osd_defaults_and_settings() {
    let d = BarConfig::default();
    assert_eq!(
        d.osd,
        OsdConfig {
            timeout_ms: 1300,
            width: 32,
            margin: 48,
            backlight: String::new(),
            backlight_dir: PathBuf::from("/sys/class/backlight")
        }
    );
    assert!(d.modules.contains(&ModuleId::Osd), "on by default");
    assert_eq!(d.modules[d.modules.len() - 2..], [ModuleId::Clock, ModuleId::Power], "the clock, then the power button, last");
    assert_eq!(ModuleId::Osd.name(), "osd");
    let c = BarConfig::from_text(
        "[osd]\ntimeout_ms = 2000\nwidth = 40\nmargin = 0\nbacklight = \"intel_backlight\"\nbacklight_dir = \"/tmp/fake\"\n",
    )
    .unwrap();
    assert_eq!(
        c.osd,
        OsdConfig {
            timeout_ms: 2000,
            width: 40,
            margin: 0,
            backlight: "intel_backlight".into(),
            backlight_dir: PathBuf::from("/tmp/fake")
        }
    );
    for t in ["[osd]\ntimeout_ms = 600", "[osd]\ntimeout_ms = 5000", "[osd]\nwidth = 24", "[osd]\nwidth = 60", "[osd]\nmargin = 400"] {
        assert!(BarConfig::from_text(t).is_ok(), "{t}: bounds included");
    }
}

#[test]
fn osd_refused_with_reason() {
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    for (t, want) in [
        ("[osd]\ntimeout_ms = 599", "osd.timeout_ms 599 out of range 600 to 5000 ms"),
        ("[osd]\ntimeout_ms = 5001", "osd.timeout_ms 5001"),
        ("[osd]\nwidth = 23", "osd.width 23 out of range 24 to 60 cells"),
        ("[osd]\nwidth = 61", "osd.width 61"),
        ("[osd]\nmargin = 401", "osd.margin 401 out of range 0 to 400 px"),
        ("[osd]\nbacklight = \"../intel_backlight\"", "osd.backlight"),
        ("[osd]\nbacklight = \"..\"", "osd.backlight"),
        ("[osd]\nbacklight = \"a\\u0007b\"", "osd.backlight"),
        ("[osd]\nbacklight_dir = \"sys/class/backlight\"", "osd.backlight_dir must be an absolute path"),
    ] {
        let e = err(t);
        assert!(e.contains(want), "{t}: {e}");
    }
    assert!(err("[osd]\nfade = true").contains("fade"), "unknown key refused");
    let (c, w) = BarConfig::load(&file("osd", "[osd]\nwidth = 5\n"));
    assert_eq!(c, BarConfig::default(), "defaults, as for any invalid bar.toml");
    assert!(w.is_some_and(|w| w.contains("bar.toml invalid") && w.contains("osd.width")));
}

/// Quick Wi-Fi spec § 4: `wifi_more` as given; absent, resolved at load time from the launcher's
/// terminal; an explicit empty list hides `More…` and is never replaced.
#[test]
fn wifi_more_is_read_checked_and_resolved() {
    assert_eq!(QuickConfig::default().wifi_more, None, "absent: resolved at load time");
    let c = BarConfig::from_text("[quick]\nwifi_more = [\"foot\", \"-e\", \"nmtui\"]\n").unwrap();
    assert_eq!(c.quick.wifi_more, Some(vec!["foot".to_string(), "-e".into(), "nmtui".into()]));
    let hidden = BarConfig::from_text("[quick]\nwifi_more = []\n").unwrap();
    assert_eq!(hidden.quick.wifi_more, Some(vec![]), "an explicit empty list");
    let mut q = QuickConfig::default();
    resolve_wifi_more(&mut q, &["alacritty".to_string(), "-e".into()]);
    assert_eq!(q.wifi_more, Some(vec!["alacritty".to_string(), "-e".into(), "nmtui".into()]));
    let mut kept = hidden.quick.clone();
    resolve_wifi_more(&mut kept, &["foot".to_string()]);
    assert_eq!(kept.wifi_more, Some(vec![]), "given: never replaced");
    for (t, want) in [
        ("[quick]\nwifi_more = [\" \"]", "quick.wifi_more: the program is blank"),
        ("[quick]\nwifi_more = [\"foot\", \"a\\u0000b\"]", "quick.wifi_more: an argument holds a NUL"),
    ] {
        let e = BarConfig::from_text(t).unwrap_err();
        assert!(e.contains(want), "{t}: {e}");
    }
    assert!(BarConfig::from_text("[quick]\nwifi_mor = []").unwrap_err().contains("wifi_mor"), "unknown key refused");
}

/// Quick VPN spec: `vpn_more` is read as `wifi_more` is, its default the launcher's terminal then
/// `nmtui-edit`.
#[test]
fn vpn_more_is_read_checked_and_resolved() {
    assert_eq!(QuickConfig::default().vpn_more, None, "absent: resolved at load time");
    let c = BarConfig::from_text("[quick]\nvpn_more = [\"foot\", \"-e\", \"nmtui-edit\"]\n").unwrap();
    assert_eq!(c.quick.vpn_more, Some(vec!["foot".to_string(), "-e".into(), "nmtui-edit".into()]));
    let hidden = BarConfig::from_text("[quick]\nvpn_more = []\n").unwrap();
    assert_eq!(hidden.quick.vpn_more, Some(vec![]), "an explicit empty list");
    let mut q = QuickConfig::default();
    resolve_vpn_more(&mut q, &["alacritty".to_string(), "-e".into()]);
    assert_eq!(q.vpn_more, Some(vec!["alacritty".to_string(), "-e".into(), "nmtui-edit".into()]));
    assert_eq!(q.wifi_more, None, "the other default is its own");
    let mut kept = hidden.quick.clone();
    resolve_vpn_more(&mut kept, &["foot".to_string()]);
    assert_eq!(kept.vpn_more, Some(vec![]), "given: never replaced");
    for (t, want) in [
        ("[quick]\nvpn_more = [\" \"]", "quick.vpn_more: the program is blank"),
        ("[quick]\nvpn_more = [\"foot\", \"a\\u0000b\"]", "quick.vpn_more: an argument holds a NUL"),
    ] {
        let e = BarConfig::from_text(t).unwrap_err();
        assert!(e.contains(want), "{t}: {e}");
    }
    assert!(BarConfig::from_text("[quick]\nvpn_mor = []").unwrap_err().contains("vpn_mor"), "unknown key refused");
}

/// `mixer` is read as `wifi_more` is: as given, absent resolved at load time from the launcher's
/// terminal then wiremix, `[]` hides the row and is never replaced.
#[test]
fn mixer_is_read_checked_and_resolved() {
    assert_eq!(QuickConfig::default().mixer, None, "absent: resolved at load time");
    let c = BarConfig::from_text("[quick]\nmixer = [\"foot\", \"-e\", \"wiremix\"]\n").unwrap();
    assert_eq!(c.quick.mixer, Some(vec!["foot".to_string(), "-e".into(), "wiremix".into()]));
    let hidden = BarConfig::from_text("[quick]\nmixer = []\n").unwrap();
    assert_eq!(hidden.quick.mixer, Some(vec![]));
    let mut q = QuickConfig::default();
    resolve_mixer(&mut q, &["alacritty".to_string(), "-e".into()]);
    assert_eq!(q.mixer, Some(vec!["alacritty".to_string(), "-e".into(), "wiremix".into()]));
    assert_eq!(q.wifi_more, None, "the other default is its own");
    let mut kept = hidden.quick.clone();
    resolve_mixer(&mut kept, &["foot".to_string()]);
    assert_eq!(kept.mixer, Some(vec![]), "given: never replaced");
    for (t, want) in [
        ("[quick]\nmixer = [\" \"]", "quick.mixer: the program is blank"),
        ("[quick]\nmixer = [\"foot\", \"a\\u0000b\"]", "quick.mixer: an argument holds a NUL"),
    ] {
        let e = BarConfig::from_text(t).unwrap_err();
        assert!(e.contains(want), "{t}: {e}");
    }
}

#[test]
fn quick_defaults_and_settings() {
    let d = BarConfig::default();
    assert!(d.modules.contains(&ModuleId::Quick), "on by default");
    assert_eq!(d.modules[d.modules.len() - 2..], [ModuleId::Clock, ModuleId::Power], "the clock, then the power button, last");
    assert_eq!(ModuleId::Quick.name(), "quick");
    let q = &d.quick;
    assert_eq!(q.step, 5);
    assert_eq!((&q.keep_awake, &q.night_light), (&ToggleConfig::default(), &ToggleConfig::default()), "no toggle: rows hidden");
    assert_eq!(q.lock, ["loginctl", "lock-session"]);
    assert!(q.logout.is_empty(), "no neutral logout: the button is hidden");
    assert_eq!(q.reboot, ["systemctl", "reboot"]);
    assert_eq!(q.poweroff, ["systemctl", "poweroff"]);
    assert_eq!(BarConfig::from_text("height = 22").unwrap().quick, QuickConfig::default(), "absent: defaults");
    let c = BarConfig::from_text(
        "[quick]\nstep = 10\n\
             keep_awake = { toggle = [\"tools-menu\", \"Système · Garder éveillé\"], state = \"file:$XDG_RUNTIME_DIR/garder-eveille\" }\n\
             night_light = { toggle = [\"fake-night\"], state = \"process:hyprsunset\" }\n\
             logout = [\"wm-set\", \"exit\"]\nlock = []\n",
    )
    .unwrap();
    assert_eq!(c.quick.step, 10);
    assert_eq!(c.quick.keep_awake.toggle, ["tools-menu", "Système · Garder éveillé"]);
    assert_eq!(c.quick.keep_awake.state, "file:$XDG_RUNTIME_DIR/garder-eveille");
    assert_eq!(
        (c.quick.night_light.toggle.as_slice(), c.quick.night_light.state.as_str()),
        (["fake-night".to_string()].as_slice(), "process:hyprsunset")
    );
    assert_eq!(c.quick.logout, ["wm-set", "exit"]);
    assert!(c.quick.lock.is_empty(), "an empty command hides its button");
    assert_eq!(c.quick.reboot, ["systemctl", "reboot"], "the others keep their defaults");
    for t in ["[quick]\nstep = 1", "[quick]\nstep = 25"] {
        assert!(BarConfig::from_text(t).is_ok(), "{t}: bounds included");
    }
}

#[test]
fn quick_refused_with_reason() {
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    for (t, want) in [
        ("[quick]\nstep = 0", "quick.step 0 out of range 1 to 25"),
        ("[quick]\nstep = 26", "quick.step 26"),
        ("[quick]\nlock = [\" \"]", "quick.lock: the program is blank"),
        ("[quick]\nkeep_awake = { toggle = [\"\"] }", "quick.keep_awake.toggle: the program is blank"),
        ("[quick]\nkeep_awake = { state = \"file:/run/x\" }", "quick.keep_awake.state without a toggle"),
        (
            "[quick]\nnight_light = { toggle = [\"x\"], state = \"pid:12\" }",
            "quick.night_light.state: expected \"\", \"file:<path>\" or \"process:<name>\"",
        ),
        (
            "[quick]\nnight_light = { toggle = [\"x\"], state = \"file:relative/x\" }",
            "quick.night_light.state: the file must be an absolute path",
        ),
        ("[quick]\nnight_light = { toggle = [\"x\"], state = \"file:\" }", "the file must be an absolute path"),
        ("[quick]\nnight_light = { toggle = [\"x\"], state = \"process:\" }", "quick.night_light.state: a process name is 1 to 15 bytes"),
        ("[quick]\nnight_light = { toggle = [\"x\"], state = \"process:a/b\" }", "quick.night_light.state: a process name has no '/'"),
        (
            "[quick]\nnight_light = { toggle = [\"x\"], state = \"file:/run/a\\nb\" }",
            "quick.night_light.state: the file path holds a control character: \"/run/a\\nb\"",
        ),
        ("[quick]\nlogout = [\"fake-logout\", \"a\\u0000b\"]", "quick.logout: an argument holds a NUL"),
        ("[quick]\nkeep_awake = { toggle = [\"x\\u0000\"] }", "quick.keep_awake.toggle: an argument holds a NUL"),
    ] {
        let e = err(t);
        assert!(e.contains(want), "{t}: {e}");
    }
    assert!(BarConfig::from_text("[quick]\nstep = 300").is_err(), "not a u8");
    assert!(err("[quick]\nsuspend = []").contains("suspend"), "unknown key refused: nothing sleeps from here");
    assert!(err("[quick]\nkeep_awake = { toggle = [\"x\"], every = 2 }").contains("every"), "unknown key refused");
    let (c, w) = BarConfig::load(&file("quick", "[quick]\nstep = 0\n"));
    assert_eq!(c, BarConfig::default(), "defaults, as for any invalid bar.toml");
    assert!(w.is_some_and(|w| w.contains("bar.toml invalid") && w.contains("quick.step")));
}

/// The kernel keeps 15 bytes of a process name (`/proc/<pid>/comm`):
/// "gammastep-indicator" reads "gammastep-indic" there, so a longer name could never match.
#[test]
fn a_process_state_longer_than_comm_is_refused() {
    let state = |name: &str| format!("[quick]\nnight_light = {{ toggle = [\"x\"], state = \"process:{name}\" }}\n");
    let e = BarConfig::from_text(&state("gammastep-indicator")).unwrap_err();
    assert!(e.contains("1 to 15 bytes") && e.contains("gammastep-indicator"), "{e}");
    assert!(BarConfig::from_text(&state("abcdefghijklmnop")).is_err(), "16 bytes");
    assert!(BarConfig::from_text(&state("gammastep-indic")).is_ok(), "15 bytes");
}

#[test]
fn state_forms_are_read() {
    assert_eq!(state_form(""), Ok(StateForm::Unknown));
    assert_eq!(state_form("file:/run/user/1000/flag"), Ok(StateForm::File("/run/user/1000/flag".into())));
    assert_eq!(state_form("file:~/.cache/flag"), Ok(StateForm::File("~/.cache/flag".into())));
    assert_eq!(state_form("file:$XDG_RUNTIME_DIR/garder-eveille"), Ok(StateForm::File("$XDG_RUNTIME_DIR/garder-eveille".into())));
    assert_eq!(state_form("process:hyprsunset"), Ok(StateForm::Process("hyprsunset".into())));
    assert!(state_form("file:$HOME/x").is_err(), "only $XDG_RUNTIME_DIR and ~ are expanded");
    assert!(state_form("File:/x").is_err(), "the prefixes are exact");
    assert!(state_form("process:a\u{7}").is_err(), "no control character");
}

/// Displays spec § 1: `displays` is a module with no segment, on by default; `[displays]` turns
/// the hotplug profiles off and says where the DRM connectors are listed (absolute).
#[test]
fn config_displays_defaults_and_absolute_drm_dir() {
    let d = BarConfig::default();
    assert!(d.modules.contains(&ModuleId::Displays), "on by default");
    assert_eq!(ModuleId::Displays.name(), "displays");
    assert_eq!(d.displays, DisplaysConfig { auto: true, drm_dir: PathBuf::from("/sys/class/drm") });
    assert_eq!(BarConfig::from_text("height = 22").unwrap().displays, DisplaysConfig::default(), "absent: defaults");
    let c = BarConfig::from_text("[displays]\nauto = false\ndrm_dir = \"/tmp/drm\"\n").unwrap();
    assert_eq!(c.displays, DisplaysConfig { auto: false, drm_dir: PathBuf::from("/tmp/drm") });
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    for t in ["[displays]\ndrm_dir = \"sys/class/drm\"", "[displays]\ndrm_dir = \"\""] {
        assert!(err(t).contains("displays.drm_dir must be an absolute path"), "{t}");
    }
    assert!(err("[displays]\npower = true").contains("power"), "unknown key refused");
    assert_eq!(BarConfig::from_text("modules = [\"displays\"]").unwrap().modules, [ModuleId::Displays]);
}

#[test]
fn resources_graph_is_accepted_and_ignored() {
    let c = BarConfig::from_text("[resources]\ngraph = 20\n").unwrap();
    assert_eq!(c.resources.graph, 20, "still parsed");
    assert!(BarConfig::from_text("[resources]\ngraph = 61").unwrap_err().contains("resources.graph"), "still refused out of range");
}

#[test]
fn dnd_while_sharing_defaults_off_and_reads_true() {
    assert!(!BarConfig::default().notifications.dnd_while_sharing);
    let c = BarConfig::from_text("[notifications]\ndnd_while_sharing = true\n").unwrap();
    assert!(c.notifications.dnd_while_sharing);
    assert_eq!(c.notifications.history, 50, "the rest stays default");
}

#[test]
fn notification_rules_parse() {
    let c = BarConfig::from_text(
        "[[notifications.rules]]\napp = \"Chatty\"\naction = \"mute\"\n\
         [[notifications.rules]]\napp = \"Pager\"\nurgency = \"critical\"\n\
         [[notifications.rules]]\napp = \"Slow\"\naction = \"mute\"\nurgency = \"low\"\n",
    )
    .unwrap();
    let rule = |app: &str, action, urgency| NotifyRule { app: app.into(), action, urgency };
    assert_eq!(
        c.notifications.rules,
        vec![
            rule("Chatty", Some(RuleAction::Mute), None),
            rule("Pager", None, Some(RuleUrgency::Critical)),
            rule("Slow", Some(RuleAction::Mute), Some(RuleUrgency::Low)),
        ]
    );
    assert!(BarConfig::default().notifications.rules.is_empty());
}

#[test]
fn notification_rules_are_validated() {
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    assert!(err("[[notifications.rules]]\napp = \"\"\naction = \"mute\"\n").contains("notifications.rules[1]: app empty"));
    assert!(
        err("[[notifications.rules]]\napp = \"A\"\naction = \"mute\"\n[[notifications.rules]]\napp = \"B\"\n")
            .contains("notifications.rules[2]: needs action or urgency")
    );
    assert!(
        err("[[notifications.rules]]\napp = \"A\"\naction = \"drop\"\nurgency = \"low\"\n")
            .contains("notifications.rules[1]: drop takes no urgency")
    );
    assert!(BarConfig::from_text("[[notifications.rules]]\napp = \"A\"\naction = \"silence\"\n").is_err());
    assert!(BarConfig::from_text("[[notifications.rules]]\nname = \"A\"\naction = \"mute\"\n").is_err());
    let many = |n: usize| "[[notifications.rules]]\napp = \"A\"\naction = \"mute\"\n".repeat(n);
    assert_eq!(BarConfig::from_text(&many(32)).unwrap().notifications.rules.len(), 32);
    assert!(err(&many(33)).contains("notifications.rules: at most 32 (got 33)"));
}

#[test]
fn notification_rule_apps_are_trimmed_and_bounded() {
    let rule = |app: &str| format!("[[notifications.rules]]\napp = \"{app}\"\naction = \"mute\"\n");
    assert_eq!(BarConfig::from_text(&rule("  Chatty ")).unwrap().notifications.rules[0].app, "Chatty");
    assert!(BarConfig::from_text(&rule("   ")).unwrap_err().contains("rules[1]: app empty"));
    assert!(BarConfig::from_text(&rule(&"a".repeat(64))).is_ok());
    assert!(BarConfig::from_text(&rule(&"a".repeat(65))).unwrap_err().contains("rules[1]: app too long"));
    assert!(BarConfig::from_text(&rule("a\\u0007b")).unwrap_err().contains("control characters"));
}

#[test]
fn battery_warn_defaults_dedups_and_sorts() {
    assert_eq!(BarConfig::default().battery.warn, [20, 10, 5]);
    assert_eq!(BarConfig::from_text("").unwrap().battery.warn, [20, 10, 5]);
    assert!(BarConfig::from_text("[battery]\nwarn = []").unwrap().battery.warn.is_empty());
    assert_eq!(BarConfig::from_text("[battery]\nwarn = [5, 20, 10, 20]").unwrap().battery.warn, [20, 10, 5]);
    for bad in [0, 100] {
        assert_eq!(
            BarConfig::from_text(&format!("[battery]\nwarn = [{bad}]")).unwrap_err(),
            format!("battery.warn: {bad} out of range 1 to 99")
        );
    }
}

#[test]
fn quick_dnd_until() {
    assert_eq!(QuickConfig::default().dnd_until_hm(), (8, 0));
    let unchecked = QuickConfig { dnd_until: "nonsense".into(), ..QuickConfig::default() };
    assert_eq!(unchecked.dnd_until_hm(), (8, 0), "a text the check would refuse falls back to 8:00");
    let other = QuickConfig { dnd_until: "21:15".into(), ..QuickConfig::default() };
    assert_eq!(other.dnd_until_hm(), (21, 15));
    let c = BarConfig::from_text("[quick]\ndnd_until = \"7:30\"\n").unwrap();
    assert_eq!(c.quick.dnd_until_hm(), (7, 30));
    for bad in ["24:00", "8h", ""] {
        let e = BarConfig::from_text(&format!("[quick]\ndnd_until = \"{bad}\"\n")).unwrap_err();
        assert!(e.contains("quick.dnd_until"), "{bad:?}: {e}");
    }
}

#[test]
fn hooks_default_to_none() {
    let d = BarConfig::default();
    assert!(d.hooks.is_empty());
    assert_eq!(d.hooks, HooksConfig::default());
    assert!(BarConfig::from_text("").unwrap().hooks.is_empty());
}

#[test]
fn hooks_are_read_in_order() {
    let keys = [
        "started",
        "theme_mode",
        "dnd",
        "battery_charge",
        "battery_level",
        "power_profile",
        "network",
        "vpn",
        "bluetooth",
        "privacy",
        "displays",
    ];
    let text = format!("[hooks]\n{}", keys.iter().map(|k| format!("{k} = [\"hook-{k}\"]\n")).collect::<String>());
    let c = BarConfig::from_text(&text).unwrap();
    assert!(!c.hooks.is_empty());
    let got: Vec<(&str, Vec<String>)> = c.hooks.commands().iter().map(|(k, v)| (*k, v.to_vec())).collect();
    let want: Vec<(&str, Vec<String>)> = keys.iter().map(|k| (*k, vec![format!("hook-{k}")])).collect();
    assert_eq!(got, want);
    assert!(BarConfig::from_text("[hooks]\n").unwrap().hooks.is_empty());
}

#[test]
fn hooks_check_their_commands() {
    let e = BarConfig::from_text("[hooks]\ndnd = [\" \"]").unwrap_err();
    assert!(e.contains("hooks.dnd: the program is blank"), "{e}");
    let e = BarConfig::from_text("[hooks]\nvpn = [\"x\", \"a\\u0000b\"]").unwrap_err();
    assert!(e.contains("hooks.vpn: an argument holds a NUL"), "{e}");
    assert!(BarConfig::from_text("[hooks]\nwifi = [\"x\"]").is_err(), "unknown field");
    assert!(BarConfig::from_text("[hooks]\ndnd = []").is_ok());
}

#[test]
fn timer_defaults_and_bounds() {
    let d = BarConfig::default().timer;
    assert_eq!((d.countdown, d.work, d.short_break, d.long_break, d.rounds), (10, 25, 5, 15, 4));
    assert!(d.sound.is_empty());
    assert_eq!(d.idle, TimerIdle::Glyph);
    let c = BarConfig::from_text("[timer]\nwork = 50\nidle = \"hidden\"\n").unwrap();
    assert_eq!((c.timer.work, c.timer.idle), (50, TimerIdle::Hidden));
    assert_eq!(c.timer.countdown, 10, "the rest keeps its default");
    let ok = BarConfig::from_text("[timer]\ncountdown = 1\nwork = 180\nshort_break = 1\nlong_break = 180\nrounds = 12\n");
    assert!(ok.is_ok(), "bounds included: {ok:?}");
    let err = |t: &str| BarConfig::from_text(t).unwrap_err();
    for (text, key) in [
        ("[timer]\ncountdown = 0", "timer.countdown"),
        ("[timer]\nwork = 181", "timer.work"),
        ("[timer]\nshort_break = 0", "timer.short_break"),
        ("[timer]\nlong_break = 0", "timer.long_break"),
        ("[timer]\nrounds = 13", "timer.rounds"),
        ("[timer]\nrounds = 0", "timer.rounds"),
        ("[timer]\nsound = [\"\"]", "timer.sound"),
        ("[timer]\nsound = [\"a\\u0000b\"]", "timer.sound"),
    ] {
        assert!(err(text).contains(key), "{text}: {}", err(text));
    }
    assert!(err("[timer]\nidle = \"dim\"").contains("unknown variant"));
    assert!(err("[timer]\nbeep = true").contains("beep"), "unknown key refused");
}

#[test]
fn the_timer_module_sits_just_before_the_clock() {
    let at = |m| ModuleId::ALL.iter().position(|x| *x == m).unwrap();
    assert_eq!(at(ModuleId::Timer) + 1, at(ModuleId::Clock));
    assert_eq!(ModuleId::Timer.name(), "timer");
    assert!(BarConfig::default().modules.contains(&ModuleId::Timer));
}
