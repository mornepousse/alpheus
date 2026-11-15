//! The emoji table behind the `:` prefix (launcher 2 spec): one `&'static str` of
//! `<emoji>\t<name>\n` lines generated once from Unicode's `emoji-test.txt` (see `table.rs`),
//! so it costs one read-only section and no allocation. A row of the list is the byte offset
//! of its line. Pure: no thread, no command, nothing can panic.

mod table;

use table::TABLE;

/// `(emoji, name)` of one line (without its newline), `None` for anything but `<emoji>\t<name>`.
fn parse(line: &'static str) -> Option<(&'static str, &'static str)> {
    let (emoji, name) = line.split_once('\t')?;
    (!emoji.is_empty() && !name.is_empty()).then_some((emoji, name))
}

/// Every line as (byte offset of the line, emoji, name), in the table's order.
pub fn entries() -> impl Iterator<Item = (usize, &'static str, &'static str)> {
    let mut offset = 0;
    TABLE.split_inclusive('\n').filter_map(move |line| {
        let start = offset;
        offset += line.len();
        parse(line.trim_end_matches('\n')).map(|(emoji, name)| (start, emoji, name))
    })
}

/// The line starting at `offset`; `None` unless `offset` starts one.
pub fn at(offset: usize) -> Option<(&'static str, &'static str)> {
    if offset > 0 && TABLE.as_bytes().get(offset - 1) != Some(&b'\n') {
        return None;
    }
    parse(TABLE.get(offset..)?.split('\n').next()?)
}

#[cfg(test)]
mod tests;
