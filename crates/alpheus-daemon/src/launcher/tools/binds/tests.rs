//! Tests of the binds parser: Mango's chain of `source=` files, groups, descriptions, `env=`
//! variables, combos, and every bound; Sway's `set` variables, modes, flags and include globs. Files come from a `FakeFiles` map; no real file is read.

use super::*;
use std::cell::RefCell;
use std::collections::HashMap;

/// A map of path → text standing for the disk, counting the reads of each path.
#[derive(Default)]
struct FakeFiles {
    texts: HashMap<PathBuf, String>,
    reads: RefCell<HashMap<PathBuf, usize>>,
    lists: std::cell::Cell<usize>,
}

impl FakeFiles {
    fn with(files: &[(&str, &str)]) -> FakeFiles {
        let mut f = FakeFiles::default();
        for (p, t) in files {
            f.texts.insert(PathBuf::from(p), t.to_string());
        }
        f
    }

    fn reads(&self, path: &str) -> usize {
        self.reads.borrow().get(Path::new(path)).copied().unwrap_or(0)
    }

    fn total_reads(&self) -> usize {
        self.reads.borrow().values().sum()
    }
}

impl Files for FakeFiles {
    fn read(&self, path: &Path, max: usize) -> Option<String> {
        *self.reads.borrow_mut().entry(path.to_path_buf()).or_default() += 1;
        let text = self.texts.get(path)?;
        let mut end = text.len().min(max);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Some(text[..end].to_string())
    }

    fn list(&self, dir: &Path) -> Vec<PathBuf> {
        self.lists.set(self.lists.get() + 1);
        let mut found: Vec<PathBuf> = self.texts.keys().filter(|p| p.parent() == Some(dir)).cloned().collect();
        found.sort();
        found
    }
}

fn mango_fixture() -> FakeFiles {
    FakeFiles::with(&[
        ("/cfg/mango/config.conf", include_str!("fixtures/mango/config.conf")),
        ("/cfg/mango/keys.conf", include_str!("fixtures/mango/keys.conf")),
        ("/cfg/mango/extra.conf", include_str!("fixtures/mango/extra.conf")),
    ])
}

fn first_of(path: &str) -> Roots {
    Roots::FirstOf(vec![PathBuf::from(path)])
}

/// One file `/c/config.conf` holding `text`, loaded as Mango.
fn mango(text: &str) -> Binds {
    load(Dialect::Mango, &first_of("/c/config.conf"), &FakeFiles::with(&[("/c/config.conf", text)]))
}

fn vars(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
    move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(*v))
}

#[test]
fn binds_mango_parses_bind_bindl_bindsl_and_source_chain() {
    let files = mango_fixture();
    let b = load(Dialect::Mango, &first_of("/cfg/mango/config.conf"), &files);
    assert!(b.found);
    assert_eq!(b.files, ["config.conf", "keys.conf", "extra.conf"]);
    let combos: Vec<&str> = b.binds.iter().map(|x| x.combo.as_str()).collect();
    assert_eq!(
        combos,
        [
            "Super+Return",
            "Super+B",
            "Super+Q",
            "Super+Shift+F",
            "Alt+Tab",
            "Super+code:38",
            "Super+Shift+2",
            "Ctrl+Alt+Left",
            "XF86AudioMute",
            "XF86AudioPlay",
            "Super+SUPER_L",
            "Super+Shift+R",
        ]
    );
    let from: Vec<usize> = b.binds.iter().map(|x| x.file).collect();
    assert_eq!(from, [0, 0, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2]);
    assert_eq!(b.binds[8].command, "spawn,mute-toggle", "bindl kept");
    assert_eq!(b.binds[9].command, "spawn,player-toggle", "bindsl kept");
    assert_eq!(b.binds[10].command, "toggleoverview", "bindr kept");
    assert_eq!(b.not_understood, 0, "mousebind, axisbind and gesturebind are skipped silently");
    assert_eq!(b.cut, Cut::default());
    for p in ["/cfg/mango/config.conf", "/cfg/mango/keys.conf", "/cfg/mango/extra.conf"] {
        assert_eq!(files.reads(p), 1, "{p}");
    }
}

#[test]
fn binds_section_comments_become_groups() {
    let b = load(Dialect::Mango, &first_of("/cfg/mango/config.conf"), &mango_fixture());
    assert_eq!(b.groups, ["General", "Windows", "Desktops"]);
    let groups: Vec<usize> = b.binds.iter().map(|x| x.group).collect();
    assert_eq!(groups, [0, 0, 1, 1, 1, 2, 2, 2, 0, 0, 0, 0], "a file without a header is General");
    // A header with no bind under it opens no group; one seen again reuses its group.
    let b = mango(
        "# ── Empty ──\n# ── Media ──\nbind=SUPER,m,spawn,a\n# ── Other ──\nbind=SUPER,o,spawn,b\n# ── Media ──\nbind=SUPER,n,spawn,c\n",
    );
    assert_eq!(b.groups, ["Media", "Other"]);
    let groups: Vec<usize> = b.binds.iter().map(|x| x.group).collect();
    assert_eq!(groups, [0, 1, 0]);
}

#[test]
fn binds_description_prefers_trailing_then_above_then_humanised() {
    let b = load(Dialect::Mango, &first_of("/cfg/mango/config.conf"), &mango_fixture());
    assert_eq!(b.binds[0].description, "open term", "the trailing comment");
    assert_eq!(b.binds[0].command, "spawn,foo-term", "the comment is not part of the command");
    assert_eq!(b.binds[2].description, "Close the focused window", "the comment line right above");
    assert_eq!(b.binds[3].description, "Toggle fullscreen", "the line above is a bind, not a comment");
    assert_eq!(b.binds[5].description, "Go to desktop 2", "a header is no description");

    let b = mango(
        "# above\nbind=SUPER,a,spawn,x # trailing\n\
         # not across a blank\n\nbind=SUPER,b,spawn,browser\n\
         # ── Header ──\nbind=SUPER,c,view,2\n\
         bind=SUPER,d,killclient\n\
         bind=SUPER,e,fancyfn,x\n\
         bind=SUPER,f,fancyfn\n",
    );
    let descriptions: Vec<&str> = b.binds.iter().map(|x| x.description.as_str()).collect();
    assert_eq!(descriptions, ["trailing", "Run browser", "Go to desktop 2", "Close window", "fancyfn x", "fancyfn"]);
}

#[test]
fn env_variables_of_the_chain_expand_others_stay() {
    let files = FakeFiles::with(&[
        ("/c/config.conf", "env=TERMINAL,foo-term\nsource=./keys.conf\n"),
        ("/c/keys.conf", "bind=SUPER,Return,spawn,$TERMINAL\nbind=SUPER,x,spawn,$UNSET -e ${TERMINAL}\n"),
    ]);
    let b = load(Dialect::Mango, &first_of("/c/config.conf"), &files);
    assert_eq!(b.binds[0].command, "spawn,foo-term");
    assert_eq!(b.binds[0].description, "Run foo-term");
    assert_eq!(b.binds[1].command, "spawn,$UNSET -e foo-term");
}

#[test]
fn env_variables_are_bounded() {
    // 70 definitions: the first MAX_VARS kept, the others ignored and counted; a redefinition of a
    // kept one still replaces it.
    let mut text: String = (0..70).map(|i| format!("env=a{i},v{i}\n")).collect();
    text.push_str("env=a0,again\nbind=SUPER,x,spawn,$a0 $a63 $a64 $a69\n");
    let b = mango(&text);
    assert_eq!(b.binds[0].command, "spawn,again v63 $a64 $a69");
    assert_eq!(b.cut.vars, 70 - MAX_VARS);

    // A name or a value past MAX_VAR bytes is ignored and counted.
    let text = format!("env=n,{}\nenv={},v\nbind=SUPER,x,spawn,$n\n", "x".repeat(MAX_VAR + 1), "N".repeat(MAX_VAR + 1));
    let b = mango(&text);
    assert_eq!(b.binds[0].command, "spawn,$n");
    assert_eq!(b.cut.vars, 2);
}

#[test]
fn control_characters_never_reach_the_screen() {
    let text = "# ── G\u{1b}[31mx\u{1b}]0;t\u{7} ──\nbind=SUPER,a\u{7},spawn,foo\u{1b}[2J\tbar # do\u{202e}it\u{8}\n";
    let b = mango(text);
    let bind = &b.binds[0];
    for s in [bind.combo.as_str(), bind.description.as_str(), bind.command.as_str(), b.groups[0].as_str()] {
        assert!(!s.chars().any(|c| c.is_control() || alpheus_common::text::invisible(c)), "{s:?}");
    }
    assert_eq!(bind.command, "spawn,foo bar");
    assert_eq!(bind.description, "doit");
    assert_eq!(b.groups, ["Gx"]);
}

#[test]
fn combos_print_in_title_case() {
    let m = Dialect::Mango;
    assert_eq!(describe::combo(m, "SUPER+SHIFT", "a"), "Super+Shift+A");
    assert_eq!(describe::combo(m, "NONE", "XF86AudioMute"), "XF86AudioMute");
    assert_eq!(describe::combo(m, "ALT", "Tab"), "Alt+Tab");
    assert_eq!(describe::combo(m, "CTRL", "Return"), "Ctrl+Return");
    assert_eq!(describe::combo(m, "super+ctrl+alt", "é"), "Super+Ctrl+Alt+É");
    assert_eq!(describe::combo(m, "", "F1"), "F1");
}

#[test]
fn the_humanised_table_covers_mango_functions() {
    let m = Dialect::Mango;
    for (function, args, want) in [
        ("spawn", "firefox", "Run firefox"),
        ("spawn_shell", "foo | bar", "Run foo | bar"),
        ("view", "2", "Go to desktop 2"),
        ("tag", "3", "Move window to desktop 3"),
        ("tagsilent", "4", "Send window to desktop 4"),
        ("focusdir", "left", "Focus left"),
        ("focusmon", "right", "Focus monitor right"),
        ("tagmon", "left", "Move window to monitor left"),
        ("exchange_client", "up", "Swap window up"),
        ("resizewin", "+0,-50", "Resize window"),
        ("killclient", "", "Close window"),
        ("togglefullscreen", "", "Toggle fullscreen"),
        ("togglemaximizescreen", "", "Toggle maximized"),
        ("togglefloating", "", "Toggle floating"),
        ("toggleoverview", "", "Toggle overview"),
        ("switch_layout", "", "Switch layout"),
        ("viewtoleft", "", "Go to the desktop on the left"),
        ("viewtoright", "", "Go to the desktop on the right"),
        ("tagtoleft", "", "Move window to the desktop on the left"),
        ("tagtoright", "", "Move window to the desktop on the right"),
        ("reload_config", "", "Reload configuration"),
        ("quit", "", "Quit Mango"),
        ("fancyfn", "x,y", "fancyfn x,y"),
    ] {
        assert_eq!(describe::humanise(m, function, args), want, "{function}");
    }
}

#[test]
fn a_line_not_understood_is_counted() {
    let b = mango("bind=SUPER\nbind=SUPER,a\nbind=SUPER,,spawn\nbind=SUPER,b,spawn,ok\nother=thing\nnot a key line\n");
    assert_eq!(b.not_understood, 3, "a bind short of a key or a function");
    assert_eq!(b.binds.len(), 1);
}

#[test]
fn binds_include_cycle_and_depth_are_bounded() {
    // A cycle: a sources b, b sources a (and itself): each is read once.
    let files = FakeFiles::with(&[
        ("/c/a.conf", "source=./b.conf\nbind=SUPER,a,spawn,x\n"),
        ("/c/b.conf", "source=./a.conf\nsource=b.conf\nsource=/c/./a.conf\nbind=SUPER,b,spawn,y\n"),
    ]);
    let b = load(Dialect::Mango, &first_of("/c/a.conf"), &files);
    assert_eq!((files.reads("/c/a.conf"), files.reads("/c/b.conf"), files.total_reads()), (1, 1, 2));
    assert_eq!(b.binds.len(), 2);
    assert_eq!(b.cut, Cut::default());

    // Ten files in a chain: eight read; the ninth is the one source line past the bound (the
    // tenth is never seen).
    let texts: Vec<(String, String)> =
        (1..=10).map(|i| (format!("/c/f{i}.conf"), format!("source=./f{}.conf\nbind=SUPER,{i},spawn,x\n", i + 1))).collect();
    let pairs: Vec<(&str, &str)> = texts.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let files = FakeFiles::with(&pairs);
    let b = load(Dialect::Mango, &first_of("/c/f1.conf"), &files);
    assert_eq!((files.total_reads(), b.files.len(), b.binds.len(), b.cut.files), (8, 8, 8, 1));

    // Ten files, the first sourcing the nine others: eight read, two cut.
    let mut root = String::new();
    for i in 2..=10 {
        root.push_str(&format!("source=./f{i}.conf\n"));
    }
    let mut texts = texts;
    for (i, t) in texts.iter_mut().enumerate() {
        t.1 = format!("bind=SUPER,{i},spawn,x\n");
    }
    texts[0].1 = root;
    let pairs: Vec<(&str, &str)> = texts.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let files = FakeFiles::with(&pairs);
    let b = load(Dialect::Mango, &first_of("/c/f1.conf"), &files);
    assert_eq!((files.total_reads(), b.cut.files), (MAX_FILES, 2));

    // 1,200 binds: the first 1,000 kept.
    let many: String = (0..1200).map(|i| format!("bind=SUPER,k{i},spawn,x\n")).collect();
    let b = mango(&many);
    assert_eq!((b.binds.len(), b.cut.binds), (MAX_BINDS, 200));
    assert_eq!(b.binds.last().map(|x| x.combo.as_str()), Some("Super+k999"));

    // 40 groups: the first 32 kept, the binds of the others stay in the group before.
    let many: String = (0..40).map(|i| format!("# ── G{i} ──\nbind=SUPER,k{i},spawn,x\n")).collect();
    let b = mango(&many);
    assert_eq!((b.groups.len(), b.cut.groups, b.binds.len()), (MAX_GROUPS, 8, 40));
    assert_eq!(b.binds.last().map(|x| x.group), Some(MAX_GROUPS - 1));

    // A 2 KiB line is cut at 1 KiB: the bind is kept with its command cut.
    let long = format!("bind=SUPER,a,spawn,{}\nbind=SUPER,b,spawn,y\n", "x".repeat(2048));
    let b = mango(&long);
    assert_eq!((b.binds.len(), b.cut.lines), (2, 1));
    assert_eq!(b.binds[0].command.len(), MAX_LINE - "bind=SUPER,a,".len());

    // 600 KiB in two files: the reads stop at 512 KiB, the partial last line dropped.
    let line = format!("bind=SUPER,a,spawn,{}\n", "x".repeat(99)); // 120 bytes
    let big = line.repeat(300 * 1024 / line.len());
    let files = FakeFiles::with(&[("/c/a.conf", &format!("source=./b.conf\n{big}")), ("/c/b.conf", big.as_str())]);
    let b = load(Dialect::Mango, &first_of("/c/a.conf"), &files);
    assert_eq!(b.cut.bytes, 1);
    assert_eq!(b.binds.len(), MAX_BINDS);
    let read: usize = b.binds.len() + b.cut.binds;
    assert!(read * line.len() <= MAX_BYTES && (read + 1) * line.len() + 16 > MAX_BYTES, "{read} lines read");
}

#[test]
fn no_root_readable_means_not_found() {
    let files = FakeFiles::with(&[("/elsewhere/config.conf", "bind=SUPER,a,spawn,x\n")]);
    let b = load(Dialect::Mango, &Roots::FirstOf(vec![PathBuf::from("/x/a.conf"), PathBuf::from("/x/b.conf")]), &files);
    assert!(!b.found);
    assert!(b.binds.is_empty() && b.files.is_empty());
    let b = load(Dialect::Mango, &Roots::Each(vec![PathBuf::from("/x/a.conf")]), &files);
    assert!(!b.found);
    let b = load(Dialect::Mango, &Roots::FirstOf(Vec::new()), &files);
    assert!(!b.found);
}

#[test]
fn first_of_reads_the_first_readable_and_each_reads_all() {
    let files = FakeFiles::with(&[("/x/b.conf", "bind=SUPER,b,spawn,x\n"), ("/x/c.conf", "bind=SUPER,c,spawn,x\n")]);
    let names = ["/x/a.conf", "/x/b.conf", "/x/c.conf"].map(PathBuf::from).to_vec();
    let b = load(Dialect::Mango, &Roots::FirstOf(names.clone()), &files);
    assert!(b.found);
    assert_eq!(b.files, ["b.conf"]);
    let b = load(Dialect::Mango, &Roots::Each(names), &files);
    assert_eq!(b.files, ["b.conf", "c.conf"]);
    assert_eq!(b.binds.len(), 2);
}

#[test]
fn roots_follow_xdg_then_home() {
    assert_eq!(
        roots(Dialect::Mango, vars(&[("XDG_CONFIG_HOME", "/x"), ("HOME", "/h")]), &[]),
        Roots::FirstOf(vec![PathBuf::from("/x/mango/config.conf")])
    );
    assert_eq!(roots(Dialect::Mango, vars(&[("HOME", "/h")]), &[]), Roots::FirstOf(vec![PathBuf::from("/h/.config/mango/config.conf")]));
    assert_eq!(
        roots(Dialect::Mango, vars(&[("XDG_CONFIG_HOME", "rel"), ("HOME", "/h")]), &[]),
        Roots::FirstOf(vec![PathBuf::from("/h/.config/mango/config.conf")]),
        "a relative XDG_CONFIG_HOME is ignored"
    );
    assert_eq!(roots(Dialect::Mango, vars(&[]), &[]), Roots::FirstOf(Vec::new()));
    assert_eq!(
        roots(Dialect::Mango, vars(&[("HOME", "/h")]), &["~/k.conf".to_string(), "/abs/m.conf".to_string()]),
        Roots::Each(vec![PathBuf::from("/h/k.conf"), PathBuf::from("/abs/m.conf")])
    );
}

/// Three groups, the second split across two files: binds in file order 0..=5.
fn grouped() -> Binds {
    let files = FakeFiles::with(&[
        (
            "/c/config.conf",
            "source=./more.conf\nbind=SUPER,a,spawn,foo-term # Open term\n# ── Windows ──\nbind=SUPER,q,killclient\n# ── Desktops ──\nbind=SUPER,1,view,1\n",
        ),
        (
            "/c/more.conf",
            "bind=SUPER,b,spawn,browser\n# ── Windows ──\nbind=SUPER,f,togglefullscreen\nbind=SUPER,t,spawn,term-two # Second term\n",
        ),
    ]);
    load(Dialect::Mango, &first_of("/c/config.conf"), &files)
}

#[test]
fn open_binds_lists_all_then_a_group_tab() {
    let b = grouped();
    assert_eq!(b.groups, ["General", "Windows", "Desktops"]);
    let groups: Vec<usize> = b.binds.iter().map(|x| x.group).collect();
    assert_eq!(groups, [0, 1, 2, 0, 1, 1], "file order: config.conf, then more.conf");
    use PanelRow::{Section, Tool};
    assert_eq!(
        rows(&b, 0),
        [
            Section(Label::Group(0)),
            Tool(0),
            Tool(3),
            Section(Label::Group(1)),
            Tool(1),
            Tool(4),
            Tool(5),
            Section(Label::Group(2)),
            Tool(2)
        ],
        "All: every group in group order, its binds in file order"
    );
    assert_eq!(rows(&b, 2), [Tool(1), Tool(4), Tool(5)], "tab 2 is group 1, with no section");
    assert!(rows(&b, 9).is_empty(), "no such tab");

    // A group with no bind left (none here once a header past the bound) shows no section.
    let mut b = b;
    b.groups.push("Empty".into());
    assert!(!rows(&b, 0).contains(&Section(Label::Group(3))));
    assert!(rows(&Binds::default(), 0).is_empty());
}

#[test]
fn typing_groups_the_results_by_their_best_hit() {
    let b = grouped();
    let rows: Vec<ToolRow> = (0..b.binds.len())
        .map(|i| ToolRow { id: RowId::Bind(i), icon: None, text: String::new(), trail: crate::launcher::tools::nav::Trail::None })
        .collect();
    let hit = |i: usize| Hit { source: Source::Tool(i), score: 0, frecency: 0 };
    // Ranked: group 1's bind 5, group 0's bind 0, group 1's bind 4, group 2's bind 2.
    let got = group_hits(&[hit(5), hit(0), hit(4), hit(2)], &rows, &b);
    assert_eq!(got, [hit(5), hit(4), hit(0), hit(2)], "Windows first (its best hit), its hits in rank order, then General, then Desktops");
    assert!(group_hits(&[], &rows, &b).is_empty());
}

#[test]
fn a_tilde_source_is_left_to_the_glue() {
    let files = FakeFiles::with(&[("/c/config.conf", "source=~/keys.conf\nsource=./here.conf\n")]);
    load(Dialect::Mango, &first_of("/c/config.conf"), &files);
    assert_eq!(files.reads("~/keys.conf"), 1, "kept as written: the glue's `Files` knows the home folder");
    assert_eq!(files.reads("/c/~/keys.conf"), 0);
    assert_eq!(files.reads("/c/here.conf"), 1);
}

#[test]
fn the_notes_are_fixed_words_and_counts() {
    let mut said = Vec::new();
    notes(&Binds::default(), &mut |m| said.push(m.to_string()));
    assert_eq!(said, ["tools: binds no source"]);

    let secret = "SECRET";
    let b = Binds {
        groups: vec![format!("{secret}-group")],
        binds: vec![Bind {
            combo: format!("{secret}+A"),
            description: secret.into(),
            command: format!("spawn,{secret}"),
            group: 0,
            file: 0,
        }],
        files: vec![format!("{secret}.conf")],
        not_understood: 3,
        found: true,
        cut: Cut { files: 1, bytes: 2, binds: 3, groups: 4, lines: 5, vars: 6 },
    };
    let mut said = Vec::new();
    notes(&b, &mut |m| said.push(m.to_string()));
    assert_eq!(
        said,
        [
            "tools: binds shown (1 binds, 1 groups)",
            "tools: binds 3 lines not understood",
            "tools: binds cut (files 1, bytes 2, binds 3, groups 4, lines 5, vars 6)",
        ]
    );
    assert!(said.iter().all(|l| !l.contains(secret)));
    let mut said = Vec::new();
    notes(&Binds { not_understood: 0, cut: Cut::default(), ..b }, &mut |m| said.push(m.to_string()));
    assert_eq!(said.len(), 1, "nothing skipped, nothing cut: one line");
}

fn sway_fixture() -> FakeFiles {
    FakeFiles::with(&[
        ("/cfg/sway/config", include_str!("fixtures/sway/config")),
        ("/cfg/sway/config.d/20-system.conf", include_str!("fixtures/sway/config.d/20-system.conf")),
        ("/cfg/sway/config.d/10-workspaces.conf", include_str!("fixtures/sway/config.d/10-workspaces.conf")),
    ])
}

/// One file `/c/config` holding `text`, loaded as Sway.
fn sway(text: &str) -> Binds {
    load(Dialect::Sway, &first_of("/c/config"), &FakeFiles::with(&[("/c/config", text)]))
}

#[test]
fn binds_sway_expands_set_variables_and_modes() {
    let files = sway_fixture();
    let b = load(Dialect::Sway, &first_of("/cfg/sway/config"), &files);
    assert!(b.found);
    assert_eq!(b.files, ["config", "10-workspaces.conf", "20-system.conf"], "the glob's files sorted");
    let got: Vec<(&str, &str, &str)> = b.binds.iter().map(|x| (x.combo.as_str(), x.description.as_str(), x.command.as_str())).collect();
    assert_eq!(
        got,
        [
            ("Super+Return", "Run foo-term", "exec foo-term"),
            ("Super+Shift+Q", "Close window", "kill"),
            ("Alt+Ctrl+T", "Second terminal", "exec foo-term --hold"),
            ("Super+H", "Focus the window on the left", "focus left"),
            ("Super+Shift+space", "Toggle floating", "floating toggle"),
            ("H", "Resize shrink width 10px", "resize shrink width 10px"),
            ("Return", "Enter mode default", "mode \"default\""),
            ("Super+R", "Enter mode resize", "mode \"resize\""),
            ("Super+1", "Go to workspace 1", "workspace number 1"),
            ("Super+Shift+1", "Move window to workspace 1", "move container to workspace number 1"),
            ("XF86AudioMute", "Run mute-toggle", "exec mute-toggle"),
            ("Super+code:38", "Run foo", "exec foo"),
            ("Super+Shift+C", "Reload configuration", "reload"),
        ]
    );
    assert_eq!(b.groups, ["Default", "Focus", "resize"]);
    let groups: Vec<usize> = b.binds.iter().map(|x| x.group).collect();
    assert_eq!(
        groups,
        [0, 0, 0, 1, 1, 2, 2, 1, 0, 0, 0, 0, 0],
        "the mode's binds in its group; after `}}` the group before; a new file Default"
    );
    let from: Vec<usize> = b.binds.iter().map(|x| x.file).collect();
    assert_eq!(from, [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 2]);
    assert_eq!(b.not_understood, 0, "the bar's bindsym and the other keys are skipped silently");
    assert_eq!(b.cut, Cut::default());

    // A variable's value expands the variables before it; an undefined one stays.
    let b = sway("set $a foo\nset $b $a-bar\nbindsym Mod4+x exec $b $nope\n");
    assert_eq!(b.binds[0].command, "exec foo-bar $nope");
    // `set` variables are bounded as Mango's `env=` are.
    // (Names ending in `z`: no defined name is the start of another, the longest-first rule aside.)
    let mut text: String = (0..70).map(|i| format!("set $v{i}z x{i}\n")).collect();
    text.push_str("bindsym Mod4+x exec $v0z $v63z $v64z\n");
    let b = sway(&text);
    assert_eq!((b.binds[0].command.as_str(), b.cut.vars), ("exec x0 x63 $v64z", 70 - MAX_VARS));
}

#[test]
fn sway_flags_are_skipped() {
    let b = sway(
        "set $mod Mod4\nbindsym --release --locked $mod+x exec foo\nbindsym --to-code --input-device=1:2:kbd --no-repeat Control+y kill\n",
    );
    assert_eq!(
        (b.binds[0].combo.as_str(), b.binds[0].command.as_str(), b.binds[0].description.as_str()),
        ("Super+X", "exec foo", "Run foo")
    );
    assert_eq!((b.binds[1].combo.as_str(), b.binds[1].command.as_str()), ("Ctrl+Y", "kill"));
    // A bind short of a combo or a command is counted.
    let b = sway("bindsym\nbindsym --locked\nbindsym Mod4+x\nbindsym Mod4+y kill\n");
    assert_eq!((b.binds.len(), b.not_understood), (1, 3));
}

#[test]
fn bindcode_keeps_its_code() {
    let b = sway("set $mod Mod4\nbindcode $mod+38 exec foo\nbindcode --release 9 kill\nbindcode Mod1+Shift+24 floating toggle\n");
    let combos: Vec<&str> = b.binds.iter().map(|x| x.combo.as_str()).collect();
    assert_eq!(combos, ["Super+code:38", "code:9", "Alt+Shift+code:24"]);
}

#[test]
fn sway_blocks_continuations_and_bar_binds() {
    let b = sway(
        "bindsym --locked {\n  Mod4+a exec a\n  Mod4+b exec b\n}\n\
         input * {\n  xkb_layout us\n  bindsym Mod4+no exec no\n}\n\
         bar {\n  colors {\n    background #000000\n  }\n  bindsym button4 workspace next\n}\n\
         mode \"x\" {\n  bindsym {\n    a exec in-x\n  }\n  bindsym b exec also-x\n}\n\
         bindsym Mod4+c exec one \\\n    two\n",
    );
    let got: Vec<(&str, &str)> = b.binds.iter().map(|x| (x.combo.as_str(), x.command.as_str())).collect();
    assert_eq!(got, [("Super+A", "exec a"), ("Super+B", "exec b"), ("A", "exec in-x"), ("B", "exec also-x"), ("Super+C", "exec one two")]);
    let groups: Vec<&str> = b.binds.iter().map(|x| b.groups[x.group].as_str()).collect();
    assert_eq!(groups, ["Default", "Default", "x", "x", "Default"], "a bindsym block inside a mode does not close it");
    assert_eq!(b.not_understood, 0);

    // A stray `}` and deep nesting stay bounded and harmless.
    let deep = format!("}}\n{}{}bindsym Mod4+z kill\n", "foo {\n".repeat(5000), "}\n".repeat(5000));
    let b = sway(&deep);
    assert_eq!(b.binds.len(), 1);
}

#[test]
fn sway_include_glob_and_cycle_are_bounded() {
    // `include config.d/*` reads the folder sorted, skipping hidden files; `*.conf` filters; a
    // self-include (and the root named again by a glob) is read once.
    let files = FakeFiles::with(&[
        ("/c/config", "include config.d/*\ninclude ./config\ninclude /c/./config\ninclude more/*.conf\ninclude ./*\nbindsym Mod4+r kill\n"),
        ("/c/config.d/b", "bindsym Mod4+b kill\n"),
        ("/c/config.d/a", "bindsym Mod4+a kill\n"),
        ("/c/config.d/.hidden", "bindsym Mod4+h kill\n"),
        ("/c/more/x.conf", "bindsym Mod4+x kill\n"),
        ("/c/more/y.txt", "bindsym Mod4+y kill\n"),
    ]);
    let b = load(Dialect::Sway, &first_of("/c/config"), &files);
    assert_eq!(b.files, ["config", "a", "b", "x.conf"]);
    assert_eq!(files.reads("/c/config"), 1);
    assert_eq!((files.reads("/c/config.d/.hidden"), files.reads("/c/more/y.txt")), (0, 0));
    assert_eq!(b.cut, Cut::default());

    // A glob of twelve files: eight files read in all, the rest counted.
    let texts: Vec<(String, String)> = (10..22).map(|i| (format!("/c/d/{i}.conf"), format!("bindsym Mod4+{i} kill\n"))).collect();
    let mut pairs: Vec<(&str, &str)> = texts.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    pairs.push(("/c/config", "include d/*\n"));
    let files = FakeFiles::with(&pairs);
    let b = load(Dialect::Sway, &first_of("/c/config"), &files);
    assert_eq!((files.total_reads(), b.files.len(), b.cut.files), (MAX_FILES, MAX_FILES, 12 + 1 - MAX_FILES));
    assert_eq!(b.files[1], "10.conf", "the first by name");

    // A `*` before the last component is not followed.
    let files = FakeFiles::with(&[("/c/config", "include */x.conf\n"), ("/c/a/x.conf", "bindsym Mod4+x kill\n")]);
    let b = load(Dialect::Sway, &first_of("/c/config"), &files);
    assert_eq!((files.reads("/c/a/x.conf"), b.not_understood), (0, 1));

    // A variable in an include expands; `~` is left to the glue.
    let files = FakeFiles::with(&[("/c/config", "set $d /c/inc\ninclude $d/one\ninclude ~/two\n"), ("/c/inc/one", "")]);
    load(Dialect::Sway, &first_of("/c/config"), &files);
    assert_eq!((files.reads("/c/inc/one"), files.reads("~/two")), (1, 1));
}

#[test]
fn the_etc_fallback_is_used_only_without_a_user_config() {
    let r = roots(Dialect::Sway, vars(&[("HOME", "/h")]), &[]);
    assert_eq!(r, Roots::FirstOf(vec![PathBuf::from("/h/.config/sway/config"), PathBuf::from("/etc/sway/config")]));
    assert_eq!(
        roots(Dialect::Sway, vars(&[("XDG_CONFIG_HOME", "/x"), ("HOME", "/h")]), &[]),
        Roots::FirstOf(vec![PathBuf::from("/x/sway/config"), PathBuf::from("/etc/sway/config")])
    );
    assert_eq!(roots(Dialect::Sway, vars(&[]), &[]), Roots::FirstOf(vec![PathBuf::from("/etc/sway/config")]));
    assert_eq!(roots(Dialect::Sway, vars(&[("HOME", "/h")]), &["/k".to_string()]), Roots::Each(vec![PathBuf::from("/k")]));
    assert_eq!(roots(Dialect::Mango, vars(&[]), &[]), Roots::FirstOf(Vec::new()), "Mango has no system fallback");

    let etc = [
        ("/etc/sway/config", "include /etc/sway/config.d/*\nbindsym Mod4+e kill\n"),
        ("/etc/sway/config.d/nixos.conf", "bindsym Mod4+n kill\n"),
    ];
    let mut both = etc.to_vec();
    both.push(("/h/.config/sway/config", "bindsym Mod4+u kill\n"));
    let files = FakeFiles::with(&both);
    let b = load(Dialect::Sway, &r, &files);
    assert_eq!(b.files, ["config"]);
    assert_eq!(b.binds[0].combo, "Super+U");
    assert_eq!(files.reads("/etc/sway/config"), 0, "the user's config found: /etc never read");

    let files = FakeFiles::with(&etc);
    let b = load(Dialect::Sway, &r, &files);
    assert!(b.found);
    assert_eq!(b.files, ["config", "nixos.conf"]);
    let combos: Vec<&str> = b.binds.iter().map(|x| x.combo.as_str()).collect();
    assert_eq!(combos, ["Super+E", "Super+N"]);
}

#[test]
fn sway_humanised_table() {
    let s = Dialect::Sway;
    for (function, args, want) in [
        ("exec", "foo --bar", "Run foo --bar"),
        ("exec_always", "foo", "Run foo"),
        ("exec", "--no-startup-id foo", "Run foo"),
        ("workspace", "number 3", "Go to workspace 3"),
        ("workspace", "3", "Go to workspace 3"),
        ("workspace", "next", "Go to workspace next"),
        ("move", "container to workspace number 3", "Move window to workspace 3"),
        ("move", "window to workspace 3", "Move window to workspace 3"),
        ("move", "to workspace number 4", "Move window to workspace 4"),
        ("move", "left", "Move window left"),
        ("move", "scratchpad", "Move window to the scratchpad"),
        ("scratchpad", "show", "Show the scratchpad"),
        ("kill", "", "Close window"),
        ("focus", "left", "Focus left"),
        ("fullscreen", "toggle", "Toggle fullscreen"),
        ("fullscreen", "", "Toggle fullscreen"),
        ("floating", "toggle", "Toggle floating"),
        ("sticky", "toggle", "Toggle sticky"),
        ("layout", "tabbed", "Set layout tabbed"),
        ("splith", "", "Split horizontally"),
        ("splitv", "", "Split vertically"),
        ("resize", "grow width 10px", "Resize grow width 10px"),
        ("reload", "", "Reload configuration"),
        ("exit", "", "Quit Sway"),
        ("mode", "\"resize\"", "Enter mode resize"),
        ("mode", "resize", "Enter mode resize"),
        ("fancy", "x y", "fancy x y"),
    ] {
        assert_eq!(describe::humanise(s, function, args), want, "{function} {args}");
    }
}

#[test]
fn include_globs_stop_listing_once_the_file_slots_are_full() {
    // Twenty `include dN/*`, one file each: the root takes a slot, seven globs fill the rest; the
    // thirteen after are counted, never listed.
    let texts: Vec<(String, String)> = (0..20).map(|i| (format!("/c/d{i:02}/x"), format!("bindsym Mod4+F{i} kill\n"))).collect();
    let mut pairs: Vec<(&str, &str)> = texts.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let root: String = (0..20).map(|i| format!("include d{i:02}/*\n")).collect();
    pairs.push(("/c/config", &root));
    let files = FakeFiles::with(&pairs);
    let b = load(Dialect::Sway, &first_of("/c/config"), &files);
    assert_eq!(files.lists.get(), MAX_FILES - 1, "no folder listed once the slots are full");
    assert_eq!((b.files.len(), b.cut.files), (MAX_FILES, 20 + 1 - MAX_FILES));
}

#[test]
fn a_continued_line_past_the_bound_drops_its_rest_counted_once() {
    let long = "x".repeat(MAX_LINE - 20);
    let b = sway(&format!("bindsym Mod4+a exec {long} \\\n  {long} \\\n  tail\nbindsym Mod4+b kill\n"));
    assert_eq!(b.binds.len(), 2);
    assert!(!b.binds[0].command.contains("tail"), "the pieces after the bound are dropped");
    assert!(b.binds[0].command.len() <= MAX_LINE);
    assert_eq!(b.cut.lines, 1, "one joined line cut: counted once");
    assert_eq!(b.binds[1].command, "kill");
}

#[test]
fn sway_variables_expand_longest_name_first() {
    let b = sway("set $mod Mod4\nset $ws-1 1\nbindsym $mod+1 workspace number $ws-1\nbindsym $modx kill\n");
    let got: Vec<(&str, &str)> = b.binds.iter().map(|x| (x.combo.as_str(), x.command.as_str())).collect();
    assert_eq!(got, [("Super+1", "workspace number 1"), ("Mod4x", "kill")]);
    // The longer of two names that both match wins.
    let b = sway("set $m Mod1\nset $mod Mod4\nbindsym $mod+q kill\n");
    assert_eq!(b.binds[0].combo, "Super+Q");
}

#[test]
fn an_include_strips_quotes_and_takes_home_like_a_tilde() {
    let files = FakeFiles::with(&[
        ("/c/config", "include \"/c/q one\"\ninclude '/c/q two'\ninclude $HOME/three\ninclude $NOPE/four\n"),
        ("/c/q one", ""),
        ("/c/q two", ""),
    ]);
    let b = load(Dialect::Sway, &first_of("/c/config"), &files);
    assert_eq!((files.reads("/c/q one"), files.reads("/c/q two")), (1, 1));
    assert_eq!(files.reads("~/three"), 1, "$HOME goes to the glue as ~");
    assert_eq!(files.total_reads(), 4, "an unknown variable is not followed");
    assert_eq!(b.not_understood, 1);
}

#[test]
fn a_mode_past_the_depth_cap_never_changes_the_group() {
    let modes: String = (1..=8).map(|i| format!("mode \"m{i}\" {{\n")).collect();
    let b = sway(&format!("{modes}mode \"z\" {{\n}}\nbindsym a kill\n{}", "}\n".repeat(8)));
    assert_eq!(b.binds.len(), 1);
    assert_eq!(b.groups[b.binds[0].group], "m8");
}
