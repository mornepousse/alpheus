//! Tests of the binds parser: Mango's chain of `source=` files, groups, descriptions, `env=`
//! variables, combos, and every bound. Files come from a `FakeFiles` map; no real file is read.

use super::*;
use std::cell::RefCell;
use std::collections::HashMap;

/// A map of path → text standing for the disk, counting the reads of each path.
#[derive(Default)]
struct FakeFiles {
    texts: HashMap<PathBuf, String>,
    reads: RefCell<HashMap<PathBuf, usize>>,
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
fn sway_parses_nothing_yet_but_reads_its_root() {
    let files = FakeFiles::with(&[("/c/config", "bindsym $mod+a exec x\n")]);
    let b = load(Dialect::Sway, &first_of("/c/config"), &files);
    assert!(b.found);
    assert!(b.binds.is_empty());
    assert!(!load(Dialect::Sway, &first_of("/c/none"), &files).found);
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
