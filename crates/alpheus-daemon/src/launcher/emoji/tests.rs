use super::table::TABLE;
use super::*;
use std::fmt::Write as _;

/// Skin-tone modifiers: the table keeps the base emoji only (one row, not five more).
fn is_tone(code_point: u32) -> bool {
    (0x1F3FB..=0x1F3FF).contains(&code_point)
}

/// Whether a line's code points (`1F468 200D 1F469`) are kept in the table.
fn keep(code_points: &str) -> bool {
    !code_points.split_whitespace().any(|c| u32::from_str_radix(c, 16).is_ok_and(is_tone))
}

/// A string literal's body: the table's tabs and newlines are written as escapes, and so are
/// the invisible joiner and variation selector (a reader of `table.rs` sees them).
fn escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{200d}' | '\u{fe0f}' => write!(out, "\\u{{{:x}}}", u32::from(c)).unwrap(),
            c => out.push(c),
        }
    }
    out
}

/// Regenerates `table.rs` from Unicode's `emoji-test.txt` (run it by hand, see the table's
/// header): `ALPHEUS_EMOJI_TEST` is the file, `ALPHEUS_EMOJI_SHA256` its checksum, written in
/// the header.
#[test]
#[ignore = "regenerates table.rs from a downloaded emoji-test.txt"]
fn regenerate_the_emoji_table() {
    let path = std::env::var("ALPHEUS_EMOJI_TEST").expect("ALPHEUS_EMOJI_TEST");
    let sha = std::env::var("ALPHEUS_EMOJI_SHA256").expect("ALPHEUS_EMOJI_SHA256");
    let source = std::fs::read_to_string(path).unwrap();
    let version = source.lines().find_map(|l| l.strip_prefix("# Version: ")).expect("a version line").trim();
    // The data's own copyright line (`# © 2024 Unicode®, Inc.`): the Unicode License v3 wants it
    // with every copy; the full notice is the repository's NOTICE file.
    let copyright = source.lines().find_map(|l| l.strip_prefix("# ").filter(|r| r.starts_with('\u{a9}'))).expect("a copyright line").trim();
    let mut lines = String::new();
    for line in source.lines() {
        let Some((fields, comment)) = line.split_once('#') else { continue };
        let Some((code_points, status)) = fields.split_once(';') else { continue };
        if status.trim() != "fully-qualified" || !keep(code_points) {
            continue;
        }
        // The comment reads `<emoji> E<version> <name>`.
        let mut words = comment.trim().splitn(3, ' ');
        let (emoji, _version, name) = (words.next().unwrap(), words.next().unwrap(), words.next().unwrap());
        writeln!(lines, "    \"{}\\t{}\\n\",", escape(emoji), escape(name)).unwrap();
    }
    let out = format!(
        "// Generated from https://www.unicode.org/Public/emoji/{version}/emoji-test.txt\n\
         // (Unicode emoji {version}, sha256 {sha}) by\n\
         // `launcher::emoji::tests::regenerate_the_emoji_table`: fully-qualified lines without a\n\
         // skin-tone modifier, as `<emoji>\\t<name>\\n`. Do not edit by hand.\n\
         //\n\
         // {copyright}\n\
         // The data is distributed under the Unicode License v3, whose copyright and permission\n\
         // notice is in the NOTICE file at the root of this repository\n\
         // (https://www.unicode.org/license.txt).\n\
         \n\
         #[rustfmt::skip]\n\
         pub(super) const TABLE: &str = concat!(\n{lines});\n"
    );
    std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/src/launcher/emoji/table.rs"), out).unwrap();
}

#[test]
fn the_table_is_well_formed() {
    let lines: Vec<&str> = TABLE.lines().collect();
    assert!((1800..=2200).contains(&lines.len()), "{} entries", lines.len());
    assert!(TABLE.ends_with('\n'));
    for line in &lines {
        assert_eq!(line.matches('\t').count(), 1, "{line:?}");
        let (emoji, name) = line.split_once('\t').unwrap();
        assert!(!emoji.is_empty() && !name.is_empty(), "{line:?}");
        assert!(!name.chars().any(char::is_control), "{line:?}");
        assert!(!line.chars().any(|c| is_tone(u32::from(c))), "{line:?}");
    }
    assert_eq!(lines[0], "😀\tgrinning face");
    for wanted in ["❤️\tred heart", "🇫🇷\tflag: France", "👨\u{200d}👩\u{200d}👧\tfamily: man, woman, girl"] {
        assert!(lines.contains(&wanted), "{wanted:?}");
    }
}

#[test]
fn at_reads_line_starts_only() {
    assert_eq!(at(0), Some(("😀", "grinning face")));
    assert_eq!(at(1), None);
    assert_eq!(at(TABLE.len()), None);
    assert_eq!(at(TABLE.len() + 5), None);
    let mut count = 0;
    for (offset, emoji, name) in entries() {
        assert_eq!(at(offset), Some((emoji, name)));
        count += 1;
    }
    assert_eq!(count, TABLE.lines().count());
    // Nowhere else: not inside a line, not even at a character boundary of a joined sequence.
    let starts: Vec<usize> = entries().map(|(offset, _, _)| offset).collect();
    for offset in 0..TABLE.len() {
        assert_eq!(at(offset).is_some(), starts.contains(&offset), "offset {offset}");
    }
}

/// The Unicode License v3 wants its notice with every copy of the data: in the table's header
/// (the copyright line and a pointer) and, in full, in the repository's NOTICE file.
#[test]
fn the_table_keeps_its_licence_notice() {
    let header: String = include_str!("table.rs").lines().take_while(|l| l.starts_with("//")).collect::<Vec<_>>().join("\n");
    for needed in ["\u{a9} 20", "Inc.", "Unicode License v3", "NOTICE"] {
        assert!(header.contains(needed), "{needed:?} missing from table.rs's header:\n{header}");
    }
    let notice = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../NOTICE")).unwrap();
    for needed in [
        "UNICODE LICENSE V3",
        "COPYRIGHT AND PERMISSION NOTICE",
        "Permission is hereby granted, free of charge",
        "THE DATA FILES AND SOFTWARE ARE PROVIDED \"AS IS\"",
    ] {
        assert!(notice.contains(needed), "{needed:?} missing from NOTICE");
    }
}
