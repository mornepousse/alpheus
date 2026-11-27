//! Mango's config lines: `bind`, `bindl`, `bindsl`, `bindr` (`MODS,key,function[,args]`) become
//! binds; `source=` names a file to read next (relative to the including file); `env=NAME,value`
//! defines a variable later binds expand; `# ── Name ──` opens a group. `mousebind`, `axisbind`,
//! `gesturebind` and every other key are not keyboard binds and are skipped silently.

use super::describe::{self, rule_name};
use super::{Dialect, Parse};
use std::path::{Path, PathBuf};

/// Parses one file's `text` (its folder is `here`) into `st`; gives the `source=` paths, in order.
pub(crate) fn parse(text: &str, here: &Path, st: &mut Parse) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    for raw in text.lines() {
        let line = st.cut_line(raw).trim();
        if line.is_empty() {
            st.above = None;
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            let comment = comment.trim_start_matches('#').trim();
            match rule_name(comment) {
                Some(Some(name)) => {
                    st.open_group(name.to_string());
                    st.above = None;
                }
                Some(None) => st.above = None,
                None => st.above = Some(comment.to_string()).filter(|c| !c.is_empty()),
            }
            continue;
        }
        let above = st.above.take();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (value, trailing) = split_comment(value.trim());
        match key.trim() {
            // `~/…` is the home folder, which the glue knows (`RealFiles`): kept as written.
            "source" if value == "~" || value.starts_with("~/") => sources.push(PathBuf::from(value)),
            "source" if !value.is_empty() => sources.push(here.join(value)),
            "env" => {
                if let Some((name, val)) = value.split_once(',') {
                    st.define(name.trim(), val.trim());
                }
            }
            k if is_bind(k) => bind(value, trailing, above, st),
            _ => {}
        }
    }
    sources
}

/// `bind` followed by its flag letters (`l` locked, `s` keysym, `r` release, `p` pass).
fn is_bind(key: &str) -> bool {
    key.strip_prefix("bind").is_some_and(|flags| flags.chars().all(|c| matches!(c, 'l' | 's' | 'r' | 'p')))
}

/// A value and its trailing `# comment` (a `#` at the start or after a blank), both trimmed.
pub(super) fn split_comment(value: &str) -> (&str, Option<&str>) {
    let at = value.char_indices().find(|&(i, c)| c == '#' && (i == 0 || value.get(..i).is_some_and(|v| v.ends_with(char::is_whitespace))));
    match at {
        Some((i, _)) => {
            let comment = value.get(i + 1..).unwrap_or_default().trim();
            (value.get(..i).unwrap_or_default().trim(), Some(comment).filter(|c| !c.is_empty()))
        }
        None => (value, None),
    }
}

/// One bind's value, `MODS,key,function[,args]`; a value short of a key or a function is counted
/// as not understood.
fn bind(value: &str, trailing: Option<&str>, above: Option<String>, st: &mut Parse) {
    let mut parts = value.splitn(4, ',').map(str::trim);
    let (mods, key, function) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    if key.is_empty() || function.is_empty() {
        st.out.not_understood += 1;
        return;
    }
    let args = st.expand(parts.next().unwrap_or_default());
    let description = match (trailing, above) {
        (Some(t), _) => t.to_string(),
        (None, Some(a)) => a,
        (None, None) => describe::humanise(Dialect::Mango, function, &args),
    };
    let command = if args.is_empty() { function.to_string() } else { format!("{function},{args}") };
    st.push(describe::combo(Dialect::Mango, mods, key), description, command);
}
